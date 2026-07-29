// use aes::cipher::{RegionDecrypt, RegionEncrypt, KeyInit};
// use aes::{Aes128, Region};
use bao1x_api::offsets;
use bao1x_hal::rram;

/// Traversal through a contiguous memory block, reading or writing each address exactly once.
struct Region {
    start: u64,
    end: u64,
    current: u64,
}

impl Region {
    fn new(start: u64, len: u64) -> Self {
        if start > u64::MAX - len {
            // TODO: remove details from panics to reduce code size
            panic!("Integer overflow in block creation (start={:?}, len={:?})!", start, len);
        }
        if len % 4 != 0{
            panic!("Region length of {:?} is not a multiple of 4 bytes", len);
        }
        Region {
            start: start,
            end: start + len,
            current: start,
        }
    }

    pub fn peek(&self) -> u64 {
        return self.current;
    }

    pub fn len(&self) -> usize {
        return (self.end - self.current) as usize;
    }

    fn increment(&mut self, nbytes: usize) {
        if self.current > self.end - nbytes as u64 {
            panic!("Increment ({:?} + {:?}) overflowed block!", self.current, nbytes);
        }
        self.current += nbytes as u64;
        if self.current % 4 != 0 {
            panic!("Region address misaligned: {:?}", self.current);
        }
    }

    pub fn write_u32(&mut self, value: u32) {
        let addr = self.current as *mut u32;
        // safety: if the whole block is a writeable memory region and callers only ever increment
        // the block through increment(), we ensure that the addr is a u32-aligned valid writeable
        // address and the entire write fits within the region.
        unsafe { *addr = value };
        self.increment(4);
    }

    pub fn read_u32(&mut self) -> u32 {
        let addr = self.current as *const u32;
        // safety: if the whole block is a readable memory region and callers only ever increment
        // the block through increment(), we ensure that the addr is a u32-aligned valid readable
        // address and the entire read fits within the region.
        let value = unsafe { *addr };
        self.increment(4);
        return value;
    }
}

/// Traverses through multiple non-contiguous memory blocks.
struct MemoryTraversal {
    idx: usize,
    blocks: [Region;1],
    // TODO: investigate/add mem regions from utralib/src/generated/bao1x.rs
    // TODO: maybe add a block of constant ciphertext to the program to start so it fills all of the
    // boot1 region?
}

impl MemoryTraversal {
    fn new() -> Self {
        // Determine the dimensions of the writeable part of the RRAM memory block. This is where
        // boot0 and boot1 live, so not all of this block is writeable.
        let rram_end = utralib::HW_RERAM_MEM + utralib::HW_RERAM_MEM_LEN;
        let rram_block_start = rram_end - bao1x_api::RRAM_STORAGE_LEN;
        let rram_block_len = bao1x_api::RRAM_STORAGE_LEN;
        let rram_block = Region::new(rram_block_start as u64, rram_block_len as u64);
        MemoryTraversal {
            idx: 0,
            blocks: [rram_block],
        }
    }

    fn len(&self) -> usize {
        let mut total = 0;
        for i in self.idx..self.blocks.len() {
            total += self.blocks[i].len();
        }
        return total;
    }

    fn peek(&self) -> u64 {
        self.blocks[self.idx].peek()
    }

    fn advance_block(&mut self) {
        if self.idx < self.blocks.len() - 1 {
            self.idx += 1;
        } else {
            panic!("Region index overflow!");
        }
    }

    fn write_u32(&mut self, value: u32) {
        while self.blocks[self.idx].len() < 4 {
            self.advance_block();
        }
        self.blocks[self.idx].write_u32(value);
    }

    fn read_u32(&mut self) -> u32 {
        while self.blocks[self.idx].len() < 4 {
            self.advance_block();
        }
        return self.blocks[self.idx].read_u32();
    }
}

pub struct Erasure {
    traversal: MemoryTraversal,
}

impl Erasure {
    pub fn new() -> Self {
        Erasure {
            traversal: MemoryTraversal::new(),
        }
    }

    /// Remaining length to fill.
    pub fn len(&self) -> usize {
        self.traversal.len()
    }

    /// Write new ciphertext data.
    pub fn write_u32(&mut self, data: u32) {
        self.traversal.write_u32(data);
    }

    /*
    /// Recover the key from the ciphertext, shift seed, and key block.
    pub fn recover_key(&self, shift_seed: u32, key_block: aes::Region) -> aes::Region {
        // TODO
    }
    */

    /*
    /// Decrypt the ciphertext in memory, in-place.
    pub fn decrypt(&self, key: aes::Region) -> aes::Region {
        ciphertext_traversal = MemoryTraversal::new();
        plaintext_traversal = MemoryTraversal::new();
        // TODO
    }
    */
}
