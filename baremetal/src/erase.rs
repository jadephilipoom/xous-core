use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use bao1x_hal::rram::Reram;
use core::convert::TryFrom;

mod shiftxor;
use crate::erase::shiftxor::ShiftXor;

use crate::SerialInteract;

/// Represents a contiguous block of memory to overwrite or read back.
trait MemRegion {
    /// Start address.
    fn start(&self) -> u32;
    /// End address.
    fn end(&self) -> u32;
    /// Granularity of writes to the region. The start address should always remain aligned.
    fn min_update_size(&self) -> usize;
    /// Write a slice to the start of the region. Returns a BadAlignment error if the update is not
    /// a multiple of the minimum update size.
    fn write_slice(&self, data: &[u8]) -> Result<(), xous::Error>;
    /// Clip off a portion of the start of the region.
    fn advance(&mut self, nbytes: usize) -> Result<(), xous::Error>;

    /// Total size of the region in bytes.
    fn len(&self) -> usize {
        (self.end() - self.start()) as usize
    }
    /// Returns a slice representing the region. The caller must ensure no one else owns this region
    /// for the duration of the slice's lifetime.
    unsafe fn as_slice(&self) -> &[u8] {
        core::slice::from_raw_parts(
            self.start() as *const u8,
            self.len())
    }
}

struct GenericMemRegion {
    start: u32,
    end: u32,
}

impl GenericMemRegion {
    // Typical minimum update granularity for memory regions is 4 bytes.
    const MIN_UPDATE_BYTES: usize = 4;
 
    fn new(start: usize, len: usize) -> Self {
        // Align the start to the update granularity.
        if start % Self::MIN_UPDATE_BYTES != 0 || len % Self::MIN_UPDATE_BYTES != 0 {
            panic!("Memory region ({:x}-{:x}) is not aligned to {:?} bytes", start, start+len, Self::MIN_UPDATE_BYTES);
        }
        GenericMemRegion {
            start: start as u32,
            end: (start+len) as u32,
        }
    }
}

impl MemRegion for GenericMemRegion {
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
        // Safety: we need to ensure nothing else takes ownership of this memory during the erasure
        // process.
        let dst = unsafe {
            core::slice::from_raw_parts_mut(
                self.start as *mut u8,
                data.len())
        };
        dst.copy_from_slice(data);

        // Read back to check that the write worked.
        bao1x_hal::cache_flush();
        if dst != data {
            return Err(xous::Error::AccessDenied);
        }
        Ok(())
    }

    fn advance(&mut self, nbytes: usize) -> Result<(), xous::Error> {
        if self.len() < nbytes {
            Err(xous::Error::Unavailable)
        } else {
            self.start += nbytes as u32;
            Ok(())
        }
     }
}

struct ReramRegion {
    start: u32,
    end: u32,
}

impl ReramRegion {
    // RRAM minimum update granularity is 32 bytes.
    const MIN_UPDATE_BYTES: usize = 32;
 
    fn new(baremetal_rram_offset: u32) -> Result<Self,xous::Error> {
        // Get the writeable section of RRAM that is past boot1 and the specified range reserved for
        // the baremetal image. The baremetal code actually starts at an offset of
        // 1024 from BAREMETAL_START.
        let mut start = bao1x_api::BAREMETAL_START + 1024 + baremetal_rram_offset as usize;
        let end = utralib::HW_RERAM_MEM + bao1x_api::RRAM_STORAGE_LEN;
        if start % Self::MIN_UPDATE_BYTES != 0 {
            // If the start address is not a multiple of the minimum update granularity, we need to
            // write some zeroes as padding.
            let offset = start as usize - utralib::HW_RERAM_MEM;
            let nbytes = Self::MIN_UPDATE_BYTES - offset % Self::MIN_UPDATE_BYTES;
            start += nbytes;
            let data = vec![0u8;nbytes];
            let mut rram = Reram::new();
            let len = rram.write_slice(offset, data.as_slice())?;
            if len != data.len() {
                return Err(xous::Error::InternalError);
            }
        }
        if end < start {
            return Err(xous::Error::ParseError);
        }
        Ok(ReramRegion {
            start: start as u32,
            end: end as u32,
        })
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
        // TODO: this might not be necessary if we use write_slice, for write_u32_aligned it's more
        // complicated
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

    fn advance(&mut self, nbytes: usize) -> Result<(), xous::Error> {
        if self.len() < nbytes {
            Err(xous::Error::Unavailable)
        } else {
            self.start += nbytes as u32;
            Ok(())
        }
     }


    unsafe fn as_slice(&self) -> &[u8] {
        core::slice::from_raw_parts(
            self.start as *const u8,
            self.len())
    }
}

/// Traverses through multiple non-contiguous memory blocks.
struct MemoryTraversal {
    idx: usize,
    pending: Vec<u8>,
    blocks: Vec<Box<dyn MemRegion>>,
    // TODO: investigate/add mem regions from utralib/src/generated/bao1x.rs
}

macro_rules! mem {
    ( $start: ident, $len: ident ) => {
        GenericMemRegion::new(utralib::generated::$start, utralib::generated::$len)
    };
}

impl MemoryTraversal {
    fn new(baremetal_rram_offset: u32) -> Result<Self,xous::Error> {
        let mut blocks: Vec<Box<dyn MemRegion>> = Vec::new();
        blocks.push(Box::new(ReramRegion::new(baremetal_rram_offset)?));
        blocks.push(Box::new(mem!(HW_BIO_IMEM0_MEM, HW_BIO_IMEM0_MEM_LEN)));
        blocks.push(Box::new(mem!(HW_BIO_IMEM1_MEM, HW_BIO_IMEM1_MEM_LEN)));
        blocks.push(Box::new(mem!(HW_BIO_IMEM2_MEM, HW_BIO_IMEM2_MEM_LEN)));
        blocks.push(Box::new(mem!(HW_BIO_IMEM3_MEM, HW_BIO_IMEM3_MEM_LEN)));
        // TODO: IFRAM0 works but might overwrite some USB stuff, needs further checking. IFRAM1
        // seems to block.
        // blocks.push(Box::new(mem!(HW_IFRAM0_MEM, HW_IFRAM0_MEM_LEN)));
        // blocks.push(Box::new(mem!(HW_IFRAM1_MEM, HW_IFRAM1_MEM_LEN)));
        let max_min_update = blocks.iter()
            .max_by_key(|b| b.min_update_size())
            .expect("Blocks should be nonempty")
            .min_update_size();
        Ok(MemoryTraversal {
            idx: 0,
            pending: Vec::with_capacity(max_min_update),
            blocks: blocks,
        })
    }

    /// Creates an empty erasure representing no memory.
    pub fn empty() -> Self {
        MemoryTraversal {
            idx: 0,
            pending: Vec::new(),
            blocks: Vec::new(),
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
            // it to ever happen. If that assumption is violated, return an error.
            return Err(xous::Error::BadAlignment);
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

    fn all_blocks(&self) -> &[Box<dyn MemRegion>] {
        return &self.blocks;
    }
}

pub struct Erasure {
    traversal: MemoryTraversal,
    bytes_written: usize,
}

impl Erasure {
    // Determines the chunk size for ShiftXor.
    const KEY_BYTES: usize = 16;

    pub fn new(baremetal_rram_offset: u32) -> Result<Self,xous::Error> {
        Ok(Erasure {
            traversal: MemoryTraversal::new(baremetal_rram_offset)?,
            bytes_written: 0,
        })
    }

    /// Creates an empty erasure representing no memory.
    pub fn empty() -> Self {
        Erasure {
            traversal: MemoryTraversal::empty(),
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
        let mut shifter = ShiftXor::<{ Self::KEY_BYTES }>::new(shift_seed, key_block);
        // Start a traversal that *includes* the baremetal rram code.
        let reader = MemoryTraversal::new(0)?;
        for block in reader.all_blocks() {
            // safety: we need to ensure no one else takes ownership of this memory while we're
            // reading it. The program is single-threaded and memory is only allocated from SRAM.
            // TODO: when writing SRAM, make sure we can block off only a small section of it for
            // runtime allocations.
            let mem = unsafe { block.as_slice() };
            shifter.absorb(mem);
        }

        // TODO: remove
        send_u32(shifter.counter);
        send_u32(self.bytes_written as u32);
        send_u32(reader.blocks[0].len() as u32);
        send_u32(reader.blocks[0].start());

        // Interpret the key as an array.
        <[u8;16]>::try_from(shifter.key())
            .map_err(|_| xous::Error::InternalError)
    }

}

/// Convenience helper function for sending numbers over USB/UART.
fn send_u32(x: u32) {
    let uart = crate::debug::Uart {};
    for b in x.to_le_bytes() {
        uart.putc(b);
    }
}

enum State {
    NotStarted,
    Erase,
    GetSeed,
    GetKeyBlock,
    RecoverKey,
    Done,
}

/// Erasure variant designed to be run without interactivity; the data is sent as raw bytes without
/// a repl-like interface.
///
/// The expected interaction is:
/// 1. Host sends:
///    1a. 4 bytes indicating requested ack frequency in bytes.
///    1b. 4 bytes indicating the rram offset to start erasure from.
/// 2. Device sends 4 bytes indicating requested total byte length.
/// 3. Repeat until total byte length is reached:
///    3a. Host sends <ack frequency> bytes, or remaining bytes if less.
///    3b. Device sends 4 bytes, encoding the total bytes received so far.
/// 4. Host sends the key and seed blocks.
/// 5. Device sends the recovered key.
///
/// Each party must wait for the other's messages before proceeding. For example, the host cannot
/// keep sending bytes without getting an ack in step 3. This prevents situations where due to
/// different clock frequencies, one party can fill a serial buffer much faster than the other one
/// can empty it.
pub struct OneShotErasure {
    state: State,
    erasure: Erasure,
    rx: Vec<u8>,
    seed: Vec<u8>,
    key_block: Vec<u8>,
    bytes_written: usize,
    bytes_to_fill: usize,
    last_ack: usize,
    ack_stride: usize,
}

impl OneShotErasure {
    // Determines how often we actually write the data. Buffering more data causes more stack usage;
    // buffering less incurs more overhead and internal buffering in the erase procedure.
    const WRITE_INTERVAL: usize = 256;

    // Sizes of seed and key block.
    const SEED_BYTES: usize = 16;
    const KEY_BYTES: usize = 16;

    pub fn new() -> Self {
        Self {
            state: State::NotStarted,
            erasure: Erasure::empty(),
            seed: Vec::with_capacity(Self::SEED_BYTES),
            key_block: Vec::with_capacity(Self::KEY_BYTES),
            rx: Vec::with_capacity(Self::WRITE_INTERVAL),
            bytes_written: 0,
            bytes_to_fill: 0,
            ack_stride: 0,
            last_ack: 0,
        }
    }
}

impl SerialInteract for OneShotErasure {
    fn rx_char(&mut self, c: u8) {
        match self.state {
            State::GetSeed => {
                self.seed.push(c);
            },
            State::GetKeyBlock => {
                self.key_block.push(c);
            },
            _ => {
            self.rx.push(c);
            }
        }
    }

    fn process(&mut self) {
        match &self.state {
            State::NotStarted => {
                if self.rx.len() >= 8 {
                    let (chunks, _) = self.rx.as_chunks::<4>();
                    let stride = u32::from_le_bytes(chunks[0]);
                    let rram_offset = u32::from_le_bytes(chunks[1]);
                    self.ack_stride = stride as usize;
                    self.rx.clear();
                    match Erasure::new(rram_offset) {
                        Ok(erasure) => {
                            self.state = State::Erase;
                            self.bytes_to_fill = erasure.len();
                            self.erasure = erasure;
                            send_u32(0); // "no error" code
                            send_u32(self.bytes_to_fill as u32);

                            // TODO: remove, debugging
                            let write_start = self.erasure.traversal.blocks[0].start();
                            send_u32(write_start);
                            send_u32(self.erasure.traversal.blocks[0].end());
                            send_u32(self.erasure.traversal.blocks[0].len() as u32);
                            let reader = MemoryTraversal::new(0).unwrap();
                            let mem = unsafe { &reader.blocks[0].as_slice() };
                            let read_start = reader.blocks[0].start();
                            for i in 0..8 {
                                let offset: usize = (write_start - read_start) as usize - (4 * (i+1));
                                let mut word = [0u8;4];
                                word.copy_from_slice(&mem[offset..offset+4]);
                                send_u32(read_start+offset as u32);
                                send_u32(u32::from_le_bytes(word));
                            }
                        }
                        Err(e) => {
                            send_u32(e.to_usize() as u32);
                        }
                    }
                }
            },
            State::Erase => {
                if self.rx.len() >= Self::WRITE_INTERVAL
                    || self.rx.len() >= self.last_ack + self.ack_stride
                    || self.rx.len() >= self.bytes_to_fill {
                    let src = &self.rx[..self.rx.len().min(self.bytes_to_fill)];
                    self.erasure.write_slice(src);
                    self.bytes_written += src.len();
                    self.bytes_to_fill -= src.len();
                    self.rx.clear();
                    if self.bytes_written >= self.last_ack + self.ack_stride {
                        send_u32(self.bytes_written as u32);
                        self.last_ack += self.ack_stride;
                    }
                    if self.bytes_to_fill == 0 {
                        self.state = State::GetSeed;
                    }
                }
            },
            State::GetSeed => {
                if self.seed.len() == Self::SEED_BYTES {
                    self.state = State::GetKeyBlock;
                }
            },
            State::GetKeyBlock => {
                if self.key_block.len() == Self::KEY_BYTES {
                    self.state = State::RecoverKey;
                }
            },
            State::RecoverKey => {
                // Perform key recovery.
                let key: [u8;16] = self.erasure
                    .recover_key(self.seed.as_slice(), self.key_block.as_slice())
                    .unwrap();

                // send the key to the host; despite the name, Uart::putc sends over USB if possible
                let uart = crate::debug::Uart {};
                for b in key {
                    uart.putc(b);
                }
                self.state = State::Done;
            },
            State::Done => (),
        }
    }
}
