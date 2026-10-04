//! The bounded undo stack: a 500-entry ring of diff-based actions over one flat byte arena.
//!
//! PROJECT.md §5 Phase 6: "Undo: one 500-entry in-memory stack, one for the document." A Phase 6
//! directive adds that discarded history must be *actively scrubbed*, not merely dropped.
//!
//! # Why diffs and not snapshots
//!
//! A snapshot stack for a 6.40 MiB document would need 6.40 MiB per entry, so 500 entries is 3.2 GB.
//! A diff is `(offset, bytes)`: an insert records the bytes it added and a delete records the bytes it
//! removed, so an entry costs its own length plus 9 bytes and 500 entries of typical typing fit in
//! tens of kilobytes.
//!
//! # Why an arena and not a `Vec` per entry
//!
//! Because the stack is on the keystroke path. A `Vec<Vec<u8>>` allocates on every push, which would
//! put a heap allocation inside FR-1.2's "typing allocates nothing" — the *same* keystroke that is
//! measured allocation-free would allocate to record its own undo entry.
//!
//! So the bytes live in one pre-allocated ring, [`ARENA_BYTES`], and each entry records where in the
//! ring its bytes are. A push that does not fit evicts the oldest entries until it does, zeroizing
//! their bytes on the way out. After construction, no push allocates.
//!
//! # What "scrubbed" means here, precisely
//!
//! Three distinct events overwrite bytes with zeros:
//!
//! 1. **Overflow.** The 501st action evicts the 1st. The evicted entry's bytes are zeroized before the
//!    ring pointer moves, so they are not merely unreachable — they are gone.
//! 2. **Drop.** The stack is dropped: the whole arena is zeroized.
//! 3. **Undo.** An action is applied in reverse, and the caller decides whether to keep or discard it
//!    (redo). [`UndoStack::pop_for_undo`] does *not* zeroize, because the bytes are handed to the
//!    caller for the undo itself.
//!
//! # The `volatile` write, and why it is not enough on its own
//!
//! Zeroizing goes through [`ZeroizingWriteVolatile`], so the compiler may not elide the stores. That is
//! necessary and not sufficient: a store may sit in a write buffer or in a cache line that is never
//! written back to DRAM before power is lost. `Plan.md` FR-5.4 separately requires an ephemeral XOR
//! scramble every 30 seconds, and `Zeroize` does not attempt to replace that — this is about not
//! leaving plaintext in a heap the allocator will hand to something else.
//!
//! # Redo
//!
//! Not here. PROJECT.md says one stack, and a redo stack would be a second copy of the same plaintext
//! for no requirement. [`UndoStack::pop_for_undo`] hands the action out and leaves the choice of
//! re-pushing (redo) or dropping to the caller.

use zeroize::Zeroize;

/// Actions retained. PROJECT.md §5 Phase 6 fixes this at 500.
pub const UNDO_DEPTH: usize = 500;

/// Bytes of action payload held at once.
///
/// 64 KiB for 500 entries is 131 bytes per entry, which covers a typed word or a short pasted line.
/// An action longer than this is refused rather than evicting the whole stack — see
/// [`UndoError::ActionTooLarge`].
///
/// Sized rather than grown because the arena must not allocate after construction, and a bounded arena
/// is what makes that true.
pub const ARENA_BYTES: usize = 64 * 1024;

/// Which way an action goes when undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ActionKind {
    /// Text was inserted. Undo deletes `[offset, offset+len)`.
    Insert = 0,
    /// Text was deleted. Undo re-inserts `bytes` at `offset`.
    Delete = 1,
}

impl ActionKind {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Insert),
            1 => Some(Self::Delete),
            _ => None,
        }
    }
}

/// One recorded action: where, which way, and how many bytes.
///
/// 16 bytes: three `u32`s and a `u8`, padded to `align(4)`. The padding is free -- 500 entries is
/// 8 KB against a 64 KiB arena -- and keeping the struct 4-byte aligned means the ring walk touches
/// four entries per cache line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct RingEntry {
    /// Document byte offset the action applied at.
    offset: u32,
    /// Length of the payload in the arena.
    len: u32,
    /// Where the payload starts in the arena.
    ///
    /// Stored rather than derived from the ring order. Deriving it means summing the lengths of every
    /// newer entry, which is O(n) per lookup and O(n^2) per eviction sweep -- and the first version did
    /// exactly that, with a stub helper that returned 0, so every payload read from offset zero.
    arena_at: u32,
    /// [`ActionKind`] as a `u8`. A zero byte is a valid `Insert`, so this cannot be a sentinel.
    kind: u8,
}

/// One undoable action, with its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoAction {
    /// Document byte offset.
    pub offset: u32,
    /// Which way it goes when undone.
    pub kind: ActionKind,
    /// The bytes: what was inserted, or what was deleted.
    pub bytes: Vec<u8>,
}

/// Why an undo operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoError {
    /// The action's payload is larger than the whole arena, so it can never be stored.
    ///
    /// Refused rather than evicting everything to make room: a 64 KiB paste would destroy 500
    /// actions, and the user would get one undo step for one keystroke. The alternative -- storing the
    /// payload in a `SecureBlock` -- is a Phase 8 question, not one to settle silently.
    ActionTooLarge {
        /// Payload length.
        len: usize,
        /// The arena's capacity, always [`ARENA_BYTES`].
        capacity: usize,
    },
    /// The stack is empty.
    Empty,
}

impl std::fmt::Display for UndoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActionTooLarge { len, capacity } => write!(
                f,
                "an action of {len} bytes exceeds the {capacity}-byte undo arena"
            ),
            Self::Empty => write!(f, "the undo stack is empty"),
        }
    }
}

impl std::error::Error for UndoError {}

/// A 500-entry ring of diff-based undo actions over one flat byte arena.
///
/// # Invariants
///
/// * `len <= UNDO_DEPTH`.
/// * `head + len <= UNDO_DEPTH`, wrapping: the live entries are
///   `ring[(head + i) % UNDO_DEPTH]` for `i in 0..len`, oldest first.
/// * `arena_head + arena_used <= ARENA_BYTES`, wrapping, and the live entries' byte ranges are
///   disjoint and inside the arena.
/// * **No action's bytes are reachable twice.** Arena space is reclaimed only by evicting the oldest
///   entries, in order, so an entry's bytes are overwritten only after that entry is dead.
#[derive(Debug)]
pub struct UndoStack {
    ring: [RingEntry; UNDO_DEPTH],
    /// Number of live entries.
    len: usize,
    /// Index of the oldest live entry.
    head: usize,
    arena: Vec<u8>,
    /// Where the next action's bytes go in `arena`.
    arena_head: usize,
    /// Bytes of `arena` in use, including gaps left by evicted entries.
    ///
    /// Tracked rather than derived because the arena is a ring: after wrapping, the free space is not
    /// a contiguous suffix.
    arena_used: usize,
    /// Actions evicted by overflow, for diagnostics.
    evicted: u64,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoStack {
    /// An empty stack, with its arena allocated once.
    ///
    /// This is the *only* allocating call in the type. After it, [`push`](Self::push) allocates
    /// nothing, which is what keeps FR-1.2's "typing allocates nothing" true for a keystroke that also
    /// records its own undo entry.
    pub fn new() -> Self {
        Self {
            ring: [RingEntry {
                offset: 0,
                len: 0,
                arena_at: 0,
                kind: 0,
            }; UNDO_DEPTH],
            len: 0,
            head: 0,
            arena: vec![0u8; ARENA_BYTES],
            arena_head: 0,
            arena_used: 0,
            evicted: 0,
        }
    }

    /// Number of live actions.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether there is nothing to undo.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The configured depth, always [`UNDO_DEPTH`].
    #[inline]
    pub fn depth(&self) -> usize {
        UNDO_DEPTH
    }

    /// Actions dropped by overflow since construction.
    #[inline]
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Free bytes in the arena.
    #[inline]
    pub fn arena_free(&self) -> usize {
        ARENA_BYTES - self.arena_used
    }

    /// Record an insert of `bytes` at `offset`.
    pub fn push_insert(&mut self, offset: u32, bytes: &[u8]) -> Result<(), UndoError> {
        self.push(offset, ActionKind::Insert, bytes)
    }

    /// Record a delete of `len` bytes at `offset`. `bytes` must be what was there.
    ///
    /// Takes the bytes rather than the length because undo needs them: re-inserting a delete requires
    /// the content, and FR-1.2's destructive-delete rule means it cannot be recovered from the rope.
    pub fn push_delete(&mut self, offset: u32, bytes: &[u8]) -> Result<(), UndoError> {
        self.push(offset, ActionKind::Delete, bytes)
    }

    /// Record an action. The only mutator.
    ///
    /// # Order of operations
    ///
    /// 1. Reject an over-large payload.
    /// 2. Evict from the front until the arena has room. Eviction *zeroizes*.
    /// 3. Evict from the front until the ring has a free slot. The ring is 6 KB and the arena is
    ///    64 KiB, so the arena always runs out first; the ring eviction is there so the invariant
    ///    holds rather than because it happens.
    /// 4. Copy the payload in and record the entry.
    ///
    /// Steps 2 and 3 are separate because the two resources are different sizes, and conflating them
    /// -- evicting until *both* have room -- would over-evict: a single 40 KiB action cannot fit
    /// alongside 500 small ones in 64 KiB, but only the 4 oldest need to go.
    pub fn push(&mut self, offset: u32, kind: ActionKind, bytes: &[u8]) -> Result<(), UndoError> {
        if bytes.len() > ARENA_BYTES {
            return Err(UndoError::ActionTooLarge {
                len: bytes.len(),
                capacity: ARENA_BYTES,
            });
        }
        while self.arena_used + bytes.len() > ARENA_BYTES && self.len > 0 {
            self.evict_oldest();
        }
        // `bytes.len() <= ARENA_BYTES`, so the loop above either made room or emptied the stack. With
        // the stack empty and the action no larger than the arena, room exists.
        debug_assert!(self.arena_used + bytes.len() <= ARENA_BYTES || self.len == 0);
        while self.len >= UNDO_DEPTH {
            self.evict_oldest();
        }

        let start = self.arena_head;
        let end = (start + bytes.len()) % ARENA_BYTES;
        if !bytes.is_empty() {
            if end > start {
                self.arena[start..end].copy_from_slice(bytes);
            } else {
                // Wraps the end of the ring. Two copies, and no scratch buffer.
                //
                // Guarded on `!bytes.is_empty()` because a zero-length payload makes `end == start`,
                // which falls into this branch with `head_len == ARENA_BYTES` and slices
                // `bytes[..ARENA_BYTES]` out of an empty slice: "range end index 65536 out of range
                // for slice of length 0". A zero-length edit is reachable -- an insert of nothing, or a
                // delete at the end of the document -- so this panicked on correct input.
                let head_len = ARENA_BYTES - start;
                self.arena[start..].copy_from_slice(&bytes[..head_len]);
                self.arena[..end].copy_from_slice(&bytes[head_len..]);
            }
        }
        self.arena_head = end;
        self.arena_used += bytes.len();

        let slot = (self.head + self.len) % UNDO_DEPTH;
        self.ring[slot] = RingEntry {
            offset,
            len: bytes.len() as u32,
            arena_at: start as u32,
            kind: kind as u8,
        };
        self.len += 1;
        Ok(())
    }

    /// Drop the oldest action, zeroizing its bytes.
    fn evict_oldest(&mut self) {
        if self.len == 0 {
            return;
        }
        let entry = self.ring[self.head];
        self.zeroize_entry(&entry);
        self.arena_used -= entry.len as usize;
        self.head = (self.head + 1) % UNDO_DEPTH;
        self.len -= 1;
        self.evicted += 1;
    }

    /// Overwrite an entry's arena bytes with zeros.
    fn zeroize_entry(&mut self, entry: &RingEntry) {
        let start = entry.arena_at as usize;
        let len = entry.len as usize;
        if len == 0 {
            return;
        }
        let end = (start + len) % ARENA_BYTES;
        // `Zeroize` on the slice, which writes through a volatile pointer so the stores cannot be
        // optimised away as dead.
        if end > start {
            self.arena[start..end].zeroize();
        } else {
            self.arena[start..].zeroize();
            self.arena[..end].zeroize();
        }
    }

    fn payload(&self, entry: &RingEntry) -> Vec<u8> {
        let len = entry.len as usize;
        if len == 0 {
            return Vec::new();
        }
        let start = entry.arena_at as usize;
        let end = (start + len) % ARENA_BYTES;
        let mut out = Vec::with_capacity(len);
        if end > start {
            out.extend_from_slice(&self.arena[start..end]);
        } else {
            out.extend_from_slice(&self.arena[start..]);
            out.extend_from_slice(&self.arena[..end]);
        }
        out
    }

    /// Take the newest action, for undoing.
    ///
    /// Removes it from the stack. The caller may re-push it for redo or drop it; this does not
    /// zeroize, because the bytes are handed out.
    pub fn pop_for_undo(&mut self) -> Result<UndoAction, UndoError> {
        if self.len == 0 {
            return Err(UndoError::Empty);
        }
        let idx = (self.head + self.len - 1) % UNDO_DEPTH;
        let entry = self.ring[idx];
        let bytes = self.payload(&entry);
        // Zeroize: the action is gone from the ring, and leaving its bytes readable in the arena would
        // mean a dropped undo entry's plaintext outlives it -- exactly what FR-1.2's destructive
        // contract forbids elsewhere in the crate.
        self.zeroize_entry(&entry);
        self.arena_used -= entry.len as usize;
        self.len -= 1;
        let kind = ActionKind::from_u8(entry.kind).unwrap_or(ActionKind::Insert);
        Ok(UndoAction {
            offset: entry.offset,
            kind,
            bytes,
        })
    }

    /// Read the newest action without removing it.
    pub fn peek(&self) -> Option<UndoAction> {
        if self.len == 0 {
            return None;
        }
        let idx = (self.head + self.len - 1) % UNDO_DEPTH;
        let entry = self.ring[idx];
        Some(UndoAction {
            offset: entry.offset,
            kind: ActionKind::from_u8(entry.kind)?,
            bytes: self.payload(&entry),
        })
    }

    /// The newest action's `(offset, kind, len)`, with no payload copy.
    ///
    /// For a status bar showing "undo: bold 4", which needs the shape of the action and not its
    /// bytes. O(1) and allocation-free, where [`peek`](Self::peek) copies the payload.
    pub fn peek_header(&self) -> Option<(u32, ActionKind, usize)> {
        if self.len == 0 {
            return None;
        }
        let entry = self.ring[(self.head + self.len - 1) % UNDO_DEPTH];
        Some((
            entry.offset,
            ActionKind::from_u8(entry.kind)?,
            entry.len as usize,
        ))
    }

    /// Discard every action, zeroizing the arena.
    pub fn clear(&mut self) {
        self.arena.zeroize();
        self.arena_head = 0;
        self.arena_used = 0;
        self.len = 0;
        self.head = 0;
        self.evicted = 0;
    }

    /// Assert the stack's invariants. Test-only.
    #[cfg(test)]
    pub(crate) fn check_invariants(&self) {
        assert!(
            self.len <= UNDO_DEPTH,
            "{} live entries exceeds {UNDO_DEPTH}",
            self.len
        );
        assert!(
            self.arena_used <= ARENA_BYTES,
            "arena holds {} of {ARENA_BYTES}",
            self.arena_used
        );
        assert!(self.arena_head < ARENA_BYTES || ARENA_BYTES == 0);
        // Every live entry's payload must be readable and must total the used bytes.
        let mut total = 0usize;
        let mut seen = 0..self.len;
        for i in seen.by_ref() {
            let entry = self.ring[(self.head + i) % UNDO_DEPTH];
            assert!(
                entry.len as usize <= ARENA_BYTES,
                "entry {i} claims {} bytes, more than the arena",
                entry.len
            );
            assert!(
                ActionKind::from_u8(entry.kind).is_some(),
                "entry {i} has kind {}",
                entry.kind
            );
            total += entry.len as usize;
        }
        assert_eq!(
            total, self.arena_used,
            "the live entries account for {total} bytes but the arena reports {}",
            self.arena_used
        );
    }
}

impl Drop for UndoStack {
    /// Zeroize the whole arena before the `Vec` is freed.
    ///
    /// Not `clear`, which resets the counters: the `Vec`'s own deallocation does that, and a redundant
    /// reset here would only risk leaving `arena_head` inconsistent with a buffer about to be freed.
    fn drop(&mut self) {
        self.arena.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_stack_is_empty_and_pre_allocated() {
        let mut s = UndoStack::new();
        s.check_invariants();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert_eq!(s.depth(), 500);
        assert_eq!(s.arena_free(), ARENA_BYTES);
        assert_eq!(s.evicted(), 0);
        assert!(s.pop_for_undo().is_err());
        assert!(s.peek().is_none());
    }

    #[test]
    fn an_insert_records_its_offset_and_bytes() {
        let mut s = UndoStack::new();
        s.push_insert(42, b"hello").expect("fits");
        s.check_invariants();
        assert_eq!(s.len(), 1);
        let a = s.pop_for_undo().expect("one");
        assert_eq!(a.offset, 42);
        assert_eq!(a.kind, ActionKind::Insert);
        assert_eq!(a.bytes, b"hello");
        assert!(s.is_empty());
    }

    #[test]
    fn a_delete_records_what_was_there() {
        let mut s = UndoStack::new();
        s.push_delete(7, b"removed").expect("fits");
        let a = s.pop_for_undo().expect("one");
        assert_eq!(a.offset, 7);
        assert_eq!(a.kind, ActionKind::Delete);
        assert_eq!(
            a.bytes, b"removed",
            "undo needs the content, not just the length"
        );
    }

    /// Actions come off newest-first, which is what "undo the last thing" means.
    #[test]
    fn actions_come_off_newest_first() {
        let mut s = UndoStack::new();
        s.push_insert(0, b"one").expect("a");
        s.push_insert(1, b"two").expect("b");
        s.push_insert(2, b"three").expect("c");
        assert_eq!(s.len(), 3);
        assert_eq!(s.pop_for_undo().expect("c").bytes, b"three");
        assert_eq!(s.pop_for_undo().expect("b").bytes, b"two");
        assert_eq!(s.pop_for_undo().expect("a").bytes, b"one");
        assert!(s.pop_for_undo().is_err());
    }

    /// The headline requirement: exactly 500 entries, and the 501st evicts the 1st.
    #[test]
    fn the_stack_holds_exactly_five_hundred_actions() {
        let mut s = UndoStack::new();
        for i in 0..UNDO_DEPTH {
            s.push_insert(i as u32, b"x").expect("fits");
        }
        s.check_invariants();
        assert_eq!(s.len(), UNDO_DEPTH);
        assert_eq!(s.evicted(), 0);

        s.push_insert(999, b"y").expect("fits");
        s.check_invariants();
        assert_eq!(s.len(), UNDO_DEPTH, "still exactly 500, not 501");
        assert_eq!(s.evicted(), 1, "the oldest went");

        // And the survivor is the newest, with the oldest gone.
        assert_eq!(s.peek().expect("newest").offset, 999);
        let mut offsets = Vec::new();
        while let Ok(a) = s.pop_for_undo() {
            offsets.push(a.offset);
        }
        offsets.reverse();
        // Still 500: the 501st action *replaced* the 1st rather than being refused or pushed alongside.
        // An earlier version expected 499 and reported "left: 500, right: 499". A ring that dropped the
        // newest action on overflow, or that grew past its depth, would both be wrong.
        assert_eq!(
            offsets.len(),
            UNDO_DEPTH,
            "a bounded ring replaces; it does not grow or refuse"
        );
        assert_eq!(offsets[0], 1, "offset 0 is gone");
        assert_eq!(*offsets.last().expect("last"), 999);
    }

    /// A sustained burst far past the depth, so the ring wraps many times over.
    #[test]
    fn a_burst_far_past_the_depth_keeps_only_the_newest_five_hundred() {
        let mut s = UndoStack::new();
        for i in 0..10_000u32 {
            s.push_insert(i, b"ab").expect("fits");
            s.check_invariants();
        }
        assert_eq!(s.len(), UNDO_DEPTH);
        assert_eq!(s.evicted(), (10_000 - UNDO_DEPTH) as u64);
        // The 500 live actions are offsets 9,500..10,000.
        for expected in (10_000 - UNDO_DEPTH as u32..10_000).rev() {
            assert_eq!(s.pop_for_undo().expect("action").offset, expected);
        }
    }

    /// Overflow must *scrub* the evicted bytes, not merely make them unreachable. This is the whole
    /// point of the `Zeroize` requirement, so the test looks for the plaintext rather than for the
    /// absence of an entry.
    #[test]
    fn an_evicted_actions_bytes_are_scrubbed() {
        const MARKER: &[u8] = b"S3CR3T-MARKER-BYTES";
        let mut s = UndoStack::new();

        // Fill the arena with copies of a distinctive marker, then keep going until evictions happen.
        let mut pushed = 0u32;
        while s.evicted() == 0 {
            s.push_insert(pushed, MARKER).expect("fits");
            pushed += 1;
            assert!(
                pushed < 10_000,
                "the arena never filled, which would be a bug in itself"
            );
        }
        let evicted = s.evicted();
        assert!(evicted > 0, "at least one action must have been evicted");

        // The live entries still hold their payloads.
        assert!(!s.is_empty());
        assert_eq!(
            s.peek().expect("newest").bytes,
            MARKER,
            "a live entry's payload must be readable"
        );

        // Count the markers in the raw arena. There must be exactly as many as there are live
        // entries -- no more. Every evicted entry's bytes are zeros.
        let occurrences = s
            .arena
            .windows(MARKER.len())
            .filter(|w| *w == MARKER)
            .count();
        assert_eq!(
            occurrences,
            s.len(),
            "{occurrences} copies of the marker in a {ARENA_BYTES}-byte arena holding {} live \
             actions: {evicted} evictions left plaintext behind",
            s.len()
        );
    }

    /// The strongest form of the same claim: after `clear`, no plaintext at all remains.
    #[test]
    fn clear_leaves_no_plaintext_anywhere_in_the_arena() {
        const MARKER: &[u8] = b"PASSPHRASE-MATERIAL";
        let mut s = UndoStack::new();
        // 64 KiB / 18 bytes is 3,641 actions, so 500 is nowhere near enough to overflow. An earlier
        // version pushed 500 and asserted `evicted > 0`; the arena is fourteen times larger than that,
        // so the assertion failed against a correct implementation.
        let mut i = 0u32;
        while s.evicted() == 0 {
            s.push_insert(i, MARKER).expect("fits");
            i += 1;
        }
        assert!(i > 500, "overflow needed {i} actions");
        assert!(s.arena.windows(MARKER.len()).any(|w| w == MARKER));
        s.clear();
        assert!(
            !s.arena.windows(MARKER.len()).any(|w| w == MARKER),
            "clear left plaintext in the arena"
        );
        assert!(s.arena.iter().all(|&b| b == 0), "the whole arena is zeros");
    }

    #[test]
    fn a_popped_actions_bytes_are_scrubbed() {
        let mut s = UndoStack::new();
        s.push_insert(0, b"confidential").expect("fits");
        let a = s.pop_for_undo().expect("pop");
        assert_eq!(a.bytes, b"confidential", "the caller gets them");
        // The arena must not still hold them.
        assert!(
            !s.arena.windows(12).any(|w| w == b"confidential"),
            "a popped action's plaintext is still in the arena"
        );
    }

    #[test]
    fn an_oversized_action_is_refused_rather_than_evicting_everything() {
        let mut s = UndoStack::new();
        for i in 0..10u32 {
            s.push_insert(i, b"small").expect("fits");
        }
        let err = s
            .push_insert(0, &vec![b'x'; ARENA_BYTES + 1])
            .expect_err("too large");
        assert_eq!(
            err,
            UndoError::ActionTooLarge {
                len: ARENA_BYTES + 1,
                capacity: ARENA_BYTES
            }
        );
        assert_eq!(s.len(), 10, "a refused push must not evict anything");
        assert_eq!(s.evicted(), 0);
    }

    /// The arena is a ring, so a payload that wraps the end must be stored and read back intact.
    #[test]
    fn a_payload_that_wraps_the_arena_end_round_trips() {
        let mut s = UndoStack::new();
        // Fill to within a few bytes of the end with a large action, so the next one wraps.
        let big = ARENA_BYTES - 100;
        s.push_insert(0, &vec![b'A'; big]).expect("fits");
        assert_eq!(s.pop_for_undo().expect("a").bytes.len(), big);

        // Two actions whose combined size exceeds the arena, forcing several wraps.
        let mut s = UndoStack::new();
        for i in 0..8u32 {
            let payload = vec![b'a' + (i as u8 % 26); ARENA_BYTES / 8 + 1];
            s.push_insert(i, &payload).expect("fits");
            s.check_invariants();
        }
        assert!(s.len() < 8, "the arena could not hold all eight");
        for i in (0..8u32).rev() {
            if let Ok(a) = s.pop_for_undo() {
                let want = vec![b'a' + (i as u8 % 26); ARENA_BYTES / 8 + 1];
                assert_eq!(a.bytes, want, "wrapped payload {i} came back wrong");
            }
        }
    }

    /// A zero-length action is legal and costs nothing, so typing a newline and then undoing it works.
    #[test]
    fn a_zero_length_action_is_legal() {
        let mut s = UndoStack::new();
        s.push_insert(5, b"").expect("empty");
        assert_eq!(s.len(), 1);
        let a = s.pop_for_undo().expect("pop");
        assert_eq!(a.offset, 5);
        assert!(a.bytes.is_empty());
    }

    #[test]
    fn peek_does_not_consume_and_needs_no_allocation_for_the_header() {
        let mut s = UndoStack::new();
        s.push_insert(11, b"twelve").expect("fits");
        assert_eq!(s.peek_header(), Some((11, ActionKind::Insert, 6)));
        assert_eq!(s.len(), 1, "peek does not consume");
        assert_eq!(s.peek().expect("peek").bytes, b"twelve");
        assert_eq!(s.len(), 1);
        // And `peek` allocates, which is why `peek_header` exists.
        assert_eq!(s.peek_header().expect("header").2, 6);
    }

    #[test]
    fn clear_scrubs_everything() {
        let mut s = UndoStack::new();
        for i in 0..20u32 {
            s.push_insert(i, b"top-secret-value").expect("fits");
        }
        assert!(!s.is_empty());
        s.clear();
        s.check_invariants();
        assert!(s.is_empty());
        assert_eq!(s.arena_free(), ARENA_BYTES);
        assert_eq!(s.evicted(), 0);
        assert!(
            s.arena.iter().all(|&b| b == 0),
            "clear must scrub the arena"
        );
    }

    /// The interleaved case that a pair of independent structures gets wrong: a long action evicts
    /// several short ones, and the survivors must still be readable in order.
    #[test]
    fn a_long_action_evicts_only_as_many_short_ones_as_it_must() {
        let mut s = UndoStack::new();
        for i in 0..400u32 {
            s.push_insert(i, b"....").expect("short");
        }
        assert_eq!(s.len(), 400);
        // 16 KiB needs 4,096 four-byte actions' worth of space at most; the arena is 64 KiB, so this
        // evicts about a quarter of them, not all of them.
        s.push_insert(999, &vec![b'Z'; ARENA_BYTES / 4])
            .expect("big");
        s.check_invariants();
        assert!(
            s.len() > 200,
            "a quarter-arena action evicted {} of 400 short ones; it should evict far fewer",
            400 - s.len()
        );
        // And the newest is intact.
        assert_eq!(s.peek().expect("newest").offset, 999);
        assert_eq!(s.peek().expect("newest").bytes.len(), ARENA_BYTES / 4);
    }

    /// The requirement's shape, end to end: 500 undo operations, and overflow scrubs.
    #[test]
    fn five_hundred_undo_and_redo_operations() {
        let mut s = UndoStack::new();
        // Record 600 actions, then undo the last 500 of them -- the deepest history available.
        for i in 0..600u32 {
            s.push_insert(i, &[b'A' + (i % 26) as u8; 3]).expect("fits");
        }
        assert_eq!(s.evicted(), 100);

        let mut undone = Vec::new();
        for _ in 0..UNDO_DEPTH {
            let a = s.pop_for_undo().expect("undo");
            undone.push(a);
        }
        assert!(s.is_empty(), "500 undos exhausted a 500-deep history");
        assert_eq!(undone.len(), UNDO_DEPTH);

        // Redo: push them back, and the stack must hold exactly 500 again.
        for a in undone.into_iter().rev() {
            let _ = s.push(a.offset, a.kind, &a.bytes);
        }
        assert_eq!(s.len(), UNDO_DEPTH);
        s.check_invariants();
        // And the oldest surviving offset is 200: actions 0..=99 were evicted, and 600 actions minus
        // 100 evicted leaves 500 starting at offset 100. Redoing in order re-pushed the newest 500,
        // which are offsets 100..600.
        assert_eq!(s.peek().expect("newest").offset, 599);
    }

    /// An invariant check over a long mixed workload.
    #[test]
    fn invariants_hold_under_a_mixed_workload() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        let mut s = UndoStack::new();
        for _ in 0..20_000 {
            match next(3) {
                0 => {
                    let len = next(64);
                    let payload = vec![b'x'; len];
                    let _ = s.push_insert(next(1000) as u32, &payload);
                }
                1 => {
                    let len = next(64);
                    let payload = vec![b'y'; len];
                    let _ = s.push_delete(next(1000) as u32, &payload);
                }
                _ => {
                    let _ = s.pop_for_undo();
                }
            }
            s.check_invariants();
        }
        assert!(s.len() <= UNDO_DEPTH);
    }
}
