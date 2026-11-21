// Copyright 2026 RealBrakedownPerf Authors
//
// This file is part of lcpc-2d, which is part of lcpc.
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

/*!
Portable scalar ChaCha20 keystream generator matching `rand_chacha::ChaCha20Rng`.

Generates one 64-byte (16-word) ChaCha20 block at a time in 16 `u32` scalar
locals with a 64-bit block counter (words 12–13) and 64-bit stream identifier
(words 14–15), refilling a 4-block (64-word) buffer and serving `TryRng` with
`rand_core::block::BlockRng` word semantics.
*/

use core::convert::Infallible;
use core::fmt;
use rand::{SeedableRng, TryCryptoRng, TryRng};

const SIGMA: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];
const BLOCK_WORDS: usize = 16;
const BUF_BLOCKS: u64 = 4;
const BUF_WORDS: usize = BLOCK_WORDS * BUF_BLOCKS as usize;

/// Compute a single 16-word ChaCha20 block using 16 `u32` scalar locals.
#[inline(always)]
fn chacha20_block(key: &[u32; 8], stream: u64, block: u64, out: &mut [u32; BLOCK_WORDS]) {
    let s12 = block as u32;
    let s13 = (block >> 32) as u32;
    let s14 = stream as u32;
    let s15 = (stream >> 32) as u32;

    let mut x0 = SIGMA[0];
    let mut x1 = SIGMA[1];
    let mut x2 = SIGMA[2];
    let mut x3 = SIGMA[3];
    let mut x4 = key[0];
    let mut x5 = key[1];
    let mut x6 = key[2];
    let mut x7 = key[3];
    let mut x8 = key[4];
    let mut x9 = key[5];
    let mut x10 = key[6];
    let mut x11 = key[7];
    let mut x12 = s12;
    let mut x13 = s13;
    let mut x14 = s14;
    let mut x15 = s15;

    macro_rules! qr {
        ($a:expr, $b:expr, $c:expr, $d:expr) => {
            $a = $a.wrapping_add($b);
            $d ^= $a;
            $d = $d.rotate_left(16);
            $c = $c.wrapping_add($d);
            $b ^= $c;
            $b = $b.rotate_left(12);
            $a = $a.wrapping_add($b);
            $d ^= $a;
            $d = $d.rotate_left(8);
            $c = $c.wrapping_add($d);
            $b ^= $c;
            $b = $b.rotate_left(7);
        };
    }

    macro_rules! dr {
        () => {
            qr!(x0, x4, x8, x12);
            qr!(x1, x5, x9, x13);
            qr!(x2, x6, x10, x14);
            qr!(x3, x7, x11, x15);

            qr!(x0, x5, x10, x15);
            qr!(x1, x6, x11, x12);
            qr!(x2, x7, x8, x13);
            qr!(x3, x4, x9, x14);
        };
    }

    // 10 double rounds (20 rounds), unrolled so no loop counter competes for registers.
    dr!();
    dr!();
    dr!();
    dr!();
    dr!();
    dr!();
    dr!();
    dr!();
    dr!();
    dr!();

    out[0] = x0.wrapping_add(SIGMA[0]);
    out[1] = x1.wrapping_add(SIGMA[1]);
    out[2] = x2.wrapping_add(SIGMA[2]);
    out[3] = x3.wrapping_add(SIGMA[3]);
    out[4] = x4.wrapping_add(key[0]);
    out[5] = x5.wrapping_add(key[1]);
    out[6] = x6.wrapping_add(key[2]);
    out[7] = x7.wrapping_add(key[3]);
    out[8] = x8.wrapping_add(key[4]);
    out[9] = x9.wrapping_add(key[5]);
    out[10] = x10.wrapping_add(key[6]);
    out[11] = x11.wrapping_add(key[7]);
    out[12] = x12.wrapping_add(s12);
    out[13] = x13.wrapping_add(s13);
    out[14] = x14.wrapping_add(s14);
    out[15] = x15.wrapping_add(s15);
}

/// Fill `out` with consecutive ChaCha20 keystream blocks starting at `first_block`.
///
/// The block counter advances with wrapping 64-bit addition after each 16-word
/// block.
///
/// # Panics
///
/// Panics if `out.len()` is not a multiple of 16.
#[inline(never)]
pub fn keystream_blocks(key: &[u32; 8], stream: u64, first_block: u64, out: &mut [u32]) {
    assert_eq!(
        out.len() % BLOCK_WORDS,
        0,
        "keystream_blocks output length must be a multiple of 16"
    );
    let mut ctr = first_block;
    for chunk in out.chunks_exact_mut(BLOCK_WORDS) {
        let block_out: &mut [u32; BLOCK_WORDS] = chunk.try_into().unwrap();
        chacha20_block(key, stream, ctr, block_out);
        ctr = ctr.wrapping_add(1);
    }
}

/// Portable scalar ChaCha20 stream generator compatible with `rand_chacha::ChaCha20Rng`.
///
/// Emits the identical 32-bit word stream for any 32-byte seed, 64-bit stream
/// identifier, and 68-bit word position, and implements `SeedableRng`,
/// `TryRng<Error = Infallible>` (hence `Rng`), and `TryCryptoRng`.
#[derive(Clone)]
pub struct ChaCha20Stream {
    key: [u32; 8],
    stream: u64,
    block: u64,
    buf: [u32; BUF_WORDS],
    idx: usize,
}

impl fmt::Debug for ChaCha20Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChaCha20Stream")
            .field("stream", &self.stream)
            .field("block", &self.block)
            .field("idx", &self.idx)
            .finish_non_exhaustive()
    }
}

impl ChaCha20Stream {
    /// Refill the 4-block output buffer from `self.block..self.block + 4`.
    #[inline]
    fn refill(&mut self) {
        keystream_blocks(&self.key, self.stream, self.block, &mut self.buf);
        self.block = self.block.wrapping_add(BUF_BLOCKS);
        self.idx = 0;
    }

    #[inline(never)]
    fn next_u64_slow(&mut self) -> u64 {
        if self.idx == BUF_WORDS - 1 {
            let lo = self.buf[BUF_WORDS - 1];
            self.refill();
            let hi = self.buf[0];
            self.idx = 1;
            (u64::from(hi) << 32) | u64::from(lo)
        } else {
            self.refill();
            let lo = self.buf[0];
            let hi = self.buf[1];
            self.idx = 2;
            (u64::from(hi) << 32) | u64::from(lo)
        }
    }

    /// Return the 8 little-endian 32-bit key words derived from the seed.
    #[inline]
    pub fn key(&self) -> &[u32; 8] {
        &self.key
    }

    /// Return the current 64-bit stream identifier.
    #[inline]
    pub fn get_stream(&self) -> u64 {
        self.stream
    }

    /// Set the 64-bit stream identifier while preserving the current word position.
    ///
    /// If words from the current buffered 4-block window have already been
    /// consumed, the buffer is regenerated for the new stream at the same
    /// position.
    #[inline]
    pub fn set_stream(&mut self, stream: u64) {
        self.stream = stream;
        if self.idx < BUF_WORDS {
            let wp = self.get_word_pos();
            self.set_word_pos(wp);
        }
    }

    /// Return the current offset from the start of the stream in 32-bit words.
    #[inline]
    pub fn get_word_pos(&self) -> u128 {
        if self.idx >= BUF_WORDS {
            u128::from(self.block) * (BLOCK_WORDS as u128)
        } else {
            let base_block = self.block.wrapping_sub(BUF_BLOCKS);
            u128::from(base_block) * (BLOCK_WORDS as u128) + (self.idx as u128)
        }
    }

    /// Seek to the given 32-bit word offset from the start of the stream.
    ///
    /// The upper 60 bits above the 68-bit period are ignored, matching
    /// `rand_chacha::ChaCha20Rng::set_word_pos`.
    #[inline]
    pub fn set_word_pos(&mut self, word_pos: u128) {
        let block = (word_pos / (BLOCK_WORDS as u128)) as u64;
        let rem = (word_pos % (BLOCK_WORDS as u128)) as usize;
        self.block = block;
        if rem == 0 {
            self.idx = BUF_WORDS;
        } else {
            self.refill();
            self.idx = rem;
        }
    }
}

impl SeedableRng for ChaCha20Stream {
    type Seed = [u8; 32];

    #[inline]
    fn from_seed(seed: Self::Seed) -> Self {
        let mut key = [0u32; 8];
        for (dst, chunk) in key.iter_mut().zip(seed.chunks_exact(4)) {
            *dst = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        Self {
            key,
            stream: 0,
            block: 0,
            buf: [0u32; BUF_WORDS],
            idx: BUF_WORDS,
        }
    }
}

impl TryRng for ChaCha20Stream {
    type Error = Infallible;

    #[inline]
    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        if self.idx >= BUF_WORDS {
            self.refill();
        }
        let w = self.buf[self.idx];
        self.idx += 1;
        Ok(w)
    }

    #[inline]
    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        let idx = self.idx;
        if idx < BUF_WORDS - 1 {
            let lo = self.buf[idx];
            let hi = self.buf[idx + 1];
            self.idx = idx + 2;
            return Ok((u64::from(hi) << 32) | u64::from(lo));
        }
        Ok(self.next_u64_slow())
    }

    #[inline]
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        let mut offset = 0;
        while offset < dst.len() {
            if self.idx >= BUF_WORDS {
                self.refill();
            }
            let avail_words = BUF_WORDS - self.idx;
            let rem_bytes = dst.len() - offset;
            let full_words = (rem_bytes / 4).min(avail_words);
            for i in 0..full_words {
                let w = self.buf[self.idx + i];
                dst[offset + 4 * i..offset + 4 * i + 4].copy_from_slice(&w.to_le_bytes());
            }
            self.idx += full_words;
            offset += full_words * 4;
            if offset < dst.len() && self.idx < BUF_WORDS {
                let tail = dst.len() - offset;
                let w = self.buf[self.idx];
                self.idx += 1;
                dst[offset..].copy_from_slice(&w.to_le_bytes()[..tail]);
                break;
            }
        }
        Ok(())
    }
}

impl TryCryptoRng for ChaCha20Stream {}

#[cfg(test)]
mod tests {
    use super::{keystream_blocks, ChaCha20Stream};
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha20Rng;

    fn test_seeds() -> Vec<[u8; 32]> {
        let mut inc = [0u8; 32];
        for (i, b) in inc.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(13).wrapping_add(7);
        }
        let mut high_bit = [0u8; 32];
        high_bit[31] = 1;
        high_bit[0] = 0x80;
        high_bit[15] = 0xfe;
        vec![
            [0u8; 32],
            [0xffu8; 32],
            [0x5au8; 32],
            inc,
            high_bit,
        ]
    }

    #[test]
    fn matches_chacha20rng_from_seed_and_streams_10k_u64() {
        let streams = [0u64, 1, 7, u64::MAX];
        for seed in test_seeds() {
            for &stream in &streams {
                let mut ours = ChaCha20Stream::from_seed(seed);
                let mut ref_rng = ChaCha20Rng::from_seed(seed);
                ours.set_stream(stream);
                ref_rng.set_stream(stream);
                assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());
                for i in 0..10_000 {
                    assert_eq!(
                        ours.next_u64(),
                        ref_rng.next_u64(),
                        "u64 mismatch at step {i} stream {stream}"
                    );
                }
                assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());
            }
        }
    }

    #[test]
    fn matches_chacha20rng_seed_from_u64_and_streams() {
        let u64_seeds = [0u64, 1, u64::MAX, 0x4c43_5043_dead_beef];
        let streams = [0u64, 1, 7, u64::MAX];
        for &seed in &u64_seeds {
            for &stream in &streams {
                let mut ours = ChaCha20Stream::seed_from_u64(seed);
                let mut ref_rng = ChaCha20Rng::seed_from_u64(seed);
                ours.set_stream(stream);
                ref_rng.set_stream(stream);
                assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());
                for i in 0..10_000 {
                    assert_eq!(
                        ours.next_u64(),
                        ref_rng.next_u64(),
                        "seed_from_u64({seed:#x}) stream {stream} mismatch at {i}"
                    );
                }
                assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());
            }
        }
    }

    #[test]
    fn matches_chacha20rng_mixed_ops_and_fill_bytes_0_to_70() {
        let mut ours = ChaCha20Stream::seed_from_u64(0x1234_5678_9abc_def0);
        let mut ref_rng = ChaCha20Rng::seed_from_u64(0x1234_5678_9abc_def0);
        ours.set_stream(7);
        ref_rng.set_stream(7);

        // Exhaustively test every fill_bytes length 0..=70 interspersed with u32/u64 draws.
        for len in 0..=70usize {
            let mut a = vec![0xaa; len];
            let mut b = vec![0xaa; len];
            ours.fill_bytes(&mut a);
            ref_rng.fill_bytes(&mut b);
            assert_eq!(a, b, "fill_bytes mismatch at len {len}");
            assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());

            assert_eq!(ours.next_u32(), ref_rng.next_u32());
            assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());
            assert_eq!(ours.next_u64(), ref_rng.next_u64());
            assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());
        }

        // Pseudo-random mix of next_u32, next_u64, and fill_bytes (0..=70).
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut buf_a = [0u8; 70];
        let mut buf_b = [0u8; 70];
        for step in 0..2_000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            match (state >> 60) % 3 {
                0 => {
                    assert_eq!(ours.next_u32(), ref_rng.next_u32(), "step {step} u32");
                }
                1 => {
                    assert_eq!(ours.next_u64(), ref_rng.next_u64(), "step {step} u64");
                }
                _ => {
                    let len = ((state >> 32) as usize) % 71;
                    ours.fill_bytes(&mut buf_a[..len]);
                    ref_rng.fill_bytes(&mut buf_b[..len]);
                    assert_eq!(buf_a[..len], buf_b[..len], "step {step} fill({len})");
                }
            }
            assert_eq!(
                ours.get_word_pos(),
                ref_rng.get_word_pos(),
                "word_pos mismatch at step {step}"
            );
        }
    }

    #[test]
    fn matches_chacha20rng_set_stream_mid_buffer() {
        // Test set_stream after consuming various word counts covering empty, block-aligned,
        // mid-block, and boundary positions inside the 64-word buffer.
        for &prefix_words in &[0usize, 1, 15, 16, 17, 32, 48, 63, 64, 65, 130] {
            let mut ours = ChaCha20Stream::seed_from_u64(0xfeed_face_cafe_babe);
            let mut ref_rng = ChaCha20Rng::seed_from_u64(0xfeed_face_cafe_babe);

            for _ in 0..prefix_words {
                assert_eq!(ours.next_u32(), ref_rng.next_u32());
            }
            assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());

            for &new_stream in &[1u64, 7, u64::MAX, 0] {
                ours.set_stream(new_stream);
                ref_rng.set_stream(new_stream);
                assert_eq!(
                    ours.get_word_pos(),
                    ref_rng.get_word_pos(),
                    "word_pos after set_stream({new_stream}) at prefix {prefix_words}"
                );
                for j in 0..80 {
                    assert_eq!(
                        ours.next_u32(),
                        ref_rng.next_u32(),
                        "word {j} after set_stream({new_stream}) at prefix {prefix_words}"
                    );
                }
                assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());
            }
        }
    }

    #[test]
    fn matches_chacha20rng_counter_carry_across_2_pow_32() {
        // Position just below block 2^32 so advancing 200 words carries from word 12 into word 13.
        let positions = [
            ((1u128 << 32) - 2) * 16,
            ((1u128 << 32) - 2) * 16 + 7,
            ((1u128 << 32) - 1) * 16 + 15,
            ((u64::MAX as u128) - 1) * 16 + 5,
        ];
        for &pos in &positions {
            let mut ours = ChaCha20Stream::seed_from_u64(0x0bad_f00d_1337_c0de);
            let mut ref_rng = ChaCha20Rng::seed_from_u64(0x0bad_f00d_1337_c0de);
            ours.set_stream(u64::MAX);
            ref_rng.set_stream(u64::MAX);

            ours.set_word_pos(pos);
            ref_rng.set_word_pos(pos);
            assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());

            for i in 0..200 {
                assert_eq!(
                    ours.next_u32(),
                    ref_rng.next_u32(),
                    "carry mismatch at pos {pos:#x} step {i}"
                );
                assert_eq!(ours.get_word_pos(), ref_rng.get_word_pos());
            }
        }
    }

    #[test]
    fn keystream_blocks_matches_chacha20rng() {
        let seed = test_seeds()[3];
        let ours_rng = ChaCha20Stream::from_seed(seed);
        let stream = 0x1122_3344_5566_7788u64;
        let first_block = (1u64 << 32) - 2;
        let mut out = vec![0u32; 16 * 12];
        keystream_blocks(ours_rng.key(), stream, first_block, &mut out);

        let mut ref_rng = ChaCha20Rng::from_seed(seed);
        ref_rng.set_stream(stream);
        ref_rng.set_word_pos(u128::from(first_block) * 16);
        for (i, &w) in out.iter().enumerate() {
            assert_eq!(w, ref_rng.next_u32(), "keystream_blocks word {i}");
        }

        // Empty output slice is a valid multiple of 16.
        let mut empty: [u32; 0] = [];
        keystream_blocks(ours_rng.key(), stream, first_block, &mut empty);
    }

    #[test]
    #[should_panic(expected = "multiple of 16")]
    fn keystream_blocks_rejects_non_multiple_of_16() {
        let key = [0u32; 8];
        let mut out = [0u32; 17];
        keystream_blocks(&key, 0, 0, &mut out);
    }

    #[test]
    fn debug_hides_key_and_clone_matches() {
        let seed = [0xab; 32];
        let mut a = ChaCha20Stream::from_seed(seed);
        let dbg = format!("{a:?}");
        assert!(dbg.contains("ChaCha20Stream"));
        assert!(!dbg.contains("key"));
        assert!(!dbg.contains("2880154539")); // 0xabab_abab in decimal
        assert!(!dbg.contains("abab"));

        let _ = a.next_u64();
        let mut b = a.clone();
        for _ in 0..200 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }
}
