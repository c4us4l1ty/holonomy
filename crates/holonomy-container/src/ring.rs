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
use crate::ContainerError;

/// **`SecureBlock`'s allocation failure, as this crate's error type.**
///
/// **Deliberately opaque, and the same reason [`AeadError`] is one.** `SecureBlockError` distinguishes
/// "the page lock was refused" from "the mapping failed" from "the registry is full", and every one
/// of those has the same remedy here — the container does not open — so forwarding the detail would
/// give a caller three ways to learn something about the host's limits for no benefit.
fn map_secure(_e: holonomy_secure::SecureBlockError) -> ContainerError {
    ContainerError::SecureBlock
}

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
    /// Every slot holds an edit that has not been written back yet.
    ///
    /// **Its own variant rather than a discarded edit.** The ring has three slots, and a slot
    /// holding an uncommitted edit cannot be reused without writing that edit out first. Three
    /// uncommitted edits at once is the only way to get here, and the answer is to commit — not
    /// to drop one of them on the floor.
    AllSlotsPending,
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
            Self::AllSlotsPending => {
                f.write_str("all three ring slots hold edits that have not been written back")
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

/// Stack scratch for reading one chunk's trailing page.
///
/// `#[repr(align)]` rather than an [`AlignedBuf`] because the point is that it costs **nothing**:
/// a `Ring` is measured against FR-3.1's 192 KiB ceiling and a fourth heap buffer would break that
/// number for the sake of a value that is 16 bytes of it. It lives on the caller's stack, is
/// never retained, and holds only bytes the file already exposes in the clear.
#[repr(align(4096))]
struct TagPage([u8; layout::IO_ALIGN as usize]);

/// The Poly1305 tag chunk `index` carries on disk right now — the seal the next one supersedes.
///
/// # Why this exists and why it is a read
///
/// `aead`'s nonce chains on the previous tag, so re-sealing needs that tag. It cannot be
/// cached across calls: `commit` is allowed to fail after sealing, and a cache updated before
/// the write lands would name a predecessor tag the file does not have, which is a nonce reuse
/// again. **Reading the authoritative value from the file is the only construction that cannot
/// desynchronise**, and it costs one 4 KiB `pread64` against a path that already writes 64 KiB.
///
/// # Why one page is enough, and why it is aligned
///
/// The tag is the last 16 bytes of a 64 KiB slot, so it is inside the slot's final
/// [`layout::IO_ALIGN`] page. `chunk_offset` is a multiple of [`CHUNK_SLOT`] (64 KiB) and
/// `CHUNK_SLOT` is a multiple of `IO_ALIGN` (4 KiB), so that page starts at an aligned offset
/// and the transfer satisfies `O_DIRECT` exactly as a full-slot transfer would.
///
/// # What it returns for an unwritten region
///
/// The bytes are whatever is there — chaff, on a container that has been created but whose
/// payload has not reached this index yet. That is the right answer: an unwritten slot has no
/// predecessor seal, and the chaff underneath is unpredictable, which is all the chaining term
/// requires.
pub fn prev_tag_from_disk(
    file: &DirectFile,
    omega: u64,
    index: u64,
) -> Result<[u8; layout::TAG_LEN], std::io::Error> {
    let mut page = TagPage([0u8; layout::IO_ALIGN as usize]);
    let base = layout::chunk_offset(omega, index);
    let tail = base + CHUNK_SLOT - layout::IO_ALIGN;
    file.read_slice_at(tail, &mut page.0)?;
    let mut tag = [0u8; layout::TAG_LEN];
    let start = (layout::IO_ALIGN - layout::TAG_LEN as u64) as usize;
    tag.copy_from_slice(&page.0[start..]);
    Ok(tag)
}

/// A three-slot window over the chunk stream.
///
/// # The slot invariant
///
/// **Every method that touches a slot's bytes updates its two flags, and nothing else may.**
/// The invariant is:
///
/// | `dirty` | `resident` | the slot holds |
/// |---|---|---|
/// | `false` | `Some(i)` | the **plaintext** of chunk `i` |
/// | `true` | `Some(i)` | the **edited** plaintext of chunk `i`, not yet written |
/// | `false` | `None` | nothing — stale bytes, ciphertext, or zeroes |
///
/// A dirty slot **always** has a resident index, because "which chunk is this edit for?" is
/// the question a write-back has to answer, and a dirty slot with no index is an edit that
/// cannot be written and therefore cannot be kept.
///
/// **The whole point of the table is that no row is reachable by accident.** The bug this
/// fixes was a slot that was neither: `advance` and `retreat` cleared `dirty` and `resident`
/// together when they evicted a slot, so an edit that had not been written back was
/// *silently discarded* — the bytes were still in the slot, but nothing would ever write
/// them, and nothing would ever say they were missing. `stage_blank` did the same thing to
/// all three slots at once. **The user would type, the file would save, and the edit would
/// not be in it.**
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
    ///
    /// # FR-4.8: the slots are page-locked, and that is the reason this returns a `Result`
    ///
    /// The three 64 KiB slots are the only structure in this crate that holds document plaintext
    /// for longer than one call, and FR-4.8 names them: *"3-stage **page-locked** ring buffer"*.
    /// Before this they were `alloc_zeroed` — 16-byte aligned, on the heap, swap-eligible,
    /// dump-eligible, and bounded by nothing.
    ///
    /// **It falls over rather than degrading.** The obvious alternative is to page-lock what can be
    /// locked and leave the rest, which is what `mlock` failing under a small `RLIMIT_MEMLOCK`
    /// looks like. That would be a caller running the weaker of two protections while believing it
    /// ran the stronger — **so the error is `ContainerError::SecureBlock` and the container does
    /// not open.** The usual cause is `RLIMIT_MEMLOCK`, which `main.rs` prints at boot and
    /// `the_reported_memlock_ceiling_is_the_real_one` pins against a live `getrlimit`.
    pub fn new(chunks: u64) -> Result<Self, ContainerError> {
        let slots = [
            AlignedBuf::page_locked(CHUNK_SLOT as usize).map_err(map_secure)?,
            AlignedBuf::page_locked(CHUNK_SLOT as usize).map_err(map_secure)?,
            AlignedBuf::page_locked(CHUNK_SLOT as usize).map_err(map_secure)?,
        ];
        Ok(Self {
            slots,
            resident: [None; RING_STAGES],
            center_slot: 1,
            center: 0,
            chunks,
            dirty: [false; RING_STAGES],
        })
    }

    /// **Whether all three slots are page-locked.** FR-4.8, assertable rather than assumed.
    ///
    /// The gate `the_ring_buffer_is_page_locked` reads this. **It is a method rather than a
    /// structural fact** because the backing is a runtime choice, and a requirement that cannot be
    /// asked about is a requirement nobody can check.
    pub fn slots_are_page_locked(&self) -> bool {
        self.slots.iter().all(AlignedBuf::is_page_locked)
    }

    /// **Whether every slot's address satisfies `O_DIRECT`'s buffer-alignment rule.**
    ///
    /// **The companion to [`Self::slots_are_page_locked`] and the cost of changing the backing.**
    /// `alloc_zeroed` with an explicit 4096 alignment and an `mmap`ed `SecureBlock` both produce
    /// page-aligned memory, so the two are interchangeable in this respect — but they are reachable
    /// by different routes, and `O_DIRECT` answers `EINVAL` with a message that says nothing about
    /// alignment. `DirectFile` re-checks the *offset* on every call for the same reason; this is the
    /// buffer half.
    pub fn slots_are_direct_io_aligned(&self) -> bool {
        self.slots.iter().all(AlignedBuf::is_aligned)
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
        // **A slot that holds an uncommitted edit is written back before it is reused.**
        //
        // The ring has three slots and a document being edited needs all three, so the slot a
        // prefetch wants is often the slot an edit is sitting in. Overwriting it would drop the
        // edit on the floor — the bytes would still be in the slot, but nothing would ever write
        // them and nothing would ever say they were missing. **Writing them out first costs one
        // 64 KiB `pwrite` and turns a silent data loss into a file that is correct.**
        //
        // The write can fail, and when it does the error propagates and the edit is *still*
        // there: `flush_slot` clears the flags only once the bytes are on the disk.
        if self.dirty[slot] {
            self.flush_slot(slot, file, omega, k_enc, n_root)?;
        }
        // Read straight into the slot: no bounce buffer, because the slot is already
        // 4096-aligned and 65,536 bytes long. This is the reuse that keeps the ring at
        // three buffers instead of four.
        //
        // **The flags are dropped *before* the read, not after.** The slot is about to hold
        // something else, so it holds nothing that any other method may read as a chunk: a
        // failed authentication below would otherwise leave a slot that claims to hold chunk
        // `index` as plaintext while holding the ciphertext that failed to authenticate — and
        // `neighbour` would hand that ciphertext back as document text.
        self.resident[slot] = None;
        self.dirty[slot] = false;
        file.read_exact_at(layout::chunk_offset(omega, index), &mut self.slots[slot])?;
        aead::open_chunk(k_enc, n_root, index, self.slots[slot].as_mut_slice())?;
        self.resident[slot] = Some(index);
        Ok(())
    }

    /// Make `index` the centre with blank plaintext, **without reading it from disk**.
    ///
    /// Needed when growing a document: a chunk past the old end has no ciphertext to decrypt,
    /// so seeking to it would fail authentication against whatever chaff happens to be there.
    /// Every slot is dropped rather than rotated, because after a growth the on-disk contents
    /// of the neighbouring slots are no longer a useful prefetch.
    ///
    /// # A slot holding an uncommitted edit is *not* dropped
    ///
    /// **This used to wipe all three slots and clear both flags on each of them**, which meant a
    /// staged edit that had not been written back was silently destroyed — the ring would go on
    /// to write a *different* chunk and report success, and the edit would simply not be in the
    /// file. The user would type, the file would save, and the edit would not be in it.
    ///
    /// **So a slot with an uncommitted edit keeps both of its flags and its bytes**, and the new
    /// centre is a slot that has nothing pending in it. The pending edit stays pending: it is
    /// still owed to the disk, it is still written by the next [`commit`](Self::commit), and if
    /// the ring wants that slot's memory for a prefetch it writes the edit out first (see
    /// [`load_slot`](Self::load_slot)).
    pub fn stage_blank(&mut self, index: u64) -> Result<(), RingError> {
        self.check_range(index)?;
        // **The centre must be a slot with nothing pending in it.** A dirty slot holds an edit
        // that has not reached the disk; reusing it would destroy that edit.
        let Some(slot) = (0..RING_STAGES).find(|&s| !self.dirty[s]) else {
            return Err(RingError::AllSlotsPending);
        };
        self.slots[slot].wipe();
        self.resident[slot] = None;
        self.dirty[slot] = false;
        self.center_slot = slot;
        self.center = index;
        self.resident[slot] = Some(index);
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
    ///
    /// **A slot with an uncommitted edit is not freed.** It keeps its `dirty` flag, its resident
    /// index and its bytes, so the edit is still owed to the disk and is still written by the
    /// next [`commit`](Self::commit). **This used to clear both flags unconditionally**, which
    /// discarded the edit: the bytes stayed in the slot and nothing would ever write them.
    pub fn advance(&mut self) {
        let prev = (self.center_slot + RING_STAGES - 1) % RING_STAGES;
        if !self.dirty[prev] {
            self.resident[prev] = None;
        }
        self.center_slot = (self.center_slot + 1) % RING_STAGES;
        self.center += 1;
    }

    /// Rotate the ring one chunk backward. The slot holding `center+1` becomes free.
    ///
    /// **The same rule as [`advance`](Self::advance): an uncommitted edit is not freed.**
    pub fn retreat(&mut self) {
        let next = (self.center_slot + 1) % RING_STAGES;
        if !self.dirty[next] {
            self.resident[next] = None;
        }
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
    ///
    /// # The centre must be resident, and that is the caller's job
    ///
    /// A slot that is not resident holds ciphertext (after a commit) or stale bytes, and
    /// editing either produces a chunk that decrypts perfectly to garbage. **The ring cannot
    /// check this** — it has no way to know what the caller meant — so it asserts it in debug
    /// builds and the callers honour it: `seek` loads the centre, `stage_blank` and
    /// `stage_plaintext` install it, and nothing else calls this.
    pub fn center_bytes_mut(&mut self) -> &mut [u8] {
        debug_assert!(
            self.resident[self.center_slot] == Some(self.center),
            "the centre slot does not hold chunk {} as plaintext; editing it would write garbage",
            self.center
        );
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

    /// **How many chunks have edits that have not been written back.**
    ///
    /// **Allocation-free, because the ring's whole budget is 192 KiB and nothing in it may
    /// allocate after construction.** The count is enough to answer the question the ring
    /// exists to keep honest — "is there an edit that is still owed to the disk?" — and it
    /// distinguishes *one* pending edit from *two*, which `has_dirty` cannot.
    pub fn pending_count(&self) -> usize {
        self.dirty.iter().filter(|&&d| d).count()
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
            if self.flush_slot(slot, file, omega, k_enc, n_root)? {
                written += 1;
            }
        }
        if written > 0 {
            file.sync()?;
        }
        Ok(written)
    }

    /// **Seal one dirty slot and write it to disk.** Returns whether bytes reached the disk.
    ///
    /// # Why this is a separate function
    ///
    /// Because "write a dirty slot" is now needed in two places: by [`commit`](Self::commit), which
    /// writes them all, and by [`load_slot`](Self::load_slot), which has to write one before it
    /// reuses the slot's memory. **Two copies of a seal-and-write is two chances to get the
    /// ordering wrong**, and the ordering is the whole safety property: see below.
    ///
    /// # The ordering, which is the safety property
    ///
    /// The plaintext is already in `slot[..CHUNK_PLAINTEXT]`; sealing in place avoids a fourth
    /// 64 KiB buffer, which would break the 192 KiB ceiling.
    ///
    /// **The predecessor tag is read from the file, here, and not before the loop.**
    ///
    /// The slot's own tag region is not a source: a dirty slot reached by
    /// `center_bytes_mut` still carries the tag `load_slot` left there, but a dirty slot
    /// reached by `stage_plaintext` went through `stage_blank`, which wipes. Reading one
    /// and assuming the other is the kind of reasoning that is right until the day it is
    /// not. One `pread64` of 4 KiB against a 64 KiB write, taken from the authoritative
    /// copy, is what makes the nonce un-repeatable even across a failed write and a retry.
    ///
    /// **The slot stops being resident only when the write lands.**
    ///
    /// Sealing happens in place, so between `seal_in_place` and a successful write the slot
    /// holds ciphertext. On success both flags are cleared and the next read re-fetches from
    /// disk. On failure the ciphertext is opened back into plaintext *in the same slot* and
    /// the dirty/resident flags are kept, so the edit survives for a retry. If recovery ever
    /// fails, the slot is wiped and both flags are dropped rather than leaving sealed bytes
    /// that a later commit could re-seal as document text.
    ///
    /// **A failure before the seal leaves the edit exactly where it was.** The predecessor
    /// tag is read first, so a closed descriptor fails there — with the plaintext still in
    /// the slot and the slot still dirty, which is the state a retry wants.
    fn flush_slot(
        &mut self,
        slot: usize,
        file: &DirectFile,
        omega: u64,
        k_enc: &[u8; 32],
        n_root: &[u8; 24],
    ) -> Result<bool, RingError> {
        let Some(index) = self.resident[slot] else {
            // **A dirty slot with no index cannot be written.** It cannot happen under the
            // invariant, and the answer if it does is to drop the flag rather than leave a
            // slot that a later commit would seal as somebody else's plaintext.
            self.dirty[slot] = false;
            return Ok(false);
        };
        let prev_tag = prev_tag_from_disk(file, omega, index)?;
        aead::seal_in_place(
            k_enc,
            n_root,
            index,
            &prev_tag,
            CHUNK_PLAINTEXT,
            self.slots[slot].as_mut_slice(),
        )?;
        if let Err(e) = file.write_exact_at(layout::chunk_offset(omega, index), &self.slots[slot]) {
            // **A failed write must not discard the edit.** The slot still holds the exact
            // ciphertext we just produced, so `open_chunk` can decrypt it back into the same
            // plaintext in place -- no fourth 64 KiB buffer, and the dirty/resident flags stay
            // set for a retry. If that recovery ever fails, wipe and drop the flags rather than
            // leave a slot that a later commit could re-seal as document text.
            if aead::open_chunk(k_enc, n_root, index, self.slots[slot].as_mut_slice()).is_err() {
                self.slots[slot].wipe();
                self.dirty[slot] = false;
                self.resident[slot] = None;
            }
            return Err(e.into());
        }
        self.dirty[slot] = false;
        self.resident[slot] = None;
        Ok(true)
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
