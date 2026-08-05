use alloc::boxed::Box;
// use alloc::collections::VecDeque;
use alloc::vec::Vec;
use bao1x_hal::rram::Reram;
use core::convert::TryFrom;
use digest::Digest;
use sha2_bao1x::Sha256;

/// Represents a contiguous block of memory to overwrite or read back.
trait MemRegion {
    /// Start address.
    fn start(&self) -> u32;
    /// End address.
    fn end(&self) -> u32;
    /// Total size of the region in bytes.
    fn len(&self) -> usize {
        (self.end() - self.start()) as usize
    }
    /// Granularity of writes to the region. The start address should always remain aligned.
    fn min_update_size(&self) -> usize;
    /// Write a slice to the start of the region. Returns a BadAlignment error if the update is not
    /// a multiple of the minimum update size.
    fn write_slice(&self, data: &[u8]) -> Result<(), xous::Error>;
    /// Returns a slice representing a prefix of the region.
    fn read_slice(&self, len: usize) -> Result<&[u8], xous::Error>;
    /// Clip off a portion of the start of the region.
    fn advance(&mut self, nbytes: usize) -> Result<(), xous::Error>;
}

struct ReramRegion {
    start: u32,
    end: u32,
}

impl ReramRegion {
    // RRAM minimum update granularity is 32 bytes.
    const MIN_UPDATE_BYTES: usize = 32;
 
    fn new(baremetal_image_text_reserved: usize) -> Self {
        // Get the writeable section of RRAM that is past boot1 and the specified range reserved for
        // the baremetal image.
        let mut start = bao1x_api::BAREMETAL_START + baremetal_image_text_reserved;
        // TODO: uncomment
        // let end = utralib::HW_RERAM_MEM + bao1x_api::RRAM_STORAGE_LEN;
        let end = start + 128;
        // Align the start to the update granularity.
        if start % Self::MIN_UPDATE_BYTES != 0 {
            start += Self::MIN_UPDATE_BYTES - start % Self::MIN_UPDATE_BYTES;
        }
        if end < start {
            panic!("Calculated a negative-size RRAM! ({:x}-{:x})", start, end);
        }
        ReramRegion {
            start: start as u32,
            end: end as u32,
        }
    }
}

impl MemRegion for ReramRegion {
    fn start(&self) -> u32 {
        self.start
    }
    
    fn end(&self) -> u32 {
        self.end
    }
    
    fn min_update_size(&self) -> usize {
        Self::MIN_UPDATE_BYTES
    }

    fn write_slice(&self, data: &[u8]) -> Result<(), xous::Error> {
        if data.len() % Self::MIN_UPDATE_BYTES != 0 {
            return Err(xous::Error::BadAlignment);
        }
        if data.len() > self.len() {
            return Err(xous::Error::MemoryInUse);
        }
        let offset = self.start as usize - utralib::HW_RERAM_MEM;
        let mut rram = Reram::new();
        let len = rram.write_slice(offset, data)?;
        if len != data.len() {
            return Err(xous::Error::InternalError);
        }
        Ok(())
    }

    fn read_slice(&self, len: usize) -> Result<&[u8], xous::Error> {
        if self.len() < len {
            return Err(xous::Error::Unavailable)
        }

        // safety: this is safe if the caller has ensured that the RRAM region is in fact readable.
        let bytes = unsafe { core::slice::from_raw_parts(
            self.start as *const u8,
            len) };
        Ok(bytes)
    }

    fn advance(&mut self, nbytes: usize) -> Result<(), xous::Error> {
        if nbytes % 32 != 0 {
            Err(xous::Error::BadAlignment)
        } else if self.len() < nbytes {
            Err(xous::Error::Unavailable)
        } else {
            self.start += nbytes as u32;
            Ok(())
        }
    }

}

/// Traverses through multiple non-contiguous memory blocks.
struct MemoryTraversal {
    idx: usize,
    pending: Vec<u8>,
    blocks: [Box<dyn MemRegion>;1],
    // TODO: investigate/add mem regions from utralib/src/generated/bao1x.rs
    // TODO: maybe add a block of constant ciphertext to the program to start so it fills all of the
    // boot1 region?
}

impl MemoryTraversal {
    fn new() -> Self {
        // TODO: get a tighter bound here.
        let baremetal_image_reserved = 100000;
        let blocks: [Box<dyn MemRegion>;1] = [
            Box::new(ReramRegion::new(baremetal_image_reserved)),
        ];
        let max_min_update = blocks.iter()
            .max_by_key(|b| b.min_update_size())
            .expect("Blocks should be nonempty")
            .min_update_size();
        MemoryTraversal {
            idx: 0,
            pending: Vec::with_capacity(max_min_update),
            blocks: blocks,
        }
    }

    fn len(&self) -> usize {
        let mut total = 0;
        for i in self.idx..self.blocks.len() {
            total += self.blocks[i].len();
        }
        return total;
    }

    fn peek(&self) -> u32 {
        self.blocks[self.idx].start()
    }

    fn advance_block(&mut self) -> Result<(), xous::Error> {
        if self.idx < self.blocks.len() - 1 {
            self.idx += 1;
            Ok(())
        } else {
            Err(xous::Error::OutOfMemory)
        }
    }

    fn write_slice(&mut self, data: &[u8]) -> Result<(), xous::Error> {
        let min_size = self.blocks[self.idx].min_update_size();

        // If we don't have enough data for a write, update pending and exit.
        if self.pending.len() + data.len() < min_size {
            self.pending.extend_from_slice(data);
            return Ok(());
        }

        // If there's not enough space in the block, skip to the next one and retry.
        if self.blocks[self.idx].len() == 0 {
            self.advance_block()?;
            return self.write_slice(data);
        } else if self.blocks[self.idx].len() < min_size {
            // If this happens, we might get mismatches between write and read patterns. However,
            // since we expect the minimum size to be a multiple of the memory size, we don't expect
            // it to ever happen. If that assumption is violated, panic.
            panic!("Block size is not a multiple of minimum update size!");
        }

        // Fill, write, and clear the pending vector if present.
        let mut rem_data = data;
        if self.pending.len() != 0 {
            let (head, tail) = data.split_at(min_size - self.pending.len());
            self.pending.extend_from_slice(head);
            self.blocks[self.idx].write_slice(self.pending.as_slice())?;
            self.blocks[self.idx].advance(self.pending.len())?;
            self.pending.clear();
            rem_data = tail;
        }

        // Find the greatest multiple of the min update size that fits in both data and the
        // remainder of the current block.
        let cutoff = self.blocks[self.idx].len().min(rem_data.len());
        let cutoff_aligned = cutoff - (cutoff % min_size);
        let (head, tail) = rem_data.split_at(cutoff_aligned);
        self.blocks[self.idx].write_slice(head)?;
        self.blocks[self.idx].advance(head.len())?;
        return self.write_slice(tail);
    }

    /// Returns a contiguous slice of data. If possible, the slice has the requested length; it may
    /// be shorter if that much contiguous data is not available.
    fn read_slice(&mut self, len: usize) -> Result<&[u8], xous::Error> {
        if self.blocks[self.idx].len() == 0 {
            self.advance_block()?;
            self.read_slice(len)
        } else {
            let max_len = self.blocks[self.idx].len();
            self.blocks[self.idx].read_slice(len.min(max_len))
        }
    }
}

/// Iteratively performs ShiftXOR function as described in the SUANT paper.
struct ShiftXor {
    seed: [u8; 16],
    key_block: [u8; 16],
    // TODO: change to VecDeque to avoid copies
    pending: Vec<u8>,
    counter: u32,
}

impl ShiftXor {
    /// The chunk size is determined by the key size, which also must match the key size. We are
    /// assuming AES-128 here; for AES-256 this and the key/seed sizes would need to be doubled.
    // TODO: try making this a parameter <N>
    const CHUNK_BYTES: usize = 16;

    /// Derived size of shift parameter. Unlike in the SUANT paper, we round up to the next byte
    /// boundary when pulling bytes from the extraction function to avoid shifting bits within
    /// bytes.
    const SHIFT_BITS: usize = 7;
    // TODO: this definition gives an error because bit_width() is not stable
    // const SHIFT_BITS: usize = (Self::CHUNK_BYTES * 8).bit_width() as usize;
    const SHIFT_BYTES: usize = (Self::SHIFT_BITS + 7) / 8;

    fn new(seed: &[u8], key_block: &[u8]) -> Self {
        ShiftXor {
            seed: <[u8;16]>::try_from(seed).expect("Invalid seed length!"),
            key_block: <[u8;16]>::try_from(key_block).expect("Invalid key length!"),
            pending: Vec::with_capacity(32), // size of hash output
            counter: 0,
        }
    }

    fn get_shift(&mut self) -> usize {
        if self.pending.len() >= Self::SHIFT_BYTES {
            // Decode shift from the prefix pending bytes (little-endian).
            let mut shift: u32 = 0;
            for &b in self.pending.iter().rev() {
                shift <<= 8;
                shift |= b as u32;
            }
            let tail = self.pending.split_off(Self::SHIFT_BYTES);
            self.pending = tail;
            shift as usize
        } else {
            // Load more bytes and then try again.
            let mut h = Sha256::new();
            h.update(self.seed);
            h.update(self.counter.to_le_bytes());
            self.counter += 1;
            self.pending.extend_from_slice(&h.finalize());
            self.get_shift()
        }
    }

    fn absorb(&mut self, ciphertext: &[u8]) -> Result<(), xous::Error> {
        if ciphertext.len() != Self::CHUNK_BYTES {
            return Err(xous::Error::BadAlignment);
        }

        // XOR the key block with a cyclic shift of the ciphertext.
        let shift = self.get_shift();
        for i in 0..ciphertext.len() {
            let ct_lower_idx = ((shift / 8) + i) % ciphertext.len();
            let ct_upper_idx = ((shift / 8) + i + 1) % ciphertext.len();
            let ct_lower = ciphertext[ct_lower_idx] >> (shift % 8);
            let ct_upper = ciphertext[ct_upper_idx] & ((1 << (shift % 8)) - 1);
            let ct = if shift % 8 == 0 { ct_lower } else { ct_lower | (ct_upper << (8 - (shift % 8))) };
            self.key_block[i] ^= ct;
        }
        Ok(())
    }

    fn key(&self) -> &[u8] {
        &self.key_block
    }
}


pub struct Erasure {
    traversal: MemoryTraversal,
    bytes_written: usize,
}

impl Erasure {
    pub fn new() -> Self {
        Erasure {
            traversal: MemoryTraversal::new(),
            bytes_written: 0,
        }
    }

    /// Remaining length to fill.
    pub fn len(&self) -> usize {
        self.traversal.len()
    }

    /// Next address to fill.
    pub fn peek(&self) -> u32 {
        self.traversal.peek()
    }

    pub fn write_slice(&mut self, data: &[u8]) {
        self.traversal.write_slice(data).unwrap();
        self.bytes_written += data.len();
    }

    /// Recover the key from the ciphertext, shift seed, and key block.
    pub fn recover_key(&self, shift_seed: &[u8], key_block: &[u8]) -> Result<[u8;16], xous::Error> {
        let mut shifter = ShiftXor::new(shift_seed, key_block);
        let mut reader = MemoryTraversal::new();
        let nchunks = self.bytes_written.div_ceil(ShiftXor::CHUNK_BYTES);
        for _ in 0..nchunks {
            let chunk = reader.read_slice(ShiftXor::CHUNK_BYTES)?;
            if chunk.len() == ShiftXor::CHUNK_BYTES {
                shifter.absorb(chunk)?;
            } else if chunk.len() < ShiftXor::CHUNK_BYTES {
                let mut v = Vec::new();
                v.extend_from_slice(chunk);
                while v.len() < ShiftXor::CHUNK_BYTES {
                    let next_chunk = reader
                        .read_slice(ShiftXor::CHUNK_BYTES - v.len())?;
                    v.extend_from_slice(next_chunk);
                }
                shifter.absorb(v.as_slice())?;
            } else {
                // This shouldn't happen!
                return Err(xous::Error::InternalError);
            }
        }

        // Interpret the key as an array.
        <[u8;16]>::try_from(shifter.key())
            .map_err(|_| xous::Error::InternalError)
    }

}
