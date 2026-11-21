//! Append-only Merkle-mountain-range accumulator over public segment-record metadata.

use holonomy_seal::metadata_digest;

use crate::LineError;

const HEIGHTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mmr {
    count: u64,
    peaks: [Option<[u8; 32]>; HEIGHTS],
}

impl Mmr {
    pub(crate) fn new() -> Self {
        Self {
            count: 0,
            peaks: [None; HEIGHTS],
        }
    }

    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    pub(crate) fn append(&mut self, leaf: [u8; 32]) -> Result<(), LineError> {
        if self.count == u64::MAX {
            return Err(LineError::TooManySegments);
        }

        let mut carry = leaf;
        let mut bits = self.count;
        let mut height = 0;
        // D-59: the merge is computed into a scratch array and committed at the end. Taking peaks
        // out of `self` as the walk goes and returning early on a missing one — which is what the
        // `?` used to do — leaves the accumulator holding fewer peaks than its `count` claims, and
        // `root` and `encode_peaks` both `expect` a peak for every set bit: the next call panics
        // instead of reporting the inconsistency it inherited. The invariant is checked before
        // anything is written, and a broken one is an error, not a poisoned value.
        let mut merged = self.peaks;
        while bits & 1 == 1 {
            if height >= HEIGHTS {
                return Err(LineError::TooManySegments);
            }
            let left = merged[height].ok_or(LineError::InvalidRecord)?;
            carry = parent_hash(&left, &carry);
            merged[height] = None;
            bits >>= 1;
            height += 1;
        }
        if height >= HEIGHTS {
            return Err(LineError::TooManySegments);
        }
        merged[height] = Some(carry);
        self.peaks = merged;
        self.count += 1;
        Ok(())
    }

    pub(crate) fn root(&self) -> [u8; 32] {
        let mut encoded = Vec::with_capacity(8 + self.count.count_ones() as usize * 33);
        encoded.extend_from_slice(&self.count.to_le_bytes());
        for (height, peak) in self.peaks.iter().enumerate() {
            if self.count & (1u64 << height) != 0 {
                encoded.push(height as u8);
                encoded.extend_from_slice(peak.as_ref().expect("set MMR bit has a peak"));
            }
        }
        metadata_digest(b"LINE_MMR_ROOT", &encoded)
    }

    pub(crate) fn encode_peaks(&self, out: &mut Vec<u8>) {
        for (height, peak) in self.peaks.iter().enumerate() {
            if self.count & (1u64 << height) != 0 {
                out.extend_from_slice(peak.as_ref().expect("set MMR bit has a peak"));
            }
        }
    }

    pub(crate) fn from_peak_bytes(count: u64, bytes: &[u8]) -> Result<Self, LineError> {
        let expected = count.count_ones() as usize * 32;
        if bytes.len() != expected {
            return Err(LineError::InvalidRecord);
        }

        let mut peaks = [None; HEIGHTS];
        let mut offset = 0;
        for (height, slot) in peaks.iter_mut().enumerate() {
            if count & (1u64 << height) != 0 {
                let end = offset + 32;
                let peak: [u8; 32] = bytes
                    .get(offset..end)
                    .ok_or(LineError::InvalidRecord)?
                    .try_into()
                    .map_err(|_| LineError::InvalidRecord)?;
                *slot = Some(peak);
                offset = end;
            }
        }
        Ok(Self { count, peaks })
    }
}

pub(crate) fn segment_leaf(
    file_id: &[u8; 16],
    epoch: &[u8; 32],
    sequence: u64,
    nonce: &[u8; 24],
    ciphertext_len: u32,
    previous_tag: &[u8; 32],
    tag: &[u8; 32],
) -> [u8; 32] {
    let mut encoded = Vec::with_capacity(16 + 32 + 8 + 24 + 4 + 32 + 32);
    encoded.extend_from_slice(file_id);
    encoded.extend_from_slice(epoch);
    encoded.extend_from_slice(&sequence.to_le_bytes());
    encoded.extend_from_slice(nonce);
    encoded.extend_from_slice(&ciphertext_len.to_le_bytes());
    encoded.extend_from_slice(previous_tag);
    encoded.extend_from_slice(tag);
    metadata_digest(b"LINE_SEGMENT_LEAF", &encoded)
}

fn parent_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut pair = [0u8; 64];
    pair[..32].copy_from_slice(left);
    pair[32..].copy_from_slice(right);
    metadata_digest(b"LINE_MMR_NODE", &pair)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_order_and_count_are_committed() {
        let mut a = Mmr::new();
        let mut b = Mmr::new();
        for leaf in [[1u8; 32], [2u8; 32], [3u8; 32]] {
            a.append(leaf).unwrap();
        }
        for leaf in [[2u8; 32], [1u8; 32], [3u8; 32]] {
            b.append(leaf).unwrap();
        }
        assert_eq!(a.count(), 3);
        assert_ne!(a.root(), b.root());
    }

    #[test]
    fn a_refused_merge_leaves_the_accumulator_exactly_as_it_was() {
        // D-59: `append` used to take peaks out of `self` as its merge walk went and return early
        // when one was missing, so the accumulator kept a `count` its peaks no longer backed and
        // the *next* append merged against the holes. The merge is computed into a scratch copy
        // now, and a broken invariant leaves the accumulator exactly as it found it.
        let mut mmr = Mmr::new();
        for leaf in [[7u8; 32], [8u8; 32], [9u8; 32]] {
            mmr.append(leaf).unwrap();
        }
        let before = mmr.clone();
        let root_before = mmr.root();
        // Break the invariant the way a corrupt-but-authenticated witness would: three segments
        // are recorded, but the peak the merge would consume is gone.
        mmr.peaks[0] = None;
        assert!(
            matches!(mmr.append([1u8; 32]), Err(LineError::InvalidRecord)),
            "a merge that cannot find its peak is refused, not half-applied"
        );
        assert_eq!(
            mmr.count(),
            before.count(),
            "a refused append counts nothing"
        );
        assert_eq!(
            mmr.peaks, before.peaks,
            "and consumes no other peak: the merge is computed into a scratch copy"
        );
        assert_eq!(root_before, before.root());
    }

    #[test]
    fn witness_peaks_round_trip_for_binary_counts() {
        let mut mmr = Mmr::new();
        for n in 0..100 {
            let mut encoded = Vec::new();
            mmr.encode_peaks(&mut encoded);
            let decoded = Mmr::from_peak_bytes(mmr.count(), &encoded).unwrap();
            assert_eq!(decoded, mmr);
            assert_eq!(decoded.root(), mmr.root());
            mmr.append([n as u8; 32]).unwrap();
        }
    }
}
