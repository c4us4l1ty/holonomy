//! Three differential models for the pieces everything else stands on.
//!
//! A fuzz run that only checks what the session believes about itself can be wrong in the same
//! direction twice. These three tests carry an independent answer instead: the spine against a
//! `Vec<u8>` of the document, the overlay against arithmetic done by hand, and the resident buffer
//! against a `String`. Each one asks for *equality with the model*, not for an invariant that both
//! sides could break together.
//!
//! Every case is seeded, so a failure replays exactly, and every assertion names the seed, the step
//! and the edit, because the first thing a fuzz failure has to be is a sentence.

use std::collections::BTreeMap;

use holonomy_text::{Draft, EditOutcome, SegmentId, Slab, SlabText, Spine};

/// A seeded generator, so no dependency and no clock enters the test.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(
            seed.wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407)
                | 1,
        )
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..bound`, or zero when there is nothing to choose from.
    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next() % bound
        }
    }

    /// A value in `low..=high`.
    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low + 1)
    }
}

/// A stand-in for the world-line: every segment's plaintext, so a spine can be materialised.
#[derive(Default)]
struct Tape {
    segments: Vec<Vec<u8>>,
}

impl Tape {
    /// A fresh segment of `len` random bytes, returned as one slab that covers it.
    fn add(&mut self, len: usize, rng: &mut Rng) -> Slab {
        let bytes: Vec<u8> = (0..len).map(|_| rng.below(256) as u8).collect();
        let segment = SegmentId::new(self.segments.len() as u64);
        self.segments.push(bytes);
        Slab::new(segment, 0, len as u32)
    }

    fn bytes(&self, slab: Slab) -> &[u8] {
        let segment = &self.segments[slab.segment().get() as usize];
        &segment[slab.start() as usize..slab.end() as usize]
    }

    /// The document a spine describes, read through `spans_in` and nothing else.
    fn materialise(&self, spine: &Spine) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = 0;
        for span in spine.spans_in(0, spine.len()) {
            assert_eq!(span.offset(), at, "spans_in must tile from zero");
            out.extend_from_slice(self.bytes(span.slab()));
            at += span.len();
        }
        assert_eq!(at, spine.len(), "spans_in must reach the document's end");
        out
    }
}

/// Everything that must be true of a spine and its model, checked after every edit.
fn agrees(tape: &Tape, spine: &Spine, model: &[u8], seed: u64, step: u64, rng: &mut Rng) {
    let shown = format!("seed {seed} step {step}");
    assert_eq!(spine.len() as usize, model.len(), "{shown}: document length");
    assert_eq!(tape.materialise(spine), model, "{shown}: document bytes");

    let spans = spine.spans_in(0, spine.len());
    assert_eq!(
        spans.len() as u32,
        spine.slab_count(),
        "{shown}: every leaf must appear once"
    );
    for (index, span) in spans.iter().enumerate() {
        assert!(!span.is_empty(), "{shown}: an empty leaf survived at {index}");
        // The O(log n) index has to agree with the walk that produced the spans.
        assert_eq!(
            spine.slab_span(index as u32),
            Some(*span),
            "{shown}: slab_span({index})"
        );
        let first = spine.locate(span.offset()).expect("a leaf's first byte is in the document");
        assert_eq!(first.slab(), span.slab(), "{shown}: locate({})", span.offset());
        assert_eq!(first.within(), 0, "{shown}: locate({}) within", span.offset());
        let last = span.offset() + span.len() - 1;
        let last = spine.locate(last).expect("a leaf's last byte is in the document");
        assert_eq!(last.slab(), span.slab(), "{shown}: the last byte of {index}");
        assert_eq!(last.within(), span.len() as u32 - 1);
    }
    assert_eq!(spine.locate(spine.len()), None, "{shown}: past the end");

    for _ in 0..4 {
        if spine.is_empty() {
            break;
        }
        let at = rng.below(spine.len());
        let hit = spine.locate(at).expect("inside the document");
        let span = spans
            .iter()
            .find(|span| span.offset() <= at && at < span.offset() + span.len())
            .expect("a leaf covers every offset");
        assert_eq!(hit.slab(), span.slab(), "{shown}: locate({at})");
        assert_eq!(hit.within(), (at - span.offset()) as u32);
    }
}

#[test]
fn the_spine_materialises_what_the_model_says_at_every_edit() {
    for seed in 0..8 {
        let mut rng = Rng::new(seed);
        let mut tape = Tape::default();
        let mut spine = Spine::empty();
        let mut model: Vec<u8> = Vec::new();
        for step in 0..120 {
            match rng.below(6) {
                0 | 1 => {
                    let slab = tape.add(rng.range(1, 40) as usize, &mut rng);
                    model.extend_from_slice(tape.bytes(slab));
                    spine = spine.append(slab);
                }
                2 | 3 => {
                    let slab = tape.add(rng.range(1, 40) as usize, &mut rng);
                    let at = rng.range(0, model.len() as u64);
                    let bytes = tape.bytes(slab).to_vec();
                    model.splice(at as usize..at as usize, bytes);
                    spine = spine.insert_slab(at, slab).expect("in range");
                }
                4 => {
                    let start = rng.range(0, model.len() as u64);
                    let end = rng.range(start, model.len() as u64);
                    model.drain(start as usize..end as usize);
                    spine = spine.remove_range(start, end).expect("in range");
                }
                _ => {
                    let start = rng.range(0, model.len() as u64);
                    let end = rng.range(start, model.len() as u64);
                    let slab = tape.add(rng.range(0, 24) as usize, &mut rng);
                    let bytes = tape.bytes(slab).to_vec();
                    model.splice(start as usize..end as usize, bytes);
                    spine = spine.replace_range(start, end, slab).expect("in range");
                }
            }
            agrees(&tape, &spine, &model, seed, step, &mut rng);
        }
    }
}

#[test]
fn splitting_at_every_offset_gives_the_model_s_halves_and_concat_puts_them_back() {
    for seed in 0..4 {
        let mut rng = Rng::new(seed);
        let mut tape = Tape::default();
        let mut spine = Spine::empty();
        let mut model: Vec<u8> = Vec::new();
        for _ in 0..12 {
            let slab = tape.add(rng.range(1, 9) as usize, &mut rng);
            model.extend_from_slice(tape.bytes(slab));
            spine = spine.append(slab);
        }
        for at in 0..=spine.len() {
            let (before, after) = spine.split_at(at).expect("in range");
            assert_eq!(
                tape.materialise(&before),
                model[..at as usize],
                "seed {seed}: the head at {at}"
            );
            assert_eq!(
                tape.materialise(&after),
                model[at as usize..],
                "seed {seed}: the tail at {at}"
            );
            assert_eq!(
                tape.materialise(&before.concat(&after)),
                model,
                "seed {seed}: concat at {at}"
            );
        }
    }
}

#[test]
fn rebuilding_from_slabs_coalesces_contiguous_neighbours_and_moves_no_byte() {
    for seed in 0..6 {
        let mut rng = Rng::new(seed);
        let mut tape = Tape::default();
        tape.segments.push(vec![b'z'; 200]);
        let mut slabs = Vec::new();
        let mut model: Vec<u8> = Vec::new();
        for _ in 0..30 {
            let start = rng.below(200) as u32;
            let len = rng.range(0, u64::from(200 - start)) as u32;
            let slab = Slab::new(SegmentId::new(0), start, len);
            model.extend_from_slice(tape.bytes(slab));
            slabs.push(slab);
        }
        // The model of a rebuild: the non-empty runs, merged where they are contiguous.
        let mut expected: Vec<Slab> = Vec::new();
        for slab in slabs.iter().copied().filter(|slab| !slab.is_empty()) {
            match expected.last_mut() {
                Some(tail) if tail.end() == slab.start() => {
                    *tail = Slab::new(tail.segment(), tail.start(), tail.len() + slab.len());
                }
                _ => expected.push(slab),
            }
        }
        let spine = Spine::from_slabs(slabs.iter().copied());
        assert_eq!(
            spine.spans_in(0, spine.len())
                .iter()
                .map(|span| span.slab())
                .collect::<Vec<_>>(),
            expected,
            "seed {seed}: the rebuilt leaves"
        );
        assert_eq!(tape.materialise(&spine), model, "seed {seed}: the rebuilt bytes");
        let spans = spine.spans_in(0, spine.len());
        for pair in spans.windows(2) {
            assert!(
                !(pair[0].slab().end() == pair[1].slab().start()),
                "seed {seed}: two leaves still name contiguous bytes"
            );
        }
    }
}

#[test]
fn the_overlay_length_is_the_sum_of_what_it_drafted() {
    for seed in 0..6 {
        let mut rng = Rng::new(seed);
        let mut tape = Tape::default();
        let mut spine = Spine::empty();
        for _ in 0..12 {
            let slab = tape.add(rng.range(1, 20) as usize, &mut rng);
            spine = spine.append(slab);
        }
        let mut draft = Draft::new();
        let mut live: BTreeMap<u64, u64> = BTreeMap::new();
        for _ in 0..40 {
            let spans = spine.spans_in(0, spine.len());
            let span = spans[(rng.below(spans.len() as u64)) as usize];
            let length = rng.range(0, u64::from(span.slab().len()) + 8);
            draft.record(span, length).expect("leaves do not overlap");
            live.insert(span.offset(), length);
        }

        // The model: every leaf sealed at its own length except where the overlay says otherwise.
        let mut expected = 0;
        for span in spine.spans_in(0, spine.len()) {
            expected += live.get(&span.offset()).copied().unwrap_or(span.len());
        }
        assert_eq!(draft.document_len(&spine), expected, "seed {seed}: document length");

        // And the spans walk that length, with the drafted leaves at their live lengths.
        let mut at = 0;
        for span in draft.spans_in(&spine, 0, expected) {
            assert_eq!(span.offset(), at, "seed {seed}: the overlay's spans must tile");
            if span.is_draft() {
                assert_eq!(
                    Some(span.len()),
                    live.get(&at).copied(),
                    "seed {seed}: a drafted leaf's live length"
                );
                assert_eq!(draft.live_len(at), Some(span.len()));
            }
            at += span.len();
        }
        assert_eq!(at, expected, "seed {seed}: the overlay's spans must reach the end");
    }
}

const TYPED: &[&str] = &[
    "a",
    "e\u{301}",
    "\u{1f44d}\u{200d}\u{1f469}",
    "\u{200d}",
    " ",
    "\n",
    "\u{6f22}",
    "\u{e9}",
];

/// The nearest character boundary at or below `at`.
fn boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[test]
fn the_resident_buffer_matches_a_string_and_refuses_what_it_cannot_hold() {
    for seed in 0..6 {
        let mut rng = Rng::new(seed);
        for capacity in [1usize, 2, 3, 4, 5, 8, 16, 64] {
            let mut slab = SlabText::from_plaintext(Vec::new(), capacity).expect("empty is text");
            let mut model = String::new();
            for step in 0..60 {
                let shown = format!("seed {seed} capacity {capacity} step {step}");
                assert_eq!(slab.capacity(), capacity, "{shown}: capacity moved");
                match rng.below(3) {
                    0 | 1 => {
                        let fragment = TYPED[(rng.below(TYPED.len() as u64)) as usize];
                        let at = boundary(&model, rng.below(model.len() as u64 + 1) as usize);
                        let fits = model.len() + fragment.len() <= capacity;
                        let outcome = slab.insert(at, fragment.as_bytes());
                        if fits {
                            assert_eq!(
                                outcome.expect("it fits"),
                                EditOutcome::InPlace,
                                "{shown}: insert {at} of {fragment:?}"
                            );
                            model.insert_str(at, fragment);
                        } else {
                            assert!(
                                matches!(outcome, Ok(EditOutcome::WouldSplit { .. })),
                                "{shown}: insert {at} of {fragment:?} cannot fit and must be \
                                 refused, got {outcome:?}"
                            );
                        }
                    }
                    2 => {
                        let start = boundary(&model, rng.below(model.len() as u64 + 1) as usize);
                        let reach = rng.range(start as u64, model.len() as u64);
                        let end = boundary(&model, reach as usize);
                        slab.delete(start, end).expect("a character range deletes");
                        model.replace_range(start..end, "");
                    }
                    _ => {
                        // Somewhere that is not a boundary, which must be refused rather than cut.
                        if model.len() < 2 {
                            continue;
                        }
                        let mut at = rng.below(model.len() as u64) as usize;
                        while at < model.len() && model.is_char_boundary(at) {
                            at += 1;
                        }
                        if at >= model.len() {
                            continue;
                        }
                        assert!(
                            slab.insert(at, b"x").is_err(),
                            "{shown}: insert inside a character must be refused"
                        );
                        assert_eq!(slab.as_str(), model, "{shown}: a refusal changed the text");
                        assert!(
                            slab.delete(at, at + 1).is_err(),
                            "{shown}: delete inside a character must be refused"
                        );
                        assert_eq!(slab.as_str(), model, "{shown}: a refusal changed the text");
                    }
                }
                assert_eq!(slab.as_str(), &model[..], "{shown}: the buffer");
                assert_eq!(slab.len(), model.len(), "{shown}: the length");
            }
        }
    }
}
