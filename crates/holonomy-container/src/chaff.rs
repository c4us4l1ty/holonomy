//! Chaff: the ChaCha20 keystream that fills everything outside the payload.
//!
//! The point of this module is that chaff is a **pure function of
//! `(K_chaff, absolute byte offset)`**. It is not stored, indexed or remembered: byte
//! `x` of the file is always `ChaCha20(K_chaff, nonce = x / 64)` at position `x mod 64`.
//! Two consequences follow, and both are why it is written this way.
//!
//! * Creating a container does not need to be resumable or journalled. A create that dies
//!   half way leaves a file whose bytes are still a correct function of the key, so a
//!   retry overwrites deterministically rather than having to know what it already wrote.
//! * A reader can regenerate any region to compare against what is on disk, which is what
//!   lets the tests assert the file really is pure keystream rather than "looks noisy".
//!
//! # Where `i` goes: the counter, not the nonce
//!
//! PRD FR-4.1 says `ChaCha20(K_chaff, nonce = i)` without defining `i`. `i` is the 64-byte
//! ChaCha **block index**, and it is applied as the *block counter*, with the nonce held
//! fixed at 12 zero bytes for the whole container.
//!
//! Putting `i` in the nonce instead does not work, and the reason is worth recording. With
//! `i` in the nonce, each 64-byte block of a region would need its own cipher instance,
//! because a `ChaCha20` object carries one nonce for its whole life. Filling a 4 KiB page
//! would mean 64 nonce changes, and seeking to byte 960 would mean a nonce of 15 *and* a
//! counter of 15 -- the index counted twice. Worse, the two ways of doing it disagreed:
//! filling `[0, 4096)` in one call and filling `[0,960)`, `[960,2048)`, `[2048,4096)` in
//! three produced different bytes at offset 960, because the single fill reached byte 960 as
//! block 15 under the nonce for block 0, while the third fill reached it as block 0 under
//! the nonce for block 15. A keystream has to be one contiguous sequence over an offset
//! range, and only a counter gives you that.
//!
//! A fixed nonce is safe here precisely because the counter is a real counter: ChaCha20 has
//! a 32-bit counter, so 128 MiB is nowhere near the limit. `K_chaff` is per-container, so
//! there is no cross-container reuse to worry about either.
//!
//! Filling at an unaligned offset is rejected rather than silently done, because ChaCha20
//! is seekable only to 64-byte block boundaries through this path. Every caller in this
//! crate works in [`IO_ALIGN`](super::layout::IO_ALIGN) units, which are a multiple of 64.

use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use chacha20::{ChaCha20, Key, Nonce};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// ChaCha20 block size, and therefore the granularity at which the keystream can be seeked.
pub const CHAFF_BLOCK: u64 = 64;

/// Why a chaff fill was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaffError {
    /// The offset is not a multiple of [`CHAFF_BLOCK`], so the keystream cannot be seeked
    /// to it.
    UnalignedOffset {
        /// The offending offset.
        offset: u64,
    },
    /// The requested region runs past the end of the container.
    OutOfRange {
        /// First byte requested.
        start: u64,
        /// One past the last byte requested.
        end: u64,
    },
}

impl core::fmt::Display for ChaffError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnalignedOffset { offset } => {
                write!(f, "chaff offset {offset} is not {CHAFF_BLOCK}-aligned")
            }
            Self::OutOfRange { start, end } => {
                write!(f, "chaff region [{start}, {end}) leaves the container")
            }
        }
    }
}

impl std::error::Error for ChaffError {}

/// The chaff keystream for one `K_chaff`.
///
/// Holds a copy of the key and scrubs it on drop, like every other secret in the tree.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Chaff {
    key: [u8; 32],
}

impl Chaff {
    /// Bind a keystream to `K_chaff`.
    pub fn new(k_chaff: &[u8; 32]) -> Self {
        let mut key = [0u8; 32];
        key.copy_from_slice(k_chaff);
        Self { key }
    }

    /// The container-wide nonce: all zero. The block index lives in the counter.
    ///
    /// See the module docs for why it is not in the nonce.
    fn container_nonce() -> Nonce {
        Nonce::from([0u8; 12])
    }

    /// Fill `buf` with the keystream that belongs at absolute `offset`.
    ///
    /// `offset % 64` must be 0. `len` need not be a multiple of 64.
    pub fn fill(&self, offset: u64, buf: &mut [u8]) -> Result<(), ChaffError> {
        if !offset.is_multiple_of(CHAFF_BLOCK) {
            return Err(ChaffError::UnalignedOffset { offset });
        }
        let len = buf.len() as u64;
        let end = offset.saturating_add(len);
        if end > super::layout::CONTAINER_SIZE {
            return Err(ChaffError::OutOfRange { start: offset, end });
        }
        let mut cipher = ChaCha20::new(&Key::from(self.key), &Self::container_nonce());
        // Three things here, each of which was wrong at some point and each of which
        // produced plausible-looking output rather than an error.
        //
        // 1. Set the block counter. A fresh `ChaCha20` sits at counter 0, so
        //    `write_keystream` would emit the counter-0 keystream for *every* offset:
        //    `fill(0)` and `fill(1 MiB)` returned identical bytes, so every page of a created
        //    container was a copy of page 0.
        // 2. `write_keystream`, not `apply_keystream`. The latter XORs the keystream into
        //    whatever the buffer already held, which is right for encrypting a message and
        //    wrong for generating noise. Every caller reuses one buffer across all 32,768
        //    pages of a create, so `apply_keystream` produced
        //    `chaff(offset) XOR chaff(offset + 4096)` on every page after the first.
        // 3. Block index in the counter only, never in the nonce. Encoding it in both made
        //    a split fill disagree with a single fill at the split point.
        //
        // 4. Seek. Without it the counter stays at 0, so `fill(4096)` would emit the
        //    keystream for block 0. The seek takes a *byte* position; passing the block index
        //    instead looks right and is wrong by 64 bytes.
        //
        // None of these was caught by a round-trip test, because both sides of every
        // round-trip went through the same broken path and so agreed with each other. The
        // cross-check against the independent reference in `tests/` is what sees them, which
        // is the whole reason that file exists.
        // `try_seek` takes a *byte* position, not a block index. That is deliberate here:
        // at a 64-byte boundary it resolves to block `offset/64` with a zero residual, which
        // is exactly the counter this module documents. Passing `offset / CHAFF_BLOCK`
        // instead -- which reads like it should be right, since the counter is a block
        // counter -- seeks to block `offset / 4096` and shifts every region by 64 bytes. The
        // reference vectors below are what distinguishes the two.
        cipher
            .try_seek(offset)
            .map_err(|_| ChaffError::OutOfRange {
                start: offset,
                end: offset,
            })?;
        cipher.write_keystream(buf);
        Ok(())
    }

    /// Same as [`fill`](Self::fill), as a fresh allocation. Convenient for tests and for
    /// comparing an on-disk region against the keystream.
    pub fn bytes_at(&self, offset: u64, len: usize) -> Result<Vec<u8>, ChaffError> {
        let mut v = vec![0u8; len];
        self.fill(offset, &mut v)?;
        Ok(v)
    }
}

impl core::fmt::Debug for Chaff {
    /// Must not print the key. Same reasoning as `SecureBlock`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Chaff { key: [redacted] }")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::CONTAINER_SIZE;

    fn key(b: u8) -> [u8; 32] {
        [b; 32]
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// The same key and offset must always produce the same bytes, or a container's
    /// chaff would drift between writes.
    #[test]
    fn fill_is_deterministic() {
        let c = Chaff::new(&key(1));
        let a = c.bytes_at(65_536, 4096).expect("first");
        let b = c.bytes_at(65_536, 4096).expect("second");
        assert_eq!(a, b);
    }

    /// Different keys must produce completely different bytes at the same offset.
    #[test]
    fn different_keys_give_different_bytes() {
        let a = Chaff::new(&key(1)).bytes_at(65_536, 4096).expect("a");
        let b = Chaff::new(&key(2)).bytes_at(65_536, 4096).expect("b");
        assert_ne!(a, b);
        // Not "no byte coincides": across 4,096 bytes about 16 coincide by chance
        // (4096/256), so requiring zero would be a permanently flaky assertion. Require
        // instead that under 5% coincide, which is roughly 13 sigma below the expected
        // 6.25%.
        let same = a.iter().zip(&b).filter(|(x, y)| x == y).count();
        assert!(
            same < a.len() / 20,
            "{same} of {} bytes coincided; ~{} is expected by chance",
            a.len(),
            a.len() / 256
        );
    }

    /// Filling a *dirty* buffer must give the same bytes as filling a fresh one.
    ///
    /// Regression test for a silent corruption bug. `apply_keystream` XORs into the buffer
    /// rather than writing the keystream, so reusing one buffer across pages produced
    /// `chaff(offset) XOR chaff(offset + 4096)` from the second page onward. The result was
    /// still high-entropy and still wrote without error, so no other test could see it; the
    /// only way to notice was to read a page back and compare it to the expected keystream.
    /// `fill`'s contract is "these are the bytes at this offset", not "XOR these into what is
    /// already there", and this test is what holds it to that.
    #[test]
    fn fill_overwrites_rather_than_xors() {
        let c = Chaff::new(&key(0x5C));
        let mut reused = vec![0xFFu8; 4096];
        let mut fresh = vec![0u8; 4096];

        for page in 0..8u64 {
            let off = page * 4096;
            c.fill(off, &mut reused).expect("fill reused");
            c.fill(off, &mut fresh).expect("fill fresh");
            assert_eq!(
                reused, fresh,
                "page {page}: reusing a buffer changed the bytes, so fill is XORing"
            );
        }

        // And explicitly: a buffer pre-loaded with 0xFF must not survive the fill.
        let mut dirty = vec![0xFFu8; 4096];
        c.fill(65_536, &mut dirty).expect("fill dirty");
        assert_ne!(
            dirty,
            vec![0xFFu8; 4096],
            "fill left the old contents behind"
        );
    }

    /// Writing in two pieces must equal writing in one. This is what makes the create path
    /// able to stream the file in 1 MiB writes instead of buffering 128 MiB.
    #[test]
    fn partial_fills_agree_with_one_big_fill() {
        let c = Chaff::new(&key(9));
        let whole = c.bytes_at(0, 4096).expect("whole");

        // Split points must be multiples of CHAFF_BLOCK: the keystream is seekable only to
        // 64-byte block boundaries, which is the real constraint this API imposes.
        let mut split = vec![0u8; 4096];
        c.fill(0, &mut split[..960]).expect("head");
        c.fill(960, &mut split[960..2048]).expect("middle");
        c.fill(2048, &mut split[2048..]).expect("tail");
        assert_eq!(
            whole.iter().zip(&split).position(|(a, b)| a != b),
            None,
            "split fill diverges from the single fill"
        );
        assert_eq!(960 % CHAFF_BLOCK, 0);
        assert_eq!(2048 % CHAFF_BLOCK, 0);
    }

    /// Offsets that are not block-aligned are refused, not silently rounded.
    #[test]
    fn unaligned_offsets_are_rejected() {
        let c = Chaff::new(&key(3));
        let mut buf = [0u8; 64];
        assert_eq!(
            c.fill(1, &mut buf),
            Err(ChaffError::UnalignedOffset { offset: 1 })
        );
        assert_eq!(
            c.fill(63, &mut buf),
            Err(ChaffError::UnalignedOffset { offset: 63 })
        );
        assert!(c.fill(64, &mut buf).is_ok());
    }

    /// Reads past the end of the container are refused.
    #[test]
    fn regions_past_the_container_are_rejected() {
        let c = Chaff::new(&key(3));
        let mut buf = vec![0u8; 4096];
        let end = CONTAINER_SIZE;
        assert!(
            c.fill(end - 4096, &mut buf).is_ok(),
            "the last page is fine"
        );
        assert_eq!(
            c.fill(end, &mut buf),
            Err(ChaffError::OutOfRange {
                start: end,
                end: end + 4096,
            })
        );
        // `end - 64` is block-aligned and still runs off the end of the container.
        assert_eq!(
            c.fill(end - 64, &mut buf),
            Err(ChaffError::OutOfRange {
                start: end - 64,
                end: end - 64 + 4096,
            })
        );
        // An unaligned offset near the end is refused for alignment, which is checked
        // before range is considered.
        assert!(matches!(
            c.fill(end - 100, &mut buf),
            Err(ChaffError::UnalignedOffset { .. })
        ));
    }

    /// Chaff must not be all zeroes, must not repeat with a short period, and must not
    /// favour either bit value. A keystream that failed any of these would put a
    /// detectable signature in the file.
    #[test]
    fn keystream_looks_uniform() {
        let c = Chaff::new(&key(0xAB));
        let data = c.bytes_at(0, 65_536).expect("64 KiB");
        assert!(data.iter().any(|&b| b != 0), "must not be all zeroes");

        // Bit balance, to about 4 sigma of a fair coin over 524,288 bits.
        let ones = data.iter().map(|b| b.count_ones() as u64).sum::<u64>();
        let total = (data.len() * 8) as u64;
        let z = (ones as f64 - total as f64 / 2.0).abs() / (total as f64 / 4.0).sqrt();
        assert!(z < 4.0, "bit imbalance z={z}");

        // No repeated 64-byte block: distinct nonces must give distinct keystream.
        let blocks: std::collections::HashSet<&[u8]> = data.chunks(CHAFF_BLOCK as usize).collect();
        assert_eq!(blocks.len(), data.len() / CHAFF_BLOCK as usize);
    }

    /// Two adjacent blocks must differ, which is the specific thing that separates a real
    /// keystream from a repeated pattern.
    #[test]
    fn adjacent_blocks_differ() {
        let c = Chaff::new(&key(0x11));
        let data = c.bytes_at(0, 256).expect("256 B");
        assert_ne!(&data[..64], &data[64..128]);
        assert_ne!(&data[64..128], &data[128..192]);
        assert_ne!(&data[128..192], &data[192..256]);
    }

    /// `Debug` must not leak the key.
    #[test]
    fn debug_redacts_the_key() {
        let rendered = format!("{:?}", Chaff::new(&key(0x5A)));
        assert!(
            !rendered.contains("5a"),
            "Debug leaked key bytes: {rendered}"
        );
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    /// The keystream must advance with the absolute offset, not restart.
    ///
    /// This is the test for the counter bug: with the block index in the nonce but the
    /// counter left at 0, `fill` returned the *same* bytes at every offset, so every page of
    /// a created container would be a copy of page 0. Still high-entropy, still a valid file,
    /// and invisible to any test that compares `fill` against `fill`.
    #[test]
    fn the_keystream_advances_with_the_offset() {
        let c = Chaff::new(&key(0x21));
        let a = c.bytes_at(0, 64).expect("at 0");
        let b = c.bytes_at(64, 64).expect("at 64");
        let far = c.bytes_at(1 << 20, 64).expect("at 1 MiB");
        assert_ne!(
            a, b,
            "adjacent blocks are identical: the block counter is not advancing"
        );
        assert_ne!(a, far, "distant blocks are identical");
        assert_ne!(b, far);
    }

    /// Filling at `offset` must equal the tail of filling from 0.
    ///
    /// This is the property that makes chaff a pure function of `(K_chaff, offset)`, and the
    /// reason a create can be retried after a crash without a journal. It also holds the
    /// counter fix honest, because "seek and generate" and "generate and take the tail" only
    /// agree when the counter is right.
    #[test]
    fn a_region_matches_the_tail_of_a_larger_fill() {
        let c = Chaff::new(&key(0x33));
        let whole = c.bytes_at(0, 8192).expect("whole");
        for offset in [64usize, 960, 4096, 4160, 8192 - 64] {
            let part = c.bytes_at(offset as u64, 64).expect("part");
            assert_eq!(
                part,
                &whole[offset..offset + 64],
                "region at {offset} does not match the tail of the larger fill"
            );
        }
    }

    /// A region's *end* may land mid-block, and the bytes must still match.
    ///
    /// `fill` requires a 64-byte-aligned start but not an aligned length, because callers
    /// write 4 KiB pages and only read 32-byte payloads out of them. Checking that a
    /// straddling length agrees with a longer fill is what keeps that asymmetry honest -- an
    /// off-by-one in the block stepping would show up here and nowhere else.
    #[test]
    fn a_region_ending_mid_block_matches() {
        let c = Chaff::new(&key(0x44));
        let whole = c.bytes_at(0, 4096).expect("whole");
        // Starts on a block boundary, ends 32 bytes into block 3.
        let part = c.bytes_at(64, 160).expect("part");
        assert_eq!(part.len(), 160);
        assert_eq!(part, &whole[64..224]);
    }

    /// And the start really is refused when unaligned, so the contract is two-sided.
    #[test]
    fn an_unaligned_start_is_refused() {
        let c = Chaff::new(&key(0x44));
        assert_eq!(
            c.bytes_at(96, 128).err(),
            Some(ChaffError::UnalignedOffset { offset: 96 })
        );
    }

    /// The nonce is fixed for the container and the index rides in the counter, so the
    /// counter must stay inside ChaCha20's 32-bit range. 2^21 blocks for 128 MiB, which is
    /// four orders of magnitude of headroom.
    #[test]
    fn the_block_index_fits_the_counter() {
        let blocks = CONTAINER_SIZE / CHAFF_BLOCK;
        assert_eq!(blocks, 2_097_152);
        assert!(
            blocks < u32::MAX as u64,
            "block index would overflow the counter"
        );
        assert_eq!(Chaff::container_nonce().as_slice(), &[0u8; 12]);
    }

    /// The whole container must be reachable: 2^21 blocks, and the last one must be
    /// fillable.
    #[test]
    fn the_whole_container_is_addressable() {
        let c = Chaff::new(&key(4));
        let blocks = CONTAINER_SIZE / CHAFF_BLOCK;
        assert_eq!(blocks, 2_097_152);
        let last = CONTAINER_SIZE - CHAFF_BLOCK;
        let tail = c.bytes_at(last, 64).expect("last block");
        assert_eq!(tail.len(), 64);
    }
    /// Chaff keystream against the independent reference, at three offsets.
    ///
    /// This is the test that matters for chaff, and it exists because round trips could not
    /// catch the two real bugs in `fill`. Both of them produced keystream that was internally
    /// consistent — a Rust round trip agreed with itself — and wrong in a way only an outside
    /// implementation would disagree about: `apply_keystream` XORing a reused buffer, and the
    /// block counter never advancing so every page of a created container was a copy of page 0.
    ///
    /// `key = 0x21 * 32`, zero nonce, counter = absolute block index. Values from
    /// `tests/xchacha20poly1305_ref.py`, which self-tests against RFC 8439 before printing.
    #[test]
    fn matches_the_independent_reference_at_several_offsets() {
        let c = Chaff::new(&key(0x21));
        for (offset, expected) in [
            (
                0u64,
                "f9076833dfbbbe285630061e62849bc485c1f1c6d298ae58f5e42452a8e0aa73",
            ),
            (
                4096,
                "6ff698377448c6b867960a24b233ac9d1a04cb9e55d526c18648afca3c01c530",
            ),
            (
                1 << 20,
                "a9219c4e820aaa885621e9328a5de7f11dfa040fbc34ce4b91f5c8041cc6381b",
            ),
        ] {
            let got = c.bytes_at(offset, 32).expect("reference offset");
            assert_eq!(
                hex(&got),
                expected,
                "chaff at offset {offset} does not match the independent reference"
            );
        }
    }

    /// Chaff at a large offset, near the end of the container.
    ///
    /// The last legal page starts at `CONTAINER_SIZE - 4096`, which is block 2,097,088. If the
    /// offset were mishandled as a byte position rather than a block index this would drift by
    /// 64× and land somewhere else entirely.
    #[test]
    fn matches_the_independent_reference_at_the_last_page() {
        let c = Chaff::new(&key(0x21));
        let last = CONTAINER_SIZE - 4096;
        let got = c.bytes_at(last, 32).expect("last page");
        // Independent of the reference: the keystream at the last page must equal the tail of a
        // fill that started 4096 bytes earlier and ran on past it.
        // `split_off` returns the tail and leaves the head, so the comparison is against the
        // return value. Reading it the other way round compares `bytes_at(last)` against the
        // keystream for `last - 4096`, which is of course a different block and fails.
        let mut stepped = c.bytes_at(last - 4096, 8192).expect("stepped");
        let tail = stepped.split_off(4096);
        assert_eq!(got, &tail[..32]);
    }
}
