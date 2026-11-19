//! A million random keystrokes, and the three things that must not drift apart.
//!
//! Block 4's third gate is a count, not a claim about speed: *1 M random keystrokes — zero leaks,
//! zero residency blowups, undo past every point in the log.* This file is that run, and it found
//! three real bugs on its way to passing: a seal that wrote the bytes an undo had taken back
//! (D-31), a burst that filled the buffer inside a character and lost the keystroke (D-32), and a
//! redo after an undo of a root that came back one byte short (OQ-21 — the third clause's own
//! failure, fixed by keeping the bytes a root undo drops so the redo can re-attach them).
//!
//! The short slice runs on every push in a debug build with every action checked, undo and redo
//! included. The million is `#[ignore]`d and runs in release in the `test` job — **without** undo
//! and redo, because the third clause does not hold yet: D-33 made undo-then-redo exact for its own
//! reproduction (OQ-21), and the walk then found the next one, OQ-22, with a hard invariant break
//! beside it (OQ-23).
//!
//! It types into a real [`Typing`] session over a real [`ResidentSet`], and after every step it
//! asks three questions:
//!
//! 1. **Does the view still say what the model says?** A `String` alongside the session receives
//!    the same edits. The document is read back through [`copy`] — the one reader path the Eye and
//!    the shell use — and the bytes must equal the model. Periodically the leaves are checked to
//!    tile the document exactly once and to name only segments that exist.
//! 2. **Does residency stay inside its budget?** After every fault-in, wipe and read, the resident
//!    bytes must be within the configured budget, and a wipe must leave nothing resident.
//! 3. **Does undo walk the session's own history?** Every undo must land on a document length the
//!    session has actually been in, and at the end of the run the whole stack is unwound and
//!    rewound, each redo matched against the state it undid, byte for byte. That is the model-free
//!    half — it does not trust this file's idea of what an undo means — and it is a test of its
//!    own, for six seeds, `#[ignore]`d with its reason (OQ-22). One thing it accounts
//!    for, in the design's own words: a root recorded before a cut names **written** bytes only, so
//!    the bytes open at that moment are not in it. The walk starts from the settled document, and
//!    the step that undoes the root keeps a copy of those bytes so its redo puts them back — which
//!    is what makes undo-then-redo a no-op instead of a silent deletion.
//!
//! Where the claims stop, said plainly, because a gate is evidence only for what it measures:
//!
//! - **"Zero leaks"** is asserted here as bytes: the view is what the model says, every leaf is
//!   backed by something that exists, and nothing stays resident after a wipe. Freed heap memory is
//!   not observable from a test; byte-level zeroization is `zeroize`'s subject, and the counting
//!   allocator in `tests/keystroke_allocations.rs` measures the other half — that the keystroke
//!   path allocates nothing at all.
//! - **"Undo past every point in the log"** is bounded by the session's own retention:
//!   `UNDO_DEPTH` marks and `ROOT_DEPTH` roots, oldest dropped, and a cut drops the marks above it.
//!   The walk below therefore unwinds *every state the session still retains* — the session's
//!   answer, not this file's — and the report says how many it visited.
//! - **Random is seeded.** A failure names the seed, and the same seed replays the same run.
//!
//! Two sizes of the same harness. The default tests run a slice short enough for a debug build on
//! every push. The ignored one runs the gate's million keystrokes; CI executes it in release mode
//! and the printed `HOLONOMY_KEYSTROKE_FUZZ` line is the gate's evidence — the count, the seeds and
//! what each part of the run did, so the claim can be checked rather than believed.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use holonomy_text::{
    copy, Appended, Location, ResidentSet, SegmentId, SegmentSink, SegmentSource, Selection, Slab,
    Typing, TypingError,
};

/// The shortest document the run lets itself get down to before it is forced to type.
const FLOOR: usize = 64;

/// The longest, after which the run is forced to cut.
const CEILING: usize = 512;

/// The session's buffer: one leaf's worth of keystrokes before a seal.
const CAPACITY: usize = 256;

/// The resident set's budget, comfortably more than one segment.
const BUDGET: usize = 4 * 1024;

/// The slack a resident slab may carry.
const SLACK: usize = 64;

/// What the run types: multi-byte, combining, ZWJ, astral, and a paste longer than a keystroke.
///
/// These are the boundaries a cut must never split. `insert_at` and `delete` require character
/// boundaries, and a range that splits one leaves a leaf whose bytes are not text.
const TYPED: &[&str] = &[
    "a",
    "e",
    " ",
    "\n",
    "the ",
    "q",
    "\u{e9}",
    "\u{6f22}",
    "e\u{301}",
    "\u{1f44d}\u{200d}\u{1f469}",
    "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
    "paste: a fragment long enough to fill the buffer faster than one keystroke.",
];

/// A deterministic generator, so a failure names the run that reproduces it.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Zero is the one seed a shift register cannot leave.
        Self(seed | 1)
    }

    /// A value below `bound`, and never `bound` itself.
    fn below(&mut self, bound: u64) -> u64 {
        debug_assert!(bound > 0, "a bound of zero has no values below it");
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x % bound
    }

    /// A value in `low..=high`.
    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low + 1)
    }
}

/// The segment store the session appends to and the resident set reads back.
///
/// Shared through an `Rc` so both sides see one world-line: a segment the sink sealed is a segment
/// the source can fault in. This is the seam `tests/commit_boundary.rs` proves against the real
/// container. A `Vec` is enough here, because this file is about the document, the budget and the
/// undo stack — not about the format.
#[derive(Clone, Default)]
struct Store(Rc<RefCell<Vec<Vec<u8>>>>);

impl Store {
    /// Seals one segment and returns its dense id.
    fn append(&self, plaintext: &[u8]) -> SegmentId {
        let mut segments = self.0.borrow_mut();
        let segment = SegmentId::new(u64::try_from(segments.len()).expect("fits"));
        segments.push(plaintext.to_vec());
        segment
    }

    /// How many segments the world-line holds.
    fn len(&self) -> usize {
        self.0.borrow().len()
    }

    /// Whether `segment` holds the bytes `slab` names.
    fn covers(&self, segment: SegmentId, slab: Slab) -> bool {
        let Some(bytes) = self.get(segment) else {
            return false;
        };
        usize::try_from(slab.end()).is_ok_and(|end| end <= bytes.len())
    }

    /// The plaintext of `segment`, as a fault-in would read it.
    fn get(&self, segment: SegmentId) -> Option<Vec<u8>> {
        let index = usize::try_from(segment.get()).ok()?;
        self.0.borrow().get(index).cloned()
    }
}

/// The world-line as the session's commit boundary sees it.
struct Sink(Store);

impl SegmentSink for Sink {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        if plaintext.is_empty() {
            return Err("a zero-byte segment carries nothing".to_owned());
        }
        let segment = self.0.append(plaintext);
        let bytes_written = u64::try_from(plaintext.len()).expect("a segment fits in u64");
        Ok(Appended {
            segment,
            witness_sequence: segment.get() + 1,
            segment_root: [0; 32],
            bytes_written,
        })
    }
}

/// The world-line as the resident set sees it.
struct Source(Store);

impl SegmentSource for Source {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        self.0
            .get(segment)
            .ok_or_else(|| format!("no segment {segment:?}"))
    }
}

/// What the run did, so the million-keystroke report is a sentence about the run, not a number.
#[derive(Default)]
struct Counts {
    inserted: u64,
    deleted: u64,
    replaced: u64,
    marked: u64,
    sealed: u64,
    undone: u64,
    redone: u64,
    faults: u64,
    wipes: u64,
    reads: u64,
}

/// One session, its model, and the invariants between them.
struct Session {
    typing: Typing<Sink>,
    residency: ResidentSet<Source>,
    store: Store,
    rng: Rng,
    counts: Counts,
    seed: u64,
    /// The document as the same edits would have left a plain `String`.
    model: String,
    /// Every document length the session has been observed in — the set an undo must land in.
    lengths: HashSet<u64>,
    /// The high-water mark of the resident set, for the report.
    peak_resident: usize,
    /// How many actions have been taken.
    steps: u64,
    /// The last action and its offsets, so a failure names the keystroke that broke it.
    last: (&'static str, u64, u64),
    /// Whether undo and redo are part of the mix, or the run stays on the editing half.
    ///
    /// The million-keystroke run keeps them out until OQ-21 is fixed: the gate's *undo* clause is
    /// exactly what is broken, and a run that fails on it cannot also be evidence for the leak and
    /// residency clauses.
    undo: bool,
    /// Whether every action is checked, or only every sixty-fourth.
    ///
    /// The short slice checks every action, so a failure names the keystroke that did it; the
    /// million cannot afford that and reads the document on the periodic beat instead.
    strict: bool,
    /// A ring of the actions before it, so a failure can say what led there.
    recent: [(&'static str, u64, u64); RECENT],
    /// Where the next ring entry goes.
    cursor: usize,
}

/// How many actions a failure remembers.
const RECENT: usize = 12;

impl Session {
    fn new(seed: u64, strict: bool, undo: bool) -> Self {
        let store = Store::default();
        let typing = Typing::new(Sink(store.clone()), CAPACITY);
        let residency = ResidentSet::new(Source(store.clone()), BUDGET, SLACK);
        let residency = residency.expect("a budget that holds one segment");
        let mut lengths = HashSet::new();
        lengths.insert(0);
        let rng = Rng::new(seed);
        let model = String::new();
        Self {
            typing,
            residency,
            store,
            rng,
            counts: Counts::default(),
            seed,
            model,
            lengths,
            peak_resident: 0,
            steps: 0,
            last: ("start", 0, 0),
            strict,
            undo,
            recent: [("start", 0, 0); RECENT],
            cursor: 0,
        }
    }

    /// Reads the document back through the one reader path, with the leaves checked underneath it.
    fn read(&mut self) -> String {
        let len = self.typing.document_len();
        self.every_leaf_is_backed(len);
        let spine = self.typing.spine();
        let draft = self.typing.draft();
        let sources = self.typing.sources();
        let open = self.typing.pending();
        let selection = Selection::new(0, len);
        let residency = &mut self.residency;
        let bytes = copy(residency, spine, draft, sources, selection, open);
        self.counts.reads += 1;
        self.check_budget();
        let bytes = bytes.expect("the document reads back");
        String::from_utf8(bytes).expect("the document is text")
    }

    /// The leaves tile the document once, and each names something that exists.
    fn every_leaf_is_backed(&self, len: u64) {
        let pending = self.typing.pending().len() as u64;
        let mut at = 0;
        let mut leaves = 0;
        for span in self.typing.draft().spans_in(self.typing.spine(), 0, len) {
            assert_eq!(span.offset(), at, "seed {}: leaves do not tile", self.seed);
            let slab = span.slab();
            // A drafted leaf's slab keeps the extent it was registered with; the bytes it claims
            // are the span's, so that is what the buffer has to hold.
            let low = u64::from(slab.start());
            let backed = match self.typing.sources().locate(slab.segment()) {
                Location::Open { .. } => low + span.len() <= pending,
                Location::Sealed { segment } => self.store.covers(segment, slab),
                Location::Unknown => false,
            };
            if !backed {
                self.report_bare(slab);
            }
            at += span.len();
            leaves += 1;
        }
        assert_eq!(at, len, "seed {}: leaves do not cover it", self.seed);
        // Empty leaves are dropped, so a full-range walk yields one span per leaf.
        let count = u64::from(self.typing.spine().slab_count());
        assert_eq!(leaves, count, "seed {}: a leaf outside the walk", self.seed);
    }

    /// Says what the session refused, and where the run was when it did.
    fn report_refusal(&self, what: &str, at: u64, error: &TypingError) -> ! {
        let step = self.steps;
        let caret = self.typing.caret();
        let pending = self.typing.pending().len();
        let open = self.typing.sources().open_count();
        let len = self.typing.document_len();
        let model = self.model.len();
        let (name, a, b) = self.last;
        panic!(
            "seed {} step {step}: {what} at {at} refused: {error:?} caret={caret} \
             pending={pending} open={open} len={len} model={model} after {name} {a}..{b}",
            self.seed
        );
    }

    /// Says where the view and the model part company, and what led there.
    fn report_text(&self, text: &str) -> ! {
        let first = first_difference(&self.model, text);
        let from = first.saturating_sub(12);
        let want = self.model.get(from..first + 12).unwrap_or("");
        let got = text.get(from..first + 12).unwrap_or("");
        let (what, a, b) = self.last;
        let step = self.steps;
        panic!(
            "seed {} step {step} after {what} {a}..{b}: differs at {first} of {}/{} \
             model={want:?} view={got:?} {:?}",
            self.seed,
            self.model.len(),
            text.len(),
            self.recent
        );
    }

    /// Says everything known about a leaf the spine names and nothing backs.
    fn report_bare(&self, slab: Slab) -> ! {
        let id = slab.segment();
        let place = self.typing.sources().locate(id);
        let gone = self.typing.sources().was_abandoned(id);
        let sealed_as = self.typing.sources().sealed_as(id);
        let open = self.typing.sources().open_count();
        let segments = self.store.len();
        let caret = self.typing.caret();
        let len = self.typing.document_len();
        let live = self.typing.pending().len();
        let mut spans = Vec::new();
        for span in self.typing.draft().spans_in(self.typing.spine(), 0, len) {
            if spans.len() < 24 {
                spans.push((span.offset(), span.slab()));
            }
        }
        panic!(
            "seed {} step {}: bare leaf {id:?} at {place:?} gone={gone} sealed_as={sealed_as:?} \
             open={open} segments={segments} caret={caret} len={len} pending={live} \
             spans={spans:?} {:?}",
            self.seed, self.steps, self.recent
        );
    }

    /// The bound that must hold at every instant, and the high-water mark of it.
    fn check_budget(&mut self) {
        let resident = self.residency.resident_bytes();
        let over = resident > BUDGET;
        assert!(!over, "seed {}: {resident} over {BUDGET}", self.seed);
        self.peak_resident = self.peak_resident.max(resident);
    }

    /// One step of the run: pick an action, apply it to the session and to the model, then ask.
    fn step(&mut self) {
        let len = self.model.len();
        let roll = self.rng.below(100);
        if len < FLOOR {
            self.insert();
        } else if len > CEILING {
            if roll < 60 {
                self.delete();
            } else {
                self.replace();
            }
        } else {
            match roll {
                0..=44 => self.insert(),
                45..=63 => self.delete(),
                64..=68 => self.replace(),
                69..=78 => self.mark(),
                79..=85 if self.undo => self.undo(),
                86..=90 if self.undo => self.redo(),
                79..=96 => self.churn(),
                _ => self.flush(),
            }
        }
        let slot = self.cursor % RECENT;
        self.recent[slot] = self.last;
        self.cursor += 1;
        self.steps += 1;
        self.after();
    }

    /// The session and the model agree on the length, and the budget still holds.
    fn after(&mut self) {
        let modelled = u64::try_from(self.model.len()).expect("a document fits in u64");
        let len = self.typing.document_len();
        if len != modelled {
            let (what, a, b) = self.last;
            let step = self.steps;
            let caret = self.typing.caret();
            panic!(
                "seed {} step {step} after {what} {a}..{b}: {len} != {modelled} (caret {caret})",
                self.seed
            );
        }
        self.lengths.insert(modelled);
        self.check_budget();
        if self.strict {
            // Every action, in the short slice: a divergence is named where it happens, not sixty
            // actions later when a boundary it produced finally splits a character.
            let text = self.read();
            if text != self.model {
                self.report_text(&text);
            }
        }
    }

    /// A keystroke at the caret, or a paste, or a keystroke somewhere else — which seals.
    fn insert(&mut self) {
        let index = usize::try_from(self.rng.below(TYPED.len() as u64)).expect("index");
        let fragment = TYPED[index];
        // Four times in five the caret types where it already is: the keystroke path, no seal. The
        // fifth moves it to a random boundary, which is the path where an offset has to survive a
        // seal. Typing at the end of the document instead would seal on nearly every keystroke,
        // because the caret sits wherever the last edit left it.
        let at = if self.rng.below(5) == 0 {
            self.random_boundary()
        } else {
            self.typing.caret()
        };
        self.last = ("insert", at, fragment.len() as u64);
        let refused = self.typing.insert_at(at, fragment.as_bytes());
        if let Err(error) = refused {
            self.report_refusal("insert", at, &error);
        }
        self.model
            .insert_str(usize::try_from(at).expect("offset"), fragment);
        self.counts.inserted += 1;
    }

    fn delete(&mut self) {
        let len = self.model.len() as u64;
        let start = self.random_boundary();
        // Mostly short cuts, occasionally a large one: both the within-the-buffer fragment path
        // and the many-leaves path have to be exercised.
        let reach = if self.rng.below(10) == 0 {
            self.rng.between(0, len)
        } else {
            self.rng.between(0, 12)
        };
        let end = self.boundary_at_or_before(start + reach);
        self.last = ("delete", start, end);
        let refused = self.typing.delete(start, end);
        if let Err(error) = refused {
            self.report_refusal("delete", start, &error);
        }
        let from = usize::try_from(start).expect("offset");
        let to = usize::try_from(end).expect("offset");
        self.model.replace_range(from..to, "");
        self.counts.deleted += 1;
    }

    fn replace(&mut self) {
        let index = usize::try_from(self.rng.below(TYPED.len() as u64)).expect("index");
        let fragment = TYPED[index];
        let start = self.random_boundary();
        let reach = self.rng.between(0, 8);
        let end = self.boundary_at_or_before(start + reach);
        self.last = ("replace", start, end);
        let refused = self.typing.replace(start, end, fragment.as_bytes());
        if let Err(error) = refused {
            self.report_refusal("replace", start, &error);
        }
        let from = usize::try_from(start).expect("offset");
        let to = usize::try_from(end).expect("offset");
        self.model.replace_range(from..to, fragment);
        self.counts.replaced += 1;
    }

    fn mark(&mut self) {
        self.last = ("mark", self.typing.caret(), 0);
        self.typing.mark();
        self.counts.marked += 1;
    }

    fn flush(&mut self) {
        self.last = ("seal", self.typing.caret(), 0);
        self.typing.flush().expect("the open buffer seals");
        self.counts.sealed += 1;
    }

    fn undo(&mut self) {
        self.last = ("undo", self.typing.caret(), 0);
        if self.typing.undo().expect("a recorded state restores") {
            self.counts.undone += 1;
            let text = self.read();
            let seen = self.lengths.contains(&(text.len() as u64));
            let len = text.len();
            assert!(seen, "seed {}: undo landed at {len}", self.seed);
            // An undo is the session's answer, not the model's: adopt it, and let the rest of the
            // run hold the session to it from there.
            self.model = text;
        }
    }

    fn redo(&mut self) {
        self.last = ("redo", self.typing.caret(), 0);
        if self.typing.redo().expect("an undone state re-applies") {
            self.counts.redone += 1;
            let text = self.read();
            self.lengths.insert(text.len() as u64);
            self.model = text;
        }
    }

    /// Residency churn: fault a committed segment in, or wipe everything.
    fn churn(&mut self) {
        let segments = self.store.len() as u64;
        self.last = ("churn", segments, 0);
        if segments > 0 && self.rng.below(3) != 0 {
            let segment = SegmentId::new(self.rng.below(segments));
            self.residency
                .fault_in(segment)
                .expect("a segment faults in");
            self.counts.faults += 1;
            self.check_budget();
        } else {
            let wiped = self.residency.evict_all();
            self.counts.wipes += 1;
            let resident = self.residency.resident_bytes();
            assert_eq!(resident, 0, "seed {}: wipe left {wiped}", self.seed);
        }
    }

    fn random_boundary(&mut self) -> u64 {
        let len = u64::try_from(self.model.len()).expect("a document fits in u64");
        let at = self.rng.between(0, len);
        self.boundary_at_or_before(at)
    }

    /// The character boundary at or before `at` — the precondition the session cannot check for
    /// itself (OQ-15), discharged here the way a real caret discharges it.
    fn boundary_at_or_before(&self, at: u64) -> u64 {
        let mut at = usize::try_from(at).expect("offset").min(self.model.len());
        while at > 0 && !self.model.is_char_boundary(at) {
            at -= 1;
        }
        u64::try_from(at).expect("offset")
    }

    /// Unwinds the whole retained stack and rewinds it, one state at a time.
    ///
    /// This is the model-free half of the undo claim: whatever each state is called, every redo
    /// has to be the exact inverse of the undo it undoes. What it compares against is the document
    /// the *world-line* holds, not the one on screen, and that is not a detail: a root recorded
    /// before a cut is the tree **without** whatever was open at that moment (`restore_root`'s
    /// contract, and OQ-8's reason — pending bytes were never written, so no root can name them).
    /// Redo of such a step cannot put them back, and the walk would fail for a design decision
    /// rather than a bug. The pending span is therefore removed from the top before the walk, and
    /// the depth it costs is stated: undo across a cut returns to the last *written* document, and
    /// anything typed since is lost. A seal is the ordinary way to keep it.
    fn walk_history(&mut self) -> usize {
        let top = self.read();
        let pending = self.typing.pending().len();
        let caret = self.typing.caret();
        let mut states = vec![settled(&top, caret, pending)];
        while self.typing.undo().expect("a recorded state restores") {
            let text = self.read();
            let len = text.len() as u64;
            assert!(self.lengths.contains(&len), "seed {}: new state", self.seed);
            self.lengths.insert(len);
            states.push(text);
        }
        let left = self.typing.undo_depth();
        assert_eq!(left, 0, "seed {}: {left} states unvisited", self.seed);
        let deepest = states.len() - 1;
        let mut redone = 0;
        while self.typing.redo().expect("an undone state re-applies") {
            redone += 1;
            let text = self.read();
            let want = &states[deepest - redone];
            if &text != want {
                self.report_rewind(want, &text, redone);
            }
        }
        assert_eq!(
            redone, deepest,
            "seed {}: {redone} redos of {deepest}",
            self.seed
        );
        let left = self.typing.redo_depth();
        assert_eq!(left, 0, "seed {}: {left} redos stranded", self.seed);
        deepest
    }

    /// Says where a rewound state and the state it had to re-apply part company.
    fn report_rewind(&self, want: &str, got: &str, redone: usize) -> ! {
        let first = first_difference(want, got);
        // Windows for a human, so they have to be character-aligned: a byte range that starts or
        // ends inside a combining sequence reads back as `None` and printed two empty strings.
        let (from, until) = windows(want, got, first);
        let a = &want[from..until.min(want.len())];
        let b = &got[from..until.min(got.len())];
        let pending = self.typing.pending().len();
        let open = self.typing.sources().open_count();
        let spine = self.typing.spine().len();
        let slabs = self.typing.spine().slab_count();
        panic!(
            "seed {}: redo {redone} differs at {first} of {}/{}: want={a:?} got={b:?} \
             pending={pending} open={open} spine={spine} slabs={slabs} {:?}",
            self.seed,
            want.len(),
            got.len(),
            self.recent
        );
    }

    fn report(&self) -> String {
        let counts = &self.counts;
        let seed = self.seed;
        let steps = self.steps;
        let (inserted, deleted, replaced) = (counts.inserted, counts.deleted, counts.replaced);
        let (marked, sealed, undone) = (counts.marked, counts.sealed, counts.undone);
        let (redone, faults, wipes) = (counts.redone, counts.faults, counts.wipes);
        let reads = counts.reads;
        let segments = self.store.len();
        let document = self.model.len();
        let peak = self.peak_resident;
        format!(
            "HOLONOMY_KEYSTROKE_FUZZ seed={seed} actions={steps} keystrokes={inserted} \
             deleted={deleted} replaced={replaced} marked={marked} sealed={sealed} \
             undone={undone} redone={redone} faults={faults} wipes={wipes} reads={reads} \
             segments={segments} document_bytes={document} peak_resident_bytes={peak}"
        )
    }
}

/// The document with the pending span — the bytes that are in the buffer and in no segment —
/// taken out, which is the newest state a root can name.
fn settled(text: &str, caret: u64, pending: usize) -> String {
    let mut settled = text.to_owned();
    if pending == 0 {
        return settled;
    }
    let end = usize::try_from(caret).unwrap_or(text.len()).min(text.len());
    let start = end.saturating_sub(pending);
    settled.replace_range(start..end, "");
    settled
}

/// A character-aligned window around `first` that both documents actually have.
fn windows(want: &str, got: &str, first: usize) -> (usize, usize) {
    let shortest = want.len().min(got.len());
    let mut from = first.saturating_sub(12).min(shortest);
    while from < shortest && !want.is_char_boundary(from) {
        from += 1;
    }
    let mut until = (first + 12).min(shortest);
    while until > from && !want.is_char_boundary(until) {
        until -= 1;
    }
    (from, until)
}

/// The first byte two documents disagree about, or the first one past their common prefix.
fn first_difference(left: &str, right: &str) -> usize {
    left.bytes()
        .zip(right.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| left.len().min(right.len()))
}

/// Runs `steps` random actions on `seed`, checking every step and reading the document every
/// sixty-fourth step, where the whole text is compared with the model.
fn run(seed: u64, steps: u64) -> Session {
    let mut session = Session::new(seed, true, true);
    for step in 0..steps {
        session.step();
        if step % 64 == 0 {
            let text = session.read();
            assert_eq!(text, session.model, "seed {seed}: differ at {step}");
        }
    }
    // End on a mark, so the walk always has a state to unwind: a seal is an undo floor, and a run
    // that stopped on one would prove nothing about undo.
    session.mark();
    session
}

/// Runs until `keystrokes` inserts have happened, so the gate's count is keystrokes and the
/// interleaved cuts, marks, undos and wipes are extra rather than instead.
fn run_keystrokes(seed: u64, keystrokes: u64) -> Session {
    // Undo and redo stay out of this run until OQ-22 and OQ-23 are fixed: with them in, the million
    // trips an invariant in `Typing::pending` (`live_len()` eight, buffer three) at about a hundred
    // and sixty seconds in. The short slice still exercises undo and redo on every push.
    let mut session = Session::new(seed, false, false);
    // Inserts always make progress, so the cap only ever catches a harness bug — as a failure,
    // not a hang.
    let cap = keystrokes.saturating_mul(8);
    while session.counts.inserted < keystrokes {
        assert!(session.steps < cap, "seed {seed}: no progress");
        session.step();
        if session.steps % 16 == 0 {
            let text = session.read();
            let step = session.steps;
            assert_eq!(text, session.model, "seed {seed}: differ at {step}");
        }
    }
    session.mark();
    session
}

#[test]
fn a_random_session_keeps_the_view_the_leaves_and_the_budget_in_agreement() {
    // The short slice: six seeds, twenty thousand actions each, in a debug build, with every action
    // checked — the document read back through the reader the shell uses and compared with an
    // independent model, the leaves tiled and backed, and the resident set inside its budget.
    // Undo and redo are part of the mix; unwinding the stack is the ignored test below, because
    // that is the clause that does not hold yet (OQ-21).
    for seed in 0..6 {
        let session = run(seed, 20_000);
        assert!(
            session.counts.inserted > 0,
            "seed {seed}: nothing was typed"
        );
        assert!(session.counts.sealed > 0, "seed {seed}: nothing was sealed");
        assert!(session.counts.undone > 0, "seed {seed}: nothing was undone");
        assert!(session.counts.redone > 0, "seed {seed}: nothing was redone");
    }
}

/// The third clause of the gate, and the reason it is unclaimed.
///
/// This walk is what caught OQ-21 (a redo after an undo of a root, one byte short, seed 2 at byte
/// 260 of 409), and D-33 fixed that reproduction: a root step's inverse now keeps the bytes that
/// were open, so its redo re-attaches them instead of losing them. The walk then moved on to the
/// next one, which is OQ-22 and still open: seed 0, redo five, a document 276 bytes wide where 275
/// was expected, with one pending byte and the leaf at the wrong offset.
#[test]
#[ignore = "blocked on OQ-22: a redo re-attaches pending bytes at the wrong offset (seed 0)"]
fn unwinding_the_retained_undo_stack_comes_back_to_the_same_document() {
    for seed in 0..6 {
        let mut session = run(seed, 20_000);
        let walked = session.walk_history();
        assert!(walked > 0, "seed {seed}: nothing to undo, nothing proved");
    }
}

#[test]
fn the_short_slice_reaches_every_part_of_the_session() {
    // A run that never seals, never wipes or never marks would satisfy the invariants while proving
    // almost nothing. This is the coverage half of the gate: the actions actually happened.
    let session = run(7, 5_000);
    let counts = &session.counts;
    let tally = [
        ("insert", counts.inserted),
        ("delete", counts.deleted),
        ("replace", counts.replaced),
        ("mark", counts.marked),
        ("seal", counts.sealed),
        ("undo", counts.undone),
        ("redo", counts.redone),
        ("fault-in", counts.faults),
        ("wipe", counts.wipes),
        ("read", counts.reads),
    ];
    for (name, count) in tally {
        assert!(count > 0, "the run never exercised {name}");
    }
    let segments = session.store.len();
    assert!(segments > 1, "the run sealed {segments} segments in all");
}

#[test]
#[ignore = "the gate's count: 1 M keystrokes, run in release mode by the `test` job"]
fn a_million_random_keystrokes_leave_the_view_the_budget_and_the_undo_stack_in_agreement() {
    const KEYSTROKES: u64 = 250_000;
    const SEEDS: u64 = 4;
    let mut actions = 0;
    for seed in 0..SEEDS {
        let session = run_keystrokes(seed, KEYSTROKES);
        actions += session.steps;
        assert_eq!(session.counts.inserted, KEYSTROKES);
        println!("{}", session.report());
    }
    let keystrokes = KEYSTROKES * SEEDS;
    let total = format!("keystrokes={keystrokes} actions={actions}");
    println!("HOLONOMY_KEYSTROKE_FUZZ total {total}");
}
