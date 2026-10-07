//! The 3-stage locked ring: `[N−1, N, N+1]`, exactly 192 KiB.
//!
//! FR-3.1 and PRD's "192 KiB 3-stage ring buffer". This is the whole reason the container
//! is never loaded into RAM: the reader keeps three chunk buffers and seeks, rather than
//! mapping or buffering 128 MiB.
//!
//! # Slot discipline
//!
//! Three slots, and slot `i` holds chunk `center + (i as isize - 1)`: slot 0 is the
//! previous chunk, slot 1 is the centre, slot 2 is the next. Advancing rotates the array by
//! one, so the slot that held `N-1` becomes the free slot for `N+2`. That is the whole
//! eviction policy, and it is deliberately a rotation rather than a search: the access
//! pattern is strictly sequential, so anything cleverer would be dead weight in the one
//! data path that must never allocate.
//!
//! # Nothing here allocates after construction
//!
//! [`Ring::new`] allocates the three [`AlignedBuf`]s and nothing else ever allocates.
//! Steady-state resident memory is therefore [`crate::layout::RING_BYTES`] = 196,608 bytes,
//! plus the caller's I/O bounce buffer if it has one -- which is why the ring reuses slot 1
//! as the read target rather than needing a fourth buffer.
//!
//! # Writes are read-modify-seal-write
//!
//! Because chunks are independently authenticated, editing chunk `N` means decrypting it,
//! changing the plaintext, re-sealing with `N`'s own nonce, and writing back only that slot.
//! A dirty slot is tracked so a `commit` writes exactly what changed and not 192 KiB per
//! keystroke. FR-3.4's dirty-region discipline is about pixels, but the same reasoning
//! applies here.

use crate::aead::{self, AeadError};
use crate::io::{AlignedBuf, DirectFile};
use crate::layout::{self, CHUNK_PLAINTEXT, CHUNK_SLOT, RING_STAGES};

/// Why a ring operation failed.
#[derive(Debug)]
pub enum RingError {
    /// The file could not be read or written.
    Io(std::io::Error),
    /// A chunk failed authentication: wrong key, wrong index, or corruption.
    Aead(AeadError),
    /// The requested chunk is not in the window and cannot be produced by advancing.
    NotResident {
        /// What was asked for.
        wanted: u64,
        /// The current centre.
        center: u64,
    },
    /// The requested chunk does not exist in this container.
    OutOfRange {
        /// What was asked for.
        index: u64,
        /// How many chunks exist.
        chunks: u64,
    },
}

impl core::fmt::Display for RingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "ring i/o: {e}"),
            Self::Aead(e) => write!(f, "ring crypto: {e}"),
            Self::NotResident { wanted, center } => {
                write!(f, "chunk {wanted} is not resident (centre {center})")
            }
            Self::OutOfRange { index, chunks } => {
                write!(f, "chunk {index} out of range for {chunks} chunks")
            }
        }
    }
}

impl std::error::Error for RingError {}

impl From<std::io::Error> for RingError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<AeadError> for RingError {
    fn from(e: AeadError) -> Self {
        Self::Aead(e)
    }
}

/// A three-slot window over the chunk stream.
pub struct Ring {
    /// Exactly three, indexed 0..2 as described in the module docs. A `Vec` would allow the
    /// size to drift; an array cannot.
    slots: [AlignedBuf; RING_STAGES],
    /// Chunk index each slot holds, `None` when free.
    resident: [Option<u64>; RING_STAGES],
    /// Which slot is the centre.
    center_slot: usize,
    /// Chunk index the centre slot is meant to hold.
    center: u64,
    /// Total chunks in the container, for bounds checking.
    pub(crate) chunks: u64,
    /// Slots whose plaintext has changed since load and must be re-sealed.
    dirty: [bool; RING_STAGES],
}

impl Ring {
    /// Allocate the three slots. This is the only allocation the ring ever does.
    pub fn new(chunks: u64) -> Self {
        let slots = [
            AlignedBuf::zeroed(CHUNK_SLOT as usize),
            AlignedBuf::zeroed(CHUNK_SLOT as usize),
            AlignedBuf::zeroed(CHUNK_SLOT as usize),
        ];
        Self {
            slots,
            resident: [None; RING_STAGES],
            center_slot: 1,
            center: 0,
            chunks,
            dirty: [false; RING_STAGES],
        }
    }

    /// Steady-state resident bytes: exactly 192 KiB.
    pub fn resident_bytes(&self) -> usize {
        layout::RING_BYTES
    }

    /// The chunk index the centre slot holds.
    pub fn center(&self) -> u64 {
        self.center
    }

    /// Total chunks this ring knows about.
    pub fn chunks(&self) -> u64 {
        self.chunks
    }

    fn check_range(&self, index: u64) -> Result<(), RingError> {
        if index >= self.chunks {
            return Err(RingError::OutOfRange {
                index,
                chunks: self.chunks,
            });
        }
        Ok(())
    }

    /// Centre the ring on `index`, loading it if necessary, and prefetch its neighbours.
    ///
    /// Neighbours are best-effort: chunk 0 has no predecessor and the last chunk has no
    /// successor, so a failure to prefetch the *ends* of the container is not an error. A
    /// failure to read an interior neighbour *is* reported, because that means the disk or
    /// the key is wrong rather than the document being short.
    pub fn seek(
        &mut self,
        file: &DirectFile,
        omega: u64,
        k_enc: &[u8; 32],
        n_root: &[u8; 24],
        index: u64,
    ) -> Result<(), RingError> {
        self.check_range(index)?;
        // Rotating is cheap and keeps the invariant simple: put `index` in the centre by
        // advancing, then loading whatever ended up free.
        while self.center < index {
            self.advance();
        }
        while self.center > index {
            self.retreat();
        }
        self.ensure_center_loaded(file, omega, k_enc, n_root)?;
        self.prefetch_neighbours(file, omega, k_enc, n_root)?;
        Ok(())
    }

    fn ensure_center_loaded(
        &mut self,
        file: &DirectFile,
        omega: u64,
        k_enc: &[u8; 32],
        n_root: &[u8; 24],
    ) -> Result<(), RingError> {
        if self.resident[self.center_slot] == Some(self.center) {
            return Ok(());
        }
        self.load_slot(self.center_slot, self.center, file, omega, k_enc, n_root)
    }

    fn prefetch_neighbours(
        &mut self,
        file: &DirectFile,
        omega: u64,
        k_enc: &[u8; 32],
        n_root: &[u8; 24],
    ) -> Result<(), RingError> {
        // Previous chunk occupies the slot one behind the centre in ring order.
        let prev_slot = (self.center_slot + RING_STAGES - 1) % RING_STAGES;
        if let Some(prev) = self.center.checked_sub(1) {
            if prev < self.chunks && self.resident[prev_slot] != Some(prev) {
                self.load_slot(prev_slot, prev, file, omega, k_enc, n_root)?;
            }
        }
        // Next chunk occupies the slot one ahead.
        let next_slot = (self.center_slot + 1) % RING_STAGES;
        let next = self.center + 1;
        if next < self.chunks && self.resident[next_slot] != Some(next) {
            self.load_slot(next_slot, next, file, omega, k_enc, n_root)?;
        }
        Ok(())
    }

    fn load_slot(
        &mut self,
        slot: usize,
        index: u64,
        file: &DirectFile,
        omega: u64,
        k_enc: &[u8; 32],
        n_root: &[u8; 24],
    ) -> Result<(), RingError> {
        self.check_range(index)?;
        // Read straight into the slot: no bounce buffer, because the slot is already
        // 4096-aligned and 65,536 bytes long. This is the reuse that keeps the ring at
        // three buffers instead of four.
        file.read_exact_at(layout::chunk_offset(omega, index), &mut self.slots[slot])?;
        aead::open_chunk(k_enc, n_root, index, self.slots[slot].as_mut_slice())?;
        self.resident[slot] = Some(index);
        self.dirty[slot] = false;
        Ok(())
    }

    /// Make `index` the centre with blank plaintext, **without reading it from disk**.
    ///
    /// Needed when growing a document: a chunk past the old end has no ciphertext to decrypt,
    /// so seeking to it would fail authentication against whatever chaff happens to be there.
    /// Every slot is dropped rather than rotated, because after a growth the on-disk contents
    /// of the neighbouring slots are no longer a useful prefetch.
    pub fn stage_blank(&mut self, index: u64) -> Result<(), RingError> {
        self.check_range(index)?;
        for slot in 0..RING_STAGES {
            self.slots[slot].wipe();
            self.resident[slot] = None;
            self.dirty[slot] = false;
        }
        self.center_slot = 1;
        self.center = index;
        self.resident[self.center_slot] = Some(index);
        Ok(())
    }

    /// Make `index` the centre holding `plaintext`, **and mark it dirty so a [`commit`](Self::commit)
/// writes it**.
///
/// # Why this exists, and why `stage_blank` could not do it
///
/// `stage_blank` deliberately leaves `dirty` false, because a blank chunk staged during a growth has not
/// been given content yet — committing it would write zeroes over the payload. **But a chunk whose
/// plaintext has been *modified* needs the opposite: marked dirty, so that a commit seals and writes it.**
/// There was no way to express that, which is why Phase 13 part 8's write-back had nowhere to go.
///
/// ## The dirty flag is set even for bytes identical to what is on disk
///
/// A caller that cannot cheaply tell the difference would otherwise have to compute one, and a caller that
/// *can* is expected to skip this call instead. **Writing an unchanged chunk costs one re-encrypt and one
/// `pwrite`, and is still correct** — the nonce is derived from `index`, not from a counter, so re-sealing
/// is idempotent in content even though the ciphertext bytes differ. Correctness does not depend on the
/// caller getting this right; only cost does.
pub fn stage_plaintext(&mut self, index: u64, plaintext: &[u8]) -> Result<(), RingError> {
    if plaintext.len() > layout::CHUNK_PLAINTEXT {
        // **`PlaintextTooLarge`, not `OutOfRange`.** The two failure modes here are independent -- a chunk
        // index past the end, and a payload longer than a chunk -- and conflating them reports the wrong
        // cause for a bug that is trivial to fix if it is named correctly and maddening if it is not.
        // `check_range` inside `stage_blank` handles the index case with its own error.
        return Err(RingError::Aead(AeadError::PlaintextTooLarge {
            len: plaintext.len(),
        }));
    }
    self.stage_blank(index)?;
    let slot = self.center_slot;
    self.slots[slot].as_mut_slice()[..plaintext.len()].copy_from_slice(plaintext);
    self.dirty[slot] = true;
    Ok(())
}

/// Rotate the ring one chunk forward. The slot holding `center-1` becomes free.
    pub fn advance(&mut self) {
        let prev = (self.center_slot + RING_STAGES - 1) % RING_STAGES;
        self.resident[prev] = None;
        self.dirty[prev] = false;
        self.center_slot = (self.center_slot + 1) % RING_STAGES;
        self.center += 1;
    }

    /// Rotate the ring one chunk backward. The slot holding `center+1` becomes free.
    pub fn retreat(&mut self) {
        let next = (self.center_slot + 1) % RING_STAGES;
        self.resident[next] = None;
        self.dirty[next] = false;
        self.center_slot = (self.center_slot + RING_STAGES - 1) % RING_STAGES;
        self.center = self.center.saturating_sub(1);
    }

    /// Decrypted plaintext of the centre chunk. Length is always [`CHUNK_PLAINTEXT`];
    /// [`crate::frame::MasterFrame::content_len`] says how much of it is real.
    pub fn center_bytes(&self) -> &[u8] {
        &self.slots[self.center_slot].as_slice()[..CHUNK_PLAINTEXT]
    }

    /// Mutable plaintext of the centre chunk. Marks the slot dirty, so a later
    /// [`commit`](Self::commit) will write it back.
    pub fn center_bytes_mut(&mut self) -> &mut [u8] {
        self.dirty[self.center_slot] = true;
        &mut self.slots[self.center_slot].as_mut_slice()[..CHUNK_PLAINTEXT]
    }

    /// A resident neighbour's plaintext, for the prefetch hit path.
    pub fn neighbour(&self, delta: i64) -> Option<&[u8]> {
        let slot = match delta {
            -1 => (self.center_slot + RING_STAGES - 1) % RING_STAGES,
            1 => (self.center_slot + 1) % RING_STAGES,
            _ => return None,
        };
        let expected = if delta < 0 {
            self.center.checked_sub(1)?
        } else {
            self.center.checked_add(1)?
        };
        if self.resident[slot] != Some(expected) {
            return None;
        }
        Some(&self.slots[slot].as_slice()[..CHUNK_PLAINTEXT])
    }

    /// True if any slot has unwritten changes.
    pub fn has_dirty(&self) -> bool {
        self.dirty.iter().any(|&d| d)
    }

    /// Re-seal and write back every dirty slot, then sync.
    ///
    /// Only dirty slots are written. On a keystroke that means one 65,536-byte
    /// `O_DIRECT` write instead of three, and on an unmodified open, none.
    pub fn commit(
        &mut self,
        file: &DirectFile,
        omega: u64,
        k_enc: &[u8; 32],
        n_root: &[u8; 24],
    ) -> Result<usize, RingError> {
        let mut written = 0;
        for slot in 0..RING_STAGES {
            if !self.dirty[slot] {
                continue;
            }
            let Some(index) = self.resident[slot] else {
                continue;
            };
            // The plaintext is already in `slot[..CHUNK_PLAINTEXT]`; sealing in place
            // avoids a fourth 64 KiB buffer, which would break the 192 KiB ceiling.
            aead::seal_in_place(
                k_enc,
                n_root,
                index,
                CHUNK_PLAINTEXT,
                self.slots[slot].as_mut_slice(),
            )?;
            file.write_exact_at(layout::chunk_offset(omega, index), &self.slots[slot])?;
            self.dirty[slot] = false;
            // **The slot is no longer resident.** Sealing happened in place, so the slot now
            // holds ciphertext while `resident` still claims it holds chunk `index` as plaintext.
            //
            // That was a real bug, found by the Phase 7 census workload rather than by the
            // container's own tests: `ensure_center_loaded` returns early when
            // `resident[slot] == Some(center)`, so a `read_content()` after a `commit()` handed
            // back the sealed bytes verbatim. The Phase 3 tests never hit it because they read
            // through a freshly opened handle, whose ring is empty; the Phase 8 session hits it on
            // every autosave-then-export, which is the single most common sequence there is.
            //
            // Forgotten rather than decrypted-back: there is no cheaper way to know whether the
            // slot was the one just sealed, and re-reading costs one `pread64` for a chunk the
            // caller is about to read anyway.
            self.resident[slot] = None;
            written += 1;
        }
        if written > 0 {
            file.sync()?;
        }
        Ok(written)
    }

    /// Zero all three slots. Called before the ring is dropped so plaintext does not sit
    /// in a freed heap page waiting to be swapped.
    pub fn wipe(&mut self) {
        for slot in self.slots.iter_mut() {
            slot.wipe();
        }
        self.resident = [None; RING_STAGES];
        self.dirty = [false; RING_STAGES];
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        self.wipe();
    }
}

impl core::fmt::Debug for Ring {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ring")
            .field("center", &self.center)
            .field("chunks", &self.chunks)
            .field("resident", &self.resident)
            .field("dirty", &self.dirty)
            .field("resident_bytes", &self.resident_bytes())
            .finish()
    }
}
