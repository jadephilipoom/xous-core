use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::{Aes128, Block};
use bao1x_api::offsets;
use bao1x_hal::rram;

struct Erasure {
    offset: u64,
}

impl Erasure {
    pub fn new() -> Self{
        Erasure {
            offset: 0,
        }
    }

    fn recover_key(&self, key_block: &Aes::Block, shift_seed: u32) {
        let mut ciphertext_offset = 0;
        let mut rram = bao1x_hal::rram::Reram::new();
        while ciphertext_offset < self.offset {
            let addr = self.offset + bao1x_api::offsets::BOOT1_START;
            rram.write_slice(addr, block.as_slice()).ok();
            ciphertext_offset += block.len();
        }
    }

    fn next_addr(&self) -> u32 {
        // TODO: figure out the destination based on the number of bytes written so far, so that all
        // available memory is eventually filled. Temporarily, we just write into RRAM; fails if the
        // offset gets too big.
        let rram_fill_start = bao1x_api::offsets::BOOT1_START;
        let rram_fill_len = bao1x_api::RRAM_STORAGE_LEN - rram_fill_start;
        if self.offset > rram_fill_len {
            panic!("Offset too large!");
        }
        return rram_fill_start + offset;
    }

    pub fn write_block(&mut self, block: &Aes::Block) {
        let addr = self.next_addr();
        let dst = unsafe { core::slice::from_raw_parts(addr as *const u32, aes::BLOCK_SIZE) };
        dst.copy_from_slice(block.as_slice().ok());
        self.offset += aes::BLOCK_SIZE;
    }

    pub fn decrypt_payload(&mut self, key_block: &Aes::Block, shift_seed: u32) {
        log::info!("Recovering key...");
        

        log::info!("Setting up AES...");
        let mut output = Block::default();
        let aes = Aes128::new_from_slice(&self.key).unwrap();

        log::info!("Decrypting {:x?} ciphertext bytes...", self.offset);
        self.offset = 0;

        log::info!("Setting key");
        log::info!("Running encryption");
        aes.encrypt_block(&mut output);
        log::info!("Key:       {:x?}", self.key);
        log::info!("Plaintext: {:x?}", self.plaintext);
        log::info!("Reference: {:x?}", self.ciphertext);
        log::info!("Result:    {:x?}", output);
    }
}
