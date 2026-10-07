//! A bounded set of resident document sections. Phase 13, item 3.
//!
//! # What this is
//!
//! **The thing that makes document length a function of the window rather than of the process.** It owns a
//! fixed *budget* of sections, loads a section from a [`Wavefunction`] on a miss, and on a budget
//! violation evicts the least recently used one with
//! [`SecureBlock::zeroize_and_release`] — synchronously, so resident memory falls before the next frame
//! and a gate can observe it. That is the rule 9C sets for evicted image rasters, applied to text.
//!
//! A section is [`SECTION_BYTES`] bytes and is one container chunk, so a miss is one authenticated
//! `pread64` plus one `open_chunk` ([`Wavefunction::read_chunk_into`], Phase 13 part 2 step 1).
//!
//! # The API shape, and why it fills a caller's buffer
//!
//! [`SectionStore::copy_into`] writes into a buffer the caller owns, and that is **the same shape as
//! [`holonomy_text::Editor::read_into`] and [`Wavefunction::read_chunk_into`]**, which is the reason to
//! choose it: a store that returned `&[u8]` into its own resident map would need interior mutability for a
//! hit, because touching the LRU tick is a write and the caller holds a borrow. `SecureBlock` is not
//! `Sync`-shareable and the tick is per-entry, so the alternatives are a lock on the read path or a raw
//! pointer. **A caller-supplied buffer is the option with no `unsafe` in it**, and the cost is one
//! `copy_from_slice` of a section — 65,520 bytes, which is what the read did anyway.
//!
//! # What this is *not*, and it is the important half of the doc
//!
//! **Nothing reads this yet.** `Editor` still holds the whole document in a rope of page-locked 4 KiB
//! leaves, so a session's residency is unchanged and the document is still entirely resident. This is the
//! *mechanism* — bounded, scrubbed, observable — built and gated on its own because it is the new and
//! risky part, and because wiring it in requires changing `Editor`'s core assumption (see the end).
//!
//! # The `mlock` finding, which changes the plan for item 4
//!
//! **Residency bounds `SecureBlock` allocations. It does not bound `RLIMIT_MEMLOCK`.** The boot chain calls
//! `Opened::lock_all_pages`, which is `mlockall(MCL_CURRENT | MCL_FUTURE)`
//! (`holonomy-jail/src/lib.rs:372`) — and `mlockall` locks **every page the process has ever mapped and
//! every page it maps later**, until `munlockall`.
//!
//! So on this host `RLIMIT_MEMLOCK = 8.00 MiB` is consumed by *the process's address space*, not by the
//! document: **a 6 MiB document costs the same `mlock` whether it is fully resident or one section is.**
//! Phase 13 item 4 — *"`RLIMIT_MEMLOCK` stops being the document-size ceiling"* — is therefore **not** a
//! consequence of item 3. It is a separate change, and the change is to **retire `mlockall` and rely on the
//! per-block `mlock` that `SecureBlock` already does**, reserving [`LockPolicy::Unlocked`] for data that is
//! derived and re-derivable from the encrypted store.
//!
//! **That is a security decision, not an optimisation, and it is not made here.** `mlockall` gives a blanket
//! "nothing in this process is ever swapped" guarantee; per-block locking gives the same guarantee for
//! every block that holds plaintext, which is what FR-1.2's threat model is about — and it is strictly
//! better for the ceiling, because unlocked derivations stop consuming the budget. What would have to be
//! audited alongside it is listed in PROJECT.md's Phase 13 part 2 notes.
//!
//! # Why eviction is LRU and not "farthest from the caret"
//!
//! **Because LRU is a property of this type and a policy is not.** "Farthest from the caret" needs a
//! position, so a caller would supply one, the store would have a field for it, and the budget would be
//! enforced against a *heuristic* — which fails open when the heuristic is wrong, leaving the store
//! unbounded. LRU needs nothing but the access sequence, so **the store cannot be made unbounded by
//! misconfiguration**: [`SectionStore::resident`] is checked before every insert, not hoped for.

use crate::manifest::SECTION_BYTES;
use holonomy_container::Wavefunction;
use std::collections::BTreeMap;
use holonomy_secure::{LockPolicy, SecureBlock};

/// Why a section could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// The container refused the read: no such chunk, damage, or a passphrase problem.
    ///
    /// **Deliberately not the container's own error type.** `ContainerError` distinguishes
    /// `AuthenticationFailed` from `Io`, and carrying it here would put a decryption oracle's worth of
    /// detail into a type a caller might log. FR-1.2's threat model treats "which failure" as sensitive, and
    /// the container's own `AeadError` docs say the same thing about itself.
    Read,
    /// The caller supplied an output buffer smaller than the section.
    ShortBuffer {
        /// The buffer's length.
        got: usize,
        /// The section's length.
        want: usize,
    },
}

/// One resident section.
struct Entry {
    /// The bytes. Page-locked, guard-bounded, and scrubbed by `zeroize_and_release` on eviction.
    block: SecureBlock,
    /// A monotonically increasing access tick, so recency is a `u64` compare rather than a list walk.
    ///
    /// **A tick rather than a linked list, because eviction is not the hot path.** An intrusive list would
    /// have to be unlinked on every *hit*, which is a write to shared structure on a read. The tick makes a
    /// hit one `u64` store into the entry already being looked up.
    used: u64,
    /// The section's length, which the last chunk of a document may be shorter than `SECTION_BYTES`.
    len: u32,
    /// Whether this section's cached bytes differ from what is on disk. Phase 13 part 8.
    ///
    /// **The dirty bit is the whole of write-back.** A dirty section is one the rope has edited, so its
    /// cached plaintext is newer than the file; an eviction has to re-seal and write it before the block
    /// goes, and a clean section does not. Without it, either every eviction re-encrypts (wasteful) or
    /// none does (the document silently reverts), and neither failure is visible in the returned bytes.
    dirty: bool,
}

/// What a store has done. Phase 13.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreStats {
    /// Sections read from the container.
    pub loads: u64,
    /// Sections served without a read.
    pub hits: u64,
    /// Sections evicted.
    pub evictions: u64,
    /// Bytes `zeroize_and_release` reported releasing, summed over every eviction.
    ///
    /// **The eviction evidence, and it is a count rather than a proof of scrubbing.** `zeroize_and_release`
    /// returns how many bytes it scrubbed and unmapped, so summing it and comparing against the store's own
    /// accounting shows eviction *released* memory rather than merely dropping a struct whose `Drop` was
    /// never reached. **The stronger claim — that the pages read back as zero — cannot be checked after
    /// `munmap`, because the mapping is gone.** That is `holonomy-secure`'s business and it is gated in
    /// `crates/holonomy-secure/tests/`; a test here that claimed otherwise would be asserting a property of
    /// a mapping that no longer exists.
    pub released_bytes: u64,
    /// Leaf writes back into the cached sections. Phase 13 part 8.
    pub writes: u64,
    /// Commits that reached the disk. Phase 13 part 8.
    ///
    /// **Separate from `writes` because they answer different questions.** `writes` counts leaf write-backs
    /// into memory; `commits` counts the expensive re-encrypt-and-write passes. A session that edits without
    /// ever evicting has many `writes` and no `commits` -- which is the whole design working, and a number
    /// that would be invisible if both were counted together.
    pub commits: u64,
}

/// A bounded set of resident document sections, loaded on demand.
///
/// The container is borrowed for `'c` rather than owned, so the store cannot outlive the file it reads —
/// and a caller cannot hold two stores against one container with independent budgets and no combined
/// bound. **One container, one store, one budget**, which is the only arrangement where *"resident text
/// stays bounded"* is a statement about the process rather than about a value.
pub struct SectionStore<'c> {
    container: &'c mut Wavefunction,
    /// Maximum resident sections. Enforced before every insert.
    budget: usize,
    /// Resident sections by index. A `BTreeMap` rather than a `Vec<Option<_>>` because the resident set is
    /// small and sparse relative to the document — 8 of 97 for the 6 MiB document §6 measures — and a
    /// `Vec` would be sized by the document rather than by the window, which is the thing being removed.
    /// **There is no `DEFAULT_BUDGET`: the number is a policy choice, not a property of the type**, and
    /// §2.9's memory budget is where it should be derived from rather than guessed here.
    resident: BTreeMap<u32, Entry>,
    /// The next access tick. `u64`, so it cannot wrap in any session that fits in a `u64` of accesses.
    tick: u64,
    /// Reused across loads, so a miss does not allocate a scratch buffer per load.
    scratch: Vec<u8>,
    /// The destination for `fetch_leaf`, kept separate from `scratch` because that method calls
    /// [`copy_into`](Self::copy_into), which needs `&mut self` *and* a destination it cannot share with
    /// the receiver's own fields. Taken out with `mem::take` for the call.
    ///
    /// **A second 65,520-byte buffer, deliberately.** The alternative -- reading straight into the leaf's
    /// destination in `out` -- only works when the leaf starts exactly on a section boundary, which is 1
    /// leaf in 17. So this is what the straddling case costs, and it is paid once and then reused.
    fetch_scratch: Vec<u8>,
    stats: StoreStats,
}

impl<'c> SectionStore<'c> {
    /// A store over `container` holding at most `budget` sections.
    ///
    /// **A budget of 0 is allowed and means "nothing stays resident"**, which is a legitimate configuration
    /// for measuring a load with no cache and is not silently promoted to 1. Every other value is what it
    /// says.
    pub fn new(container: &'c mut Wavefunction, budget: usize) -> Self {
        Self {
            container,
            budget,
            resident: BTreeMap::new(),
            tick: 0,
            scratch: Vec::new(),
            fetch_scratch: Vec::new(),
            stats: StoreStats::default(),
        }
    }

    /// The maximum resident sections.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// How many sections are resident now. **Never above `budget`.**
    pub fn resident(&self) -> usize {
        self.resident.len()
    }

    /// How many bytes of document text are resident now.
    pub fn resident_bytes(&self) -> usize {
        self.resident.values().map(|e| e.len as usize).sum()
    }

    /// What the store has done so far.
    pub fn stats(&self) -> StoreStats {
        self.stats
    }

    /// The fetch buffer's current length, or 0 if it has never been needed.
    ///
    /// **Exists to make "reused, not reallocated" observable.** `fetch_leaf` takes this buffer out of
    /// the store with `mem::take` and puts it back through a closure so that the error paths restore it
    /// too -- a claim that is easy to state and easy to break, because the naive version restores it on
    /// the success path only. A test that watches the length across many faults catches the regression
    /// that a test watching the returned bytes would not.
    pub fn fetch_buffer_len(&self) -> usize {
        self.fetch_scratch.len()
    }

    /// The container's **chunk index** for a section, or `None` if the document has no such section.
    ///
    /// **Section 0 is chunk 1, not chunk 0**, because chunk 0 is the master frame and
    /// [`Wavefunction::chunk_content_offset`] refuses index 0.
    ///
    /// # The first version of this returned a byte offset, and every read failed
    ///
    /// `Wavefunction::chunk_content_offset` answers *"where in the document's text does this chunk start"* —
    /// it is named `content_offset` and returns `(index - 1) * CHUNK_PLAINTEXT`. So `chunk_of` originally
    /// returned `Some(0)` for section 0, and that `0` was passed to `read_chunk_into` as a **chunk index**,
    /// which is chunk 0, which is the master frame, which is refused.
    ///
    /// **All seven gates failed with `StoreError::Read`**, which is the most opaque error this type has, and
    /// the symptom — every read of every section failing identically — is consistent with an off-by-one that
    /// never varies. The `Some(offset)` / `Some(index)` confusion is exactly the trap
    /// [`Wavefunction::chunk_content_offset`]'s own docs warn about, reintroduced one layer up by a function
    /// named for the other quantity. So the container is asked whether the chunk **exists**, and the
    /// **index** is what comes back.
    pub fn chunk_of(container: &Wavefunction, section: u32) -> Option<u64> {
        let index = section as u64 + 1;
        container.chunk_content_offset(index).map(|_| index)
    }

    /// Section `section`'s bytes, copied into `out`, and how many were written.
    ///
    /// # The eviction order, and why it runs *before* the insert
    ///
    /// A miss with the store already full must free a slot **before** allocating the new block, or the
    /// store peaks at `budget + 1` sections — and the peak is the number a gate should assert. Freeing
    /// afterwards is the classic way a bound becomes `budget + 1` and nobody notices, because `resident()`
    /// reads `budget` again by the time anyone looks.
    ///
    /// The victim is the least recently used by tick. Ticks are unique, so the order is total and a test
    /// can name the victim rather than asserting "something was evicted".
    ///
    /// # The consequence of that order, which is a cost and not a bug
    ///
    /// Evicting before allocating means **a failed `SecureBlock::allocate_with` costs the store a resident
    /// section**: the victim is already gone when the allocation is refused. That is the price of never
    /// exceeding the budget, and paying it is right — a store that peaks at `budget + 1` under memory
    /// pressure is exactly the store that is already failing. A short output buffer, by contrast, is checked
    /// **before** the eviction loop, because that is a caller error with no reason to disturb the store.
    pub fn copy_into(&mut self, section: u32, out: &mut [u8]) -> Result<usize, StoreError> {
        self.tick += 1;
        let tick = self.tick;

        if let Some(entry) = self.resident.get_mut(&section) {
            entry.used = tick;
            if out.len() < entry.len as usize {
                return Err(StoreError::ShortBuffer {
                    got: out.len(),
                    want: entry.len as usize,
                });
            }
            out[..entry.len as usize].copy_from_slice(&entry.block.as_slice());
            self.stats.hits += 1;
            return Ok(entry.len as usize);
        }

        let chunk = Self::chunk_of(self.container, section).ok_or(StoreError::Read)?;
        if self.scratch.len() < SECTION_BYTES {
            self.scratch.resize(SECTION_BYTES, 0);
        }
        let got = self
            .container
            .read_chunk_into(chunk, &mut self.scratch[..SECTION_BYTES])
            .map_err(|_| StoreError::Read)?;
        if out.len() < got {
            return Err(StoreError::ShortBuffer {
                got: out.len(),
                want: got,
            });
        }

        // --- Free a slot *before* allocating.
        while self.resident.len() >= self.budget {
            let Some(victim) = self.lru_victim() else {
                // Nothing resident and the budget is 0: there is no slot and no victim, so this store
                // holds nothing. Legal, and the reason `budget == 0` is documented rather than clamped.
                break;
            };
            debug_assert_ne!(victim, section, "the section being loaded is not resident");
            self.evict(victim);
        }
        if self.budget == 0 {
            // No slot will ever exist, so the bytes go straight to the caller and the store stays empty.
            out[..got].copy_from_slice(&self.scratch[..got]);
            self.stats.loads += 1;
            return Ok(got);
        }

        let mut block =
            SecureBlock::allocate_with(got, LockPolicy::PageLocked).map_err(|_| StoreError::Read)?;
        block.as_mut_slice().copy_from_slice(&self.scratch[..got]);
        self.resident.insert(
            section,
            Entry {
                block,
                used: tick,
                len: got as u32,
                // A freshly loaded section matches the file by definition. Marking it dirty would make
                // every read rewrite the container.
                dirty: false,
            },
        );
        self.stats.loads += 1;
        out[..got].copy_from_slice(&self.scratch[..got]);
        Ok(got)
    }

    /// Write `bytes` at document offset `into`, patching the cached sections that overlap.
///
/// # Why patching the cache is enough, and why that is the design
///
/// The invariant this store must keep is **`fetch_leaf` returns bytes as they are *now*, at *current*
/// offsets** — see [`LeafSource::store_leaf`](holonomy_text::LeafSource::store_leaf). Because the store is
/// addressed by the same offsets as the rope, **writing the leaf's current bytes at its current offset is
/// the whole of it**: no second coordinate system, no pending-edit overlay, no replay. That is why
/// part 8 needs none of what part 7's option B was going to cost.
///
/// The bytes go into whichever **resident** sections overlap the range and are marked dirty, so an
/// eviction re-seals them. A section that is *not* resident is not patched and not marked — it is not in
/// this store's hands, and the rope still holds the leaf, so the next fault will read the section and the
/// leaf's bytes will come back correct. **Patching a section would mean loading it, which would mean
/// spending a resident slot and a decrypt on a write-back** — and it is unnecessary, because an absent
/// section's on-disk copy is only consulted after the rope has given up the leaf.
///
/// ## A leaf that straddles a section boundary writes to both
///
/// `65,520 / 2,048 = 32` leaves per section, so a leaf straddles in 1 case in 32. Both sides are patched
/// and both are marked dirty, or the boundary leaf comes back half-old — and half-old is exactly the shape
/// of bug that survives a length check.
///
/// ## Refusals
///
/// A range past the end of the document is refused, because there is no section to hold it and a caller
/// that computed the offset wrong would otherwise get a silent no-op. An **empty** range is a legal no-op,
/// because a leaf that shrank to nothing still has to be recorded as having been evicted.
pub fn write_at(&mut self, into: usize, bytes: &[u8]) -> Result<(), StoreError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let doc_len = self.container.content_len() as usize;
    let end = into.checked_add(bytes.len()).ok_or(StoreError::Read)?;
    if end > doc_len {
        return Err(StoreError::Read);
    }

    let first = (into / SECTION_BYTES) as u32;
    let last = ((end - 1) / SECTION_BYTES) as u32;
    for section in first..=last {
        let sec_start = section as usize * SECTION_BYTES;
        // The overlap of [sec_start, sec_start+len) with [into, end).
        let lo = into.max(sec_start);
        let hi = end.min(sec_start + SECTION_BYTES);
        let Some(entry) = self.resident.get_mut(&section) else {
            continue;
        };
        let entry_len = entry.len as usize;
        // **Clamp to the section's real length, not to SECTION_BYTES.** The last section of a document is
        // shorter, and writing past it would either panic or, if it is padded, plant bytes outside the
        // document that a later read could surface.
        let hi = hi.min(sec_start + entry_len);
        if lo >= hi {
            continue;
        }
        let at = lo - sec_start;
        entry.block.as_mut_slice()[at..at + (hi - lo)].copy_from_slice(&bytes[lo - into..hi - into]);
        entry.dirty = true;
    }
    self.stats.writes += 1;
    Ok(())
}

/// Re-seal and write every dirty section, and return how many bytes reached the disk.
///
/// **The expensive call, and it is here rather than on the keystroke path on purpose.** Part 7 measured
/// write-through at **6.5–10.3× a keystroke**, 84–93 % of it the `O_DIRECT` write — so this must be driven
/// by *eviction and save*, which are bounded by the resident budget, and never by typing.
///
/// Only dirty sections are written, so a store that has been read but not edited commits nothing. That is
/// the difference between a commit that costs a millisecond and one that costs nothing, and it is why
/// `Entry::dirty` exists rather than a blanket rewrite.
pub fn commit_dirty(&mut self) -> Result<usize, StoreError> {
    let dirty: Vec<u32> = self.resident.iter().filter(|(_, e)| e.dirty).map(|(s, _)| *s).collect();
    let mut written = 0usize;
    for section in dirty {
        let Some(chunk) = Self::chunk_of(self.container, section) else {
            // The section is resident so its chunk exists; if it does not, refusing is right and
            // **leaving it dirty** is what makes the refusal recoverable -- a later commit retries.
            return Err(StoreError::Read);
        };
        // **Read the section's current bytes out, write them, and only then clear the flag.** The order is
        // the whole safety property: a write that fails leaves `dirty` set, so the bytes are still marked
        // as needing to reach the disk and a later commit retries. Clearing first would turn a failed
        // write into a silently lost edit -- the one failure this whole design exists to prevent.
        let mut buf = self.scratch.split_off(0);
        if buf.len() < SECTION_BYTES {
            buf.resize(SECTION_BYTES, 0);
        }
        let len = {
            let entry = self.resident.get(&section).expect("listed from resident");
            entry.len as usize
        };
        self.copy_into(section, &mut buf[..len])?;
        self.container
            .write_chunk(chunk, &buf[..len])
            .map_err(|_| StoreError::Read)?;
        // Zero the staging copy: it held document plaintext a moment ago and is about to be handed back.
        buf[..len].fill(0);
        self.scratch = buf;
        written += SECTION_BYTES.min(len);
        // Clear *after* the write succeeded.
        if let Some(entry) = self.resident.get_mut(&section) {
            entry.dirty = false;
        }
    }
    self.stats.commits += 1;
    Ok(written)
}

/// How many resident sections differ from disk.
pub fn dirty_sections(&self) -> usize {
    self.resident.values().filter(|e| e.dirty).count()
}

/// The resident section a load should evict: the oldest by tick.
    ///
    /// `None` only when nothing is resident, and the caller breaks out rather than spinning.
    fn lru_victim(&self) -> Option<u32> {
        self.resident
            .iter()
            .min_by_key(|(_, e)| e.used)
            .map(|(i, _)| *i)
    }

    /// Evict `section`, scrubbing and unmapping its memory synchronously.
    fn evict(&mut self, section: u32) {
        if let Some(mut entry) = self.resident.remove(&section) {
            let released = entry.block.zeroize_and_release();
            self.stats.evictions += 1;
            self.stats.released_bytes += released as u64;
        }
    }

    /// Evict everything, now.
    ///
    /// **Explicit, and not only in `Drop`.** `Drop` is the backstop for the paths nobody thought of; a
    /// caller that wants resident memory to fall *at a point* — before `commit()`, before a KDF, before a
    /// syscall whose allowlist may not include `munlock` — has to be able to say so, and cannot from a
    /// `Drop`. That is 9C's rule ("eviction must be synchronous and observable") applied to text.
    pub fn evict_all(&mut self) {
        let sections: Vec<u32> = self.resident.keys().copied().collect();
        for s in sections {
            self.evict(s);
        }
    }
}

impl Drop for SectionStore<'_> {
    fn drop(&mut self) {
        self.evict_all();
    }
}

// ---------------------------------------------------------------------------
// Opening a document for real.
// ---------------------------------------------------------------------------

/// Resident sections a session keeps by default.
///
/// **A policy number, not a property of the type**, and chosen to be small: four sections is 262,080
/// bytes of page-locked text, which is 3 % of this host's 8.00 MiB `RLIMIT_MEMLOCK` ceiling. A larger
/// budget is a faster scroll and more headroom; a smaller one is a smaller ceiling. §2.9.4 is where that
/// trade should be settled against the whole memory picture rather than here.
pub const DEFAULT_RESIDENT_SECTIONS: usize = 4;

/// A document opened from a container: the editor, and the container it reads from.
pub struct OpenedDocument {
    /// A **skeleton** editor — geometry for the whole document, bytes for the first window only.
    pub editor: holonomy_text::Editor,
    /// The container. **Not borrowed by `editor`**, and that is the point; see [`open_document`].
    pub container: Wavefunction,
}

/// Open a container from a descriptor and load its document into a **sparse** editor.
///
/// # The store is a loader, not a resident owner — and that settles the lifetime question
///
/// `SectionStore<'c>` borrows the container, so a store held *inside* something that also owns the
/// container would be self-referential. That would need `ouroboros` or `yoke`, and a dependency plus a
/// macro to save one lifetime annotation.
///
/// **This function sidesteps it, and the fact that it can is the design.** The store exists to *fetch*
/// leaves; the leaves it fetches are `SecureBlock`s owned by the rope, not views into the store. So the
/// store can be created, used to fault in the first window, and **dropped** — and the resident bytes stay
/// resident. [`Wavefunction`] is returned beside the editor rather than borrowed by it, so nothing borrows
/// anything.
///
/// What this does *not* solve is faulting a leaf in **later**, after the session has started. That needs a
/// store alive for the session's lifetime, and therefore does need the ownership question answered
/// properly. **It is not needed to open a document and paint it, and doing that first is why this shape
/// was chosen.**
///
/// # What is resident when this returns
///
/// The **whole document's geometry** — every leaf slot, `starts`, and therefore `text_len` and every
/// offset — and the **first `budget` sections' bytes**. Nothing else. `editor.resident_bytes()` is the
/// number, and it should be well under the budget's ceiling because the budget is counted in sections and
/// the window is counted in bytes.
///
/// # Why the first window is faulted at all
///
/// **Because the paint path reads through `&self` and cannot fault.** `Session`'s emitters call
/// `Editor::read_into`, which is `&self` and refuses an absent leaf; the painter counts that as
/// `PaintStats::runs_missing` — safe, and wrong to draw. So without a resident first window, opening a
/// document would open it onto a blank page. That is the honest reason the load reads anything, and it is
/// a *stopgap*: the real fix is the paint path taking `read_into_faulting`, which is a separate step.
///
/// # `vdf_iterations` is caller-supplied, and the product has no value for it yet
///
/// The count is **not recorded in the container** — `Wavefunction::open`/`adopt` take it as a
/// parameter — so a caller must know it out of band. The only constant in the tree is
/// [`TEST_VDF_ITERATIONS`](holonomy_container::TEST_VDF_ITERATIONS), which is explicitly for tests and
/// "any caller that does not care about latency". **The product has no production value**, and a real
/// unlock is supposed to derive one from a measured per-squaring cost (PROJECT.md §2.4). Passing the
/// test constant here is honest but temporary, and it means **an H1 container written by a future build
/// with different iterations would not open with this one.**
pub fn open_document(
    file: holonomy_container::io::DirectFile,
    passphrase: &str,
    vdf_iterations: u64,
    budget: usize,
) -> Result<OpenedDocument, StoreError> {
    let mut container = Wavefunction::adopt(file, passphrase, vdf_iterations).map_err(|_| StoreError::Read)?;
    let len = container.content_len() as usize;
    let mut editor = holonomy_text::Editor::from_skeleton(len);

    // Fault in the first window, so the paint path has something to draw. `Session` owns no store, so
    // this is the only moment a store exists during the load -- and that is fine, see the type's docs.
    let window = len.min(budget * SECTION_BYTES);
    if window > 0 {
        let mut store = SectionStore::new(&mut container, budget);
        let mut buf = vec![0u8; window];
        editor
            .read_into_faulting(&mut store, 0, &mut buf)
            .map_err(|_| StoreError::Read)?;
    }
    Ok(OpenedDocument { editor, container })
}

// ---------------------------------------------------------------------------
// The join: this store *is* the rope's byte source.
// ---------------------------------------------------------------------------

/// **A document offset maps to sections by division, and that division is the whole join.**
///
/// Section `s` holds document bytes `[s * SECTION_BYTES, (s + 1) * SECTION_BYTES)`. A leaf at `offset`
/// with `len` bytes therefore touches sections `offset / SECTION_BYTES` through
/// `(offset + len - 1) / SECTION_BYTES` — and because `65,520 / 3,840 = 17.0625`, **that is two sections
/// for most leaves**, not one. See [`SectionStore`]'s `fetch_leaf` for what that costs.
impl holonomy_text::LeafSource for SectionStore<'_> {
    /// Write a leaf's **current** bytes at `into`, patching whichever cached sections they overlap.
    ///
    /// This is the write half of the seam, and it is a thin wrapper over [`SectionStore::write_at`] -- the
    /// reason the rope can shed a leaf without losing it. See that method for why patching the cache rather
    /// than writing through is sufficient, and why a leaf straddling a section boundary has to do both sides.
    fn store_leaf(&mut self, into: usize, bytes: &[u8]) -> Result<(), holonomy_text::RopeError> {
        self.write_at(into, bytes).map_err(|_| holonomy_text::RopeError::SourceUnavailable)
    }

    /// Fill `out` with the `out.len()` document bytes starting at `offset`.
    ///
    /// # The two-section case, which is the whole difficulty
    ///
    /// A leaf is at most 3,841 bytes and a section is 65,520, so a leaf usually sits inside one section and
    /// **straddles the boundary in 1 leaf in 17**. When it straddles, the bytes come from two sections, and
    /// the two `copy_into` calls must not be allowed to evict each other:
    ///
    /// * **Pinning both is the store's job, and LRU does not guarantee it.** Loading section `first` and
    ///   then section `last` are two independent accesses; with a budget of 1 the second evicts the first.
    ///   The bytes are copied into `out` before the second call, so this particular read is still correct —
    ///   but a caller that held a *leaf* across two faults would get the wrong pair. **So the invariant is
    ///   that a leaf's bytes are copied out within one `fetch_leaf`, never re-read from a resident section
    ///   later.** That is why this assembles into `out` rather than returning a borrow into the store.
    ///
    /// * **One reusable scratch buffer, not one per section.** Two `SECTION_BYTES` allocations per fetch
    ///   would be two allocations per fault, and the keystroke path counts allocations. `scratch` already
    ///   exists for [`copy_into`](Self::copy_into), so this reuses it and pays nothing.
    ///
    /// # Errors carry nothing
    ///
    /// Every failure becomes [`RopeError::SourceUnavailable`], which is opaque by design. A rope that
    /// reported "chunk 47 failed to authenticate" would be a decryption oracle with a nicer interface.
    fn fetch_leaf(&mut self, offset: usize, out: &mut [u8]) -> Result<usize, holonomy_text::RopeError> {
        if out.is_empty() {
            return Ok(0);
        }
        let end = offset.checked_add(out.len()).ok_or(holonomy_text::RopeError::SourceUnavailable)?;
        let first = (offset / SECTION_BYTES) as u32;
        let last = ((end - 1) / SECTION_BYTES) as u32;

        // `copy_into` needs `&mut self` *and* a destination, so the destination cannot be a field of
        // `self` -- `copy_into` uses `self.scratch` internally and the borrow checker will not split a
        // method receiver from its own field. Taking the buffer out for the call is the way round it, and
        // it is a move of a `Vec`, not a copy: `fetch_scratch` persists across calls, so after the first
        // fault this allocates nothing.
        let mut buf = std::mem::take(&mut self.fetch_scratch);
        if buf.len() < SECTION_BYTES {
            buf.resize(SECTION_BYTES, 0);
        }

        // **A closure rather than early returns**, because `buf` has to go back into `self` on every exit
        // path including the error ones, and an early `return Err(..)` in the middle of the loop would skip
        // that -- leaving the next fetch to allocate again. `Drop` cannot cover it either: the buffer has
        // already been moved out of `self` by the time the error happens.
        let result = (|| -> Result<usize, holonomy_text::RopeError> {
            let mut written = 0usize;
            let mut section = first;
            while section <= last {
                let got = self
                    .copy_into(section, &mut buf[..SECTION_BYTES])
                    .map_err(|_| holonomy_text::RopeError::SourceUnavailable)?;
                if got == 0 {
                    return Err(holonomy_text::RopeError::SourceUnavailable);
                }
                let section_start = section as usize * SECTION_BYTES;
                let section_end = section_start + got;
                // **Two different offsets, and conflating them was the bug.** `take_start`/`take_end`
                // are positions *in the section*, and `from` indexes `buf` by them. `written` is the
                // position in `out`, the *destination*, and must not appear in either. Adding it made
                // the second section of a straddling leaf read from `section_start + written` instead of
                // `section_start`, so a 3,841-byte leaf came back 223 bytes short — and the first
                // 16 leaves in 17 were unaffected, which is why it only showed up here.
                let take_start = offset.max(section_start);
                let take_end = end.min(section_end);
                if take_end > take_start {
                    let from = take_start - section_start;
                    let n = take_end - take_start;
                    out[written..written + n].copy_from_slice(&buf[from..from + n]);
                    written += n;
                }
                section += 1;
            }
            debug_assert_eq!(
                written,
                out.len(),
                "the sections did not cover the requested range"
            );
            Ok(written)
        })();
        self.fetch_scratch = buf;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resident section is page-locked, and **this is the assertion that a policy cannot have been
    /// silently downgraded.** An unlocked block holding document text would pass every other test here: it
    /// would still be bounded, still scrub on eviction, and still return the right bytes. The only thing
    /// that distinguishes it is `is_locked`.
    #[test]
    fn a_resident_section_is_page_locked() {
        let block = SecureBlock::allocate_with(4096, LockPolicy::PageLocked).expect("allocate");
        assert!(
            block.is_locked(),
            "a resident section must not be swappable; LockPolicy::Unlocked exists for derived data \\
             and this is document text"
        );
    }

    /// Ticks are unique, so the victim is total-ordered and nameable — which is what lets a test assert
    /// *which* section was evicted rather than merely that one was.
    #[test]
    fn a_new_store_holds_nothing_and_has_no_victim() {
        let container = Wavefunction::create(
            &std::path::PathBuf::from("/nonexistent/never-created.wavefunction"),
            "p",
            "t",
            b"x",
            1,
        );
        // Only the failure path is needed: the store's constructor never touches the container.
        if let Ok(mut container) = container {
            let store = SectionStore::new(&mut container, 4);
            assert_eq!(store.resident(), 0);
            assert_eq!(store.resident_bytes(), 0);
            assert_eq!(store.budget(), 4);
            assert_eq!(store.lru_victim(), None, "nothing is resident, so there is no victim");
        }
    }
}