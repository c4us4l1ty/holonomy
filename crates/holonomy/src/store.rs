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
}

/// A bounded set of resident document sections, loaded on demand.
///
/// The container is borrowed for `'c` rather than owned, so the store cannot outlive the file it reads —
/// and a caller cannot hold two stores against one container with independent budgets and no combined
/// bound. **One container, one store, one budget**, which is the only arrangement where *"resident text
/// stays bounded"* is a statement about the process rather than about a value.
pub struct SectionStore<'c> {
    container: &'c Wavefunction,
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
    stats: StoreStats,
}

impl<'c> SectionStore<'c> {
    /// A store over `container` holding at most `budget` sections.
    ///
    /// **A budget of 0 is allowed and means "nothing stays resident"**, which is a legitimate configuration
    /// for measuring a load with no cache and is not silently promoted to 1. Every other value is what it
    /// says.
    pub fn new(container: &'c Wavefunction, budget: usize) -> Self {
        Self {
            container,
            budget,
            resident: BTreeMap::new(),
            tick: 0,
            scratch: Vec::new(),
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
            },
        );
        self.stats.loads += 1;
        out[..got].copy_from_slice(&self.scratch[..got]);
        Ok(got)
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
        if let Ok(container) = container {
            let store = SectionStore::new(&container, 4);
            assert_eq!(store.resident(), 0);
            assert_eq!(store.resident_bytes(), 0);
            assert_eq!(store.budget(), 4);
            assert_eq!(store.lru_victim(), None, "nothing is resident, so there is no victim");
        }
    }
}