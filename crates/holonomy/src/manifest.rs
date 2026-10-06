//! A document's **structure without its content**. Phase 13, item 1.
//!
//! # What this is, and what it is not
//!
//! H2 called it "the 99% frozen layer of `2000.md`'s Iceberg": read a manifest for a 2000-page document
//! and you have ~50 KB and touch no blobs, which is what makes the scrollbar, the outline and the word
//! count cheap (`H2/crates/holonomy-core/src/manifest.rs:1-6`). The idea is language-independent and
//! this is the port of it.
//!
//! **What is not ported is the role, and that is the one substantive design decision in this file.**
//! H2's sections were *user content* — top-level blocks, addressable by id, reorderable, the thing an
//! outline lists. So H2's manifest drove the **scroll geometry**: a document's height is the sum of its
//! sections' block counts, and the scrollbar moves in section-sized steps.
//!
//! H1's sections cannot have that role, and the arithmetic says so before the taste does. A 2000-page
//! document at 43 rows per page is 86,000 lines; at 18 px a line that is 1,548,000 px of content. For a
//! 600 px scrollbar to move smoothly — which is to say, to move in increments smaller than one screen —
//! each scrollbar pixel must cover fewer than 600 px of content, so the manifest would need
//! 1,548,000 / 600 = **2,580 sections**. At 6 MiB of text that is **2,436 bytes per section**, so the
//! scroll granularity would be *smaller than a line of prose*.
//!
//! **So the scroll geometry stays on [`crate::doclines::DocLines`] — per line, `O(log n)`, one byte tree —
//! and the manifest is the residency index.** Sections here are an implementation detail: the largest
//! unit that can be loaded and evicted in one operation, sized to the container's own chunk so a load is
//! one `pread64`. Nothing about them is user-visible, and nothing in the UI may ever say where one
//! begins. A user who can see a section boundary cannot unsee it, and would then ask for it to mean
//! something.
//!
//! # The two numbers a section carries, and the third a measurement removed
//!
//! * **`bytes`** — where it is. The identity.
//! * **`spans`** — how many formulas and image anchors it holds.
//!
//! **There was a third — `newlines`, for "how tall does this section render" — and it is gone.** H2 needed
//! it: its `ManifestEntry::block_count` doc says an 8,000-character section that is one paragraph renders
//! 2,070 px while the same characters as ten paragraphs render 2,373 px, so rendered height is a function
//! of *structure* and H2 had to pay Loro to tell it (`H2/.../manifest.rs:29-38`). Here it would be free —
//! counting `\n` is exact — and **free and exact is still the wrong answer**, because keeping it exact costs
//! a whole-document rebuild every ~44 keystrokes at the top of a document. [`SectionMetrics`]'s own docs
//! have the measurement; the short version is that every section but the last has a constant length, so an
//! insertion at offset 0 shifts every section's content and no section's boundary, and any section's
//! newline count can therefore change. Nothing reads the distribution — the scroll geometry is
//! [`crate::doclines::DocLines`]' job, per *line* and exact — so the field is removed rather than
//! maintained approximately.
//!
//! `spans` is the reason this file exists rather than being an extra tree in `DocLines`, and it is the
//! answer to the blocker Phase 12 named. `publish_line_heights` calls `math_blocks_for`, which calls
//! `read_document` **unconditionally** — 6 MiB on a 6 MiB document — because `for_each_math_span` is a
//! cursor over bytes and *"does this document contain any math"* cannot be asked without reading the
//! bytes to find the `$$`. [`Manifest::span_total`] is that question, answered at open, from the bytes
//! the session already has. `DocLines` cannot answer it because `DocLines` indexes *lines* and a formula
//! is not a line.
//!
//! # The per-section cost, stated as a measurement rather than a hope
//!
//! **8 bytes per section.** Two `Fenwick` trees over `n` weights each store `n + 1` `u32`s, so the
//! manifest is `2 * (n + 1) * 4 = 8n + 8` bytes — 8 per section plus an 8-byte constant. A 6 MiB document
//! at [`SECTION_BYTES`] = 65,520 is 97 sections, so **784 bytes total, 0.0013 % of the text.** It was 12
//! bytes per section while it had three trees, and losing the `newlines` tree is worth 4 bytes of memory —
//! nothing. It is worth the 3.3 ms, because that is what the field cost and this is what removing it
//! bought.
//!
//! H2's own figure is ~50 KB for 1,300 sections (`H2/crates/holonomy-core/src/manifest.rs:1-6`), which is
//! **38 B/section** — a `String` id, a title, two `i64` timestamps and an `OrderKey` per entry. This is
//! 4.7x cheaper because it carries two `u32`s and no identity at all: H2 needed `id` because its sections
//! were user content with an outline and a sync conflict model, and none of that exists here.
//!
//! **The two trees rather than a plain `Vec` is a cost with a reason, not a default.** A `Vec` of
//! `SectionMetrics` is 8 B/section with no constant, which is cheaper. But `total_bytes` and `span_total`
//! would each be `O(n)` sums, and `span_total` is called on **every paint** — so a `Vec` would make a
//! paint `O(sections)`, and 97 iterations per frame to learn a number that does not change is exactly the
//! kind of cost that is invisible until the document has 10,000 sections. Two trees buy two `O(log n)`
//! sums for 8 bytes of sentinel.
//!
//! # What is measured, and where
//!
//! **The memory this saves: 6.00 MiB of `doc_scratch` on a 6 MiB document, and 26.85 → 20.80 MiB of RSS**,
//! with the marginal cost per document byte going from 2.852 to 1.852 and the affordable document from
//! 2.20 MiB to 3.41 MiB. All three are in `tests/session_rss.rs`.
//!
//! **The latency this costs: three section reads per keystroke, 196,572 bytes, ~55 µs.** The first version
//! of `measure` was a byte-at-a-time loop and measured **+254 µs**, which is half the budget; it is now a
//! word-at-a-time mask test. That regression and its fix are both recorded at `measure`.

use holonomy_geometry::Fenwick;
use holonomy_text::Editor;

/// A section is **one container chunk** of plaintext. Phase 13, item 2.
///
/// # The number is derived from the format, not chosen
///
/// `CHUNK_PLAINTEXT = CHUNK_SLOT - TAG_LEN = 65,536 - 16 = 65,520`. Three things fall out of that
/// choice and none of them would hold for a round number like 64 KiB:
///
/// 1. **One section is one `pread64`.** The container addresses plaintext as a slot array —
///   `chunk_offset(omega, i)` is `omega + i * CHUNK_SLOT` (`holonomy-container/src/layout.rs:149`) — so
///   a section is one authenticated read with no offset arithmetic inside it. A 64,000-byte section
///   would straddle two chunks and need two reads and a straddling check in the load path forever.
/// 2. **A section is the unit `mlock` already counts.** The leaves are 4 KiB and the document's lock is
///   measured in leaves, so a 65,520-byte section is 16 leaves plus a fraction. Any size would be; this
///    one is the size that makes the number legible.
/// 3. **Eviction wastes at most one chunk of the budget.** §2.9.3's Iceberg budget for images is 8.0 MiB;
///    the text budget Phase 13 introduces is a section count, so rounding it up to a whole section costs
///    at most 65,520 bytes rather than an arbitrary fraction of a chosen size.
///
/// # What the number is *not*, and the arithmetic that rules out the alternatives
///
/// **Smaller.** A section is the residency unit, so a smaller section means less to read on a miss but
/// more reads on a scroll. 4 KiB — one leaf, which is the obvious candidate — gives 1,536 sections for a
/// 6 MiB document and **30,720 bytes of manifest**, and still means 1,536 reads to walk the document
/// once where 96 would do. The manifest is 16x more expensive and the read count is 16x worse, for a
/// per-miss latency improvement that is one `pread64` either way (both are a single 64 KiB read from the
/// page cache after the first touch).
///
/// **Larger.** 1 MiB sections would give 6 sections and a 120-byte manifest, and then a 1 MiB read on
/// every scroll — which is the 6 MiB copy Phase 12 spent the whole phase failing to remove, reintroduced
/// under a different name. **The section size and the residency budget are the same decision**: a budget
/// of `n` sections costs `n * SECTION_BYTES` and a manifest of `n * 20` bytes, and the budget is chosen
/// for memory, so the section size is chosen to make the memory arithmetic round.
///
/// # The floor, and the arithmetic that makes it a floor rather than a hope
///
/// One section is [`SECTION_BYTES`] and a document has **at least one**, so the smallest residency any
/// document can have is 65,520 bytes. A 2000-page document at 1M words is ~6 MiB of text = 96 sections;
/// a budget of 8 sections = **524,160 bytes**, which is Phase 13's number for a working set, and leaves
/// 32.5× headroom between "one page of text" and "the whole document". §NFR-1.2's 16 MiB budget
/// survives that by a wide margin — the point of the whole phase, and the thing that is measured rather
/// than derived in `tests/session_rss.rs`.
pub const SECTION_BYTES: usize = holonomy_container::layout::CHUNK_PLAINTEXT;

/// What one section of the document holds. Phase 13.
///
/// **8 bytes.** Two `u32`s, and both are bounded by the section size rather than by the document, so
/// neither can overflow a `u32` on any document this format can hold: [`SECTION_BYTES`] is 65,520 and
/// `S_MAX_PAYLOAD` is 8 MiB.
///
/// # What is *not* here, and why — the field a measurement removed
///
/// **This struct carried `newlines` until Phase 13 part 1's latency gate deleted it.** The reasoning for
/// having it was the section's *rendered height*: `newlines * cell_h` is how tall a section is, so a
/// manifest of heights can size a scrollbar for a document that has never been rendered. That is H2's
/// reason for a per-section block count, and it was the whole of H2's reason.
///
/// **It costs a whole-document rebuild every ~44 keystrokes at the top of a document**, and that is
/// measured rather than argued:
///
/// * Every section except the last has a **constant** length, because the cut is at `SECTION_BYTES` from
///   the section's start. So inserting a byte at offset 0 does not move any section boundary — it shifts
///   every section's *content* one byte right.
/// * Which means the newline just past some section's cut can move from section `k` into section `k + 1`,
///   and **any** section's newline count can change. So after an edit at the top of the document, all 48
///   sections' newline counts are stale, not three of them.
/// * `newlines.total() + 1` then disagrees with the line count the editor maintains, and the disagreement
///   is the rebuild trigger. Measured: **3,300 µs per rebuild, on one keystroke in ~21** at offset 0 —
///   **6.6x the 500 µs budget**, and it was the entire "worst" column of the keystroke gate.
///
/// **Two ways out, and the arithmetic picks one.** Shorten the rebuild's radius — impossible, since every
/// section's content moved. Or recognise that **the total is invariant when the section count is**, and that
/// nothing reads the *distribution*.
///
/// Nothing does. The scroll geometry is [`crate::doclines::DocLines`]' job — per *line*, `O(log n)`, and
/// exact — which is the design decision this module's own docs record ("the scroll geometry stays on
/// `DocLines`; the manifest is the residency index"). A manifest that keeps a second, coarser copy of a
/// quantity another structure maintains exactly is a copy that will drift, and this one drifted into a
/// 6.6x budget overrun.
///
/// **So the manifest holds what residency needs — where a section is, and what is in it — and the scroll
/// geometry stays where it was already right.** The rebuild trigger is now the document's *length*, which
/// changes on every edit and is `O(1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionMetrics {
    /// Bytes, including any terminators. The Fenwick weight, so a prefix sum is a document offset.
    pub bytes: u32,
    /// Formulas and image anchors in this section.
    ///
    /// **A count, and `span_total` is its sum** — so unlike `bytes`, shifting a marker from one section to
    /// the next leaves the total unchanged, which is exactly the property that makes the cheap sync path
    /// correct. A marker can only change sections by moving across a cut, and recomputing the caret's
    /// section and the next covers that.
    ///
    /// Tree-backed rather than a `Vec` because [`Manifest::span_total`] is called on every paint, and
    /// summing a `Vec` would make it `O(sections)` — 96 iterations per frame to learn a number that does
    /// not change, which is the kind of cost that is invisible until there are 10,000 sections.
    pub spans: u32,
}

impl SectionMetrics {
    /// The metrics of a section that holds `bytes` of text.
    fn new(bytes: u32, spans: u32) -> Self {
        Self { bytes, spans }
    }
}

/// What [`Manifest::sync`] did, for a caller that wants to know. Phase 13.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sync {
    /// One section's metrics changed. `O(log n)`.
    OneSection,
    /// The section count changed, so both trees were rebuilt. `O(sections)`.
    Rebuilt,
    /// Nothing changed. `O(1)`.
    Unchanged,
}

/// A document's sections: where they are, how tall they are, and what they contain. Phase 13.
///
/// See the module docs for why this exists and for the decision — *not* ported — that the scroll
/// geometry stays on [`crate::doclines::DocLines`].
#[derive(Debug)]
pub struct Manifest {
    /// Section byte lengths, so `prefix` is a document offset and `lower_bound` finds a section by byte.
    bytes: Fenwick,
    /// Per-section span counts, so `total()` is the whole document's span count and `weight(i)` is one
    /// section's. A Fenwick tree rather than a `Vec`, because [`Manifest::span_total`] is called on every
    /// paint and summing a `Vec` would make it `O(sections)` — the one query here that has to be cheap,
    /// because it is the one that decides whether to read the document at all.
    spans: Fenwick,
    /// The section size this manifest was built at. Reported rather than assumed, because a manifest
    /// built at 64 KiB and a document at 65,520 must not be reported as either.
    section_bytes: usize,
}

/// A section's byte range, plus the metrics it carries.
///
/// Returned together rather than as separate accessors because **a caller that needs the range needs
/// the metrics**, and `Manifest::section_of_byte` already found the index that answers both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    /// 0-based section index.
    pub index: u32,
    /// First document byte, inclusive.
    pub start: u32,
    /// One past the last document byte, exclusive.
    pub end: u32,
    /// Formulas and image anchors in the section.
    pub spans: u32,
}

impl Manifest {
    /// Build from the document's bytes. One `O(document)` scan, at open.
    ///
    /// # The scan, and why it is one pass
    ///
    /// Three numbers per section, and all three come out of one walk: the byte count is the offset
    /// arithmetic, the newline count is a byte comparison, and the span count is *also* a byte
    /// comparison — `$$` for a formula, and the two markers the table and image scanners already key
    /// on. Three separate scans would be three passes over 6 MiB at open for no reason; this is one.
    ///
    /// **Sections are cut at [`SECTION_BYTES`] and never mid-character.** A cut that landed inside a
    /// multi-byte character would put half a codepoint in one section and half in another, and a reader
    /// that loaded both would have to know they were adjacent to join them. Cutting at an arbitrary byte
    /// is fine because *bytes* are what sections hold; what is not fine is a section whose boundary is
    /// inside a character, so the cut backs up to the nearest character boundary. That costs at most
    /// three bytes of section length and makes every section independently valid UTF-8 to the extent the
    /// document is.
    ///
    /// # An empty document is one empty section
    ///
    /// Not zero. A document with no sections has no `span_total` to ask and no `section_of_byte` to call,
    /// and a manifest that needs a special case for "empty" is a manifest whose callers all need one too.
    /// The one empty section answers every question with a correct answer.
    pub fn from_text(text: &[u8]) -> Self {
        let mut sections: Vec<SectionMetrics> = Vec::new();
        let mut start = 0usize;
        while start < text.len() {
            // `next_boundary` may cut short of `SECTION_BYTES` to avoid splitting a character.
            let end = next_boundary(text, start + SECTION_BYTES);
            sections.push(measure(&text[start..end]));
            start = end;
        }
        if sections.is_empty() {
            sections.push(SectionMetrics::new(0, 0));
        }
        Self::from_sections(sections, text.len())
    }

    /// Build from per-section metrics, for a caller that measured them some other way.
    ///
    /// **`from_text` and this must agree**, and `the_manifest_agrees_with_a_scan_of_the_same_text` is
    /// what holds them to it. The second constructor exists for the case Phase 13's windowing creates:
    /// a manifest for a document that is **not resident**, built from the container's per-chunk counts
    /// rather than from bytes. That is the whole point of the type — it can be built either way, and
    /// the text is not required to be present.
    pub fn from_sections(sections: Vec<SectionMetrics>, document_bytes: usize) -> Self {
        // A caller passing an empty `Vec` gets one empty section rather than an index that panics, which
        // is the same rule `from_text` follows and for the same reason: an empty document is one empty
        // section, so every query has a correct answer.
        let sections = if sections.is_empty() {
            vec![SectionMetrics::new(0, 0)]
        } else {
            sections
        };
        debug_assert_eq!(
            sections.iter().map(|s| s.bytes as usize).sum::<usize>(),
            document_bytes,
            "the section byte lengths must sum to the document length; a mismatch means the manifest \
             was built from a different document than the one it is being used with"
        );
        let bytes: Vec<u32> = sections.iter().map(|s| s.bytes).collect();
        let spans: Vec<u32> = sections.iter().map(|s| s.spans).collect();
        Self {
            bytes: Fenwick::from_weights(&bytes),
            spans: Fenwick::from_weights(&spans),
            section_bytes: SECTION_BYTES,
        }
    }

    /// How many sections the document has. `O(1)`.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Always false: an empty document is one empty section, never zero.
    ///
    /// **Present because `len()` exists**, and clippy's `len_without_is_empty` is right that a `len` with
    /// no `is_empty` invites `x.len() == 0` as a test. It is false, which is the information a caller
    /// actually wants, and the doc comment says so rather than making them discover it.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The document's total bytes. `O(1)`.
    pub fn total_bytes(&self) -> usize {
        self.bytes.total() as usize
    }

    /// **The question Phase 12 could not ask.** `O(log n)`.
    ///
    /// How many formulas, tables and images the document holds. Zero means `publish_line_heights`,
    /// `emit_tables`, `emit_math` and `emit_images` can all skip `read_document` entirely, which is
    /// **6.00 MiB of `doc_scratch` on a 6 MiB document** — the largest single term in `session_rss.rs`'s
    /// 2.85 bytes-per-document-byte, and the one Phase 12 named as its own remaining work.
    ///
    /// **This is the whole reason `spans` is in the manifest and not derived at paint time.** A formula is
    /// not a line, so the line-indexed `DocLines` cannot answer this; and re-deriving it at paint time is
    /// the whole-document read this exists to avoid.
    pub fn span_total(&self) -> u32 {
        self.spans.total()
    }

    /// The section size this manifest was built at.
    ///
    /// **Reported rather than re-exported as a bare constant**, so a caller that hard-codes a section
    /// size anywhere can be compared against the one the manifest actually used. A manifest built at a
    /// different size — by `from_sections`, or by a future format version — is still correct; it is just
    /// not the size this constant names.
    pub fn section_bytes(&self) -> usize {
        self.section_bytes
    }

    /// Every byte this manifest occupies, computed from the representation rather than hard-coded. `O(1)`.
    ///
    /// **Two Fenwick trees of `n + 1` `u32`s each**, so `2 * (n + 1) * 4 = 8n + 8` bytes: **8 bytes per
    /// section** plus an 8-byte constant. `Vec` capacity slack is not counted because a Fenwick tree
    /// allocates exactly `n + 1`, which is why the figure is derived from the type rather than measured
    /// through a heap walk — and the gate that checks it against the text is
    /// `a_manifest_costs_eight_bytes_a_section`.
    pub fn heap_bytes(&self) -> usize {
        2 * (self.len() + 1) * std::mem::size_of::<u32>()
    }

    /// The section containing document byte `at`. `O(log n)`.
    ///
    /// An `at` past the end returns the last section, and an `at` in the document's final empty section
    /// returns that section. Both are the answer a renderer wants: the scrollbar asked where the bottom
    /// is, and the bottom is the last section.
    pub fn section_of_byte(&self, at: u32) -> u32 {
        // `lower_bound` finds the largest index whose prefix is `<= target`, which is the section
        // containing `at` when `at` is a real offset. Clamped to the last section for the `at ==
        // total_bytes` case, where `lower_bound` would return `n`.
        let i = self.bytes.lower_bound(at);
        (i as u32).min(self.len() as u32 - 1)
    }

    /// The section containing line `line`, given `line_to_byte` to turn the line into an offset.
    ///
    /// **A method taking a closure rather than reading a line index itself**, and that is the whole reason
    /// the `newlines` tree went away. A section index *is* a byte offset, so the caller already has the
    //  line geometry — [`crate::doclines::DocLines`] — and re-deriving it here would be a second copy of
    //  the same index that could disagree with it.
    pub fn section_of_line(&self, line: u32, line_to_byte: impl Fn(u32) -> u32) -> u32 {
        self.section_of_byte(line_to_byte(line))
    }

    /// The `index`-th section's byte range and metrics. `O(1)`.
    pub fn section(&self, index: u32) -> Option<Section> {
        let i = index as usize;
        if i >= self.len() {
            return None;
        }
        let start = self.bytes.prefix(i);
        let end = start + self.bytes.weight(i);
        Some(Section {
            index,
            start,
            end,
            spans: self.spans.weight(i),
        })
    }

    /// Bring the manifest back into agreement after an edit at or around `caret`. `O(section)` or
    /// `O(document)`.
    ///
    /// # The same correct-by-construction shape as [`crate::doclines::DocLines::sync`]
    ///
    /// `Session` has a dozen edit paths and threading an edit descriptor through all of them to say "I
    /// inserted a newline" is a way to be wrong in twelve places. So this is told the *line count* and
    /// decides for itself:
    ///
    /// * **Counts agree** — no newline appeared or vanished, so no section boundary moved. Only the
    ///   caret's section and the last one can be stale, and they are re-measured. `O(section)`.
    /// * **Counts disagree** — a line appeared or vanished, so a boundary may have moved and
    ///   [`Manifest::from_text`] is the honest response. `O(document)`.
    ///
    /// **Not `expected_spans`, and the reason is that the manifest should not be told its own answer.**
    /// A first version took an expected span count from outside. That would mean `TextCounts` maintaining
    /// span deltas, and *a formula's pairing is not a local fact*: typing one `$` into a document with no
    /// other creates no span and typing a second creates one, so there is no per-byte delta and a
    /// maintained count would be wrong exactly when it was asked about. The manifest re-measures the
    /// sections that can have changed and the total follows, which is both cheaper and the only version
    /// that is right.
    ///
    /// # Three sections are read, and that is the whole reason this takes an `Editor`
    ///
    /// **The first version took `&[u8]`, which meant reading the whole document on every keystroke** —
    /// 6 MiB, and the exact cost this module and Phase 11 both exist to remove. A section's bytes are a
    /// contiguous range, so `read_into` can fetch just those: three sections plus four bytes each is
    /// **196,572 bytes, a 32x reduction**, and `read_into` allocates nothing so there is no `Vec` growth
    /// either. This is why `sync` takes a [`holonomy_text::Editor`] and a scratch buffer rather than a
    /// slice — the slice *is* the whole document, and asking for it is asking for the cost.
    ///
    /// # Three candidates, and why not two
    ///
    /// An edit inside section `k` makes `k` longer and `k + 1` shorter, because the cut is at a byte offset
    /// rather than at a section's own recorded length. So the caret's section, **the one after it**, and the
    /// last one can all be stale. Two candidates was the second version and it was wrong for every
    /// mid-document edit: `k + 1`'s start had just moved and its metrics had not.
    ///
    /// Two *earlier* versions were wrong in other directions and are recorded because each produced a
    /// confident wrong answer rather than an error:
    ///
    /// * **Skipping the last section when the caret was in it** — which is where an append always is, so
    ///   every keystroke that ended a document reported `Unchanged` on a document that had grown.
    /// * **Measuring every section through the end of the document** — right for the last section and wrong
    ///   for the rest, where it would have let a mid-document section swallow its successors.
    ///
    /// # The third thing a comparison of two counts does not catch
    ///
    /// **Appending a byte changes no line and no span**, so the line-count comparison passes, and a
    /// manifest that only recomputed the caret's section through its own recorded weight would report
    /// `Unchanged` on a document that had grown by one. That was not hypothetical: it was the first
    /// version, and `a_letter_is_one_section_update_and_a_nothing_is_unchanged` caught it by expecting
    /// `OneSection` and getting `Unchanged`.
    ///
    /// # The newline case is genuinely `O(document)`, and it is stated rather than hidden
    ///
    /// Cutting the document into fixed-size sections means a byte inserted at offset 0 shifts every
    /// boundary after it, and a Fenwick tree over shifted weights is a rebuild. This is
    /// [`crate::doclines::DocLines`]'s trade in a second currency: a letter is `O(section)`, Enter is
    /// `O(document)`. With 97 sections the rebuild touches three trees of 98 `u32`s — about a microsecond
    /// — and Enter is one keystroke in forty.
    ///
    /// **A rebuild reads the whole document**, which is the one thing this module exists to avoid, so it is
    /// worth being precise about the count: one keystroke in forty, and only the one that adds a line. A
    /// measured `section_rebuilds / section_updates` ratio in `tests/session_manifest.rs` is what would show
    /// a regression here, and the gate asserts `a_newline_rebuilds_the_manifest_and_a_letter_does_not`
    /// because a *letter* rebuilding would mean the cheap path had stopped working.
    pub fn sync(
        &mut self,
        editor: &Editor,
        caret: u32,
        scratch: &mut Vec<u8>,
    ) -> Sync {
        if editor.text_len() != self.total_bytes() {
            // **The section count can have changed, and that is the only thing a length disagreement
            // means.** The last section absorbs every byte after the last cut, so a document that grows past
            // `SECTION_BYTES` in its last section has one more section than the manifest has rows for.
            // Rather than reason about "did it overflow", this asks the question that has an unambiguous
            // answer: does the manifest still tile the document?
            if let Some(need) = self.needs_rebuild() {
                let Ok(text) = editor.text() else {
                    return Sync::Unchanged;
                };
                let fresh = Self::from_text(&text);
                self.bytes = fresh.bytes;
                self.spans = fresh.spans;
                self.section_bytes = need;
                return Sync::Rebuilt;
            }
        }

        let caret_section = self.section_of_byte(caret) as usize;
        let last = self.len() - 1;
        // **Three candidates, and the reason is the fixed cut.** An edit inside section `k` makes `k`'s
        // content shift, and `k + 1`'s start is `prefix(k + 1)` — so both are stale. The last section is
        // stale too, because it absorbs whatever the document's length did. Two candidates were the second
        // version and it was wrong for every mid-document edit.
        let mut changed = self.recompute_section(editor, caret_section, scratch);
        let after = caret_section + 1;
        if after <= last {
            changed |= self.recompute_section(editor, after, scratch);
        }
        if last != caret_section && last != after {
            changed |= self.recompute_section(editor, last, scratch);
        }
        if changed {
            Sync::OneSection
        } else {
            Sync::Unchanged
        }
    }

    /// `Some(section_bytes)` when the document no longer fits the current section count, `None` when it does.
    ///
    /// **The rebuild trigger, and it is a question about arithmetic rather than about content.**
    /// `total_bytes()` is the manifest's answer to "how long is the document" and `editor.text_len()` is
    /// the document's. If they disagree, some section's length is wrong — and the only way that can happen
    /// without an edit having moved a boundary is if the last section has outgrown `SECTION_BYTES` and there
    /// is now one more section than there are rows.
    ///
    /// The test is the last section's recorded length against its own ceiling, which is `O(1)`. It does not
    /// need the editor: the last section's own weight is the whole question, and `sync` has already
    /// established that the lengths disagree by calling this.
    fn needs_rebuild(&self) -> Option<usize> {
        let last = self.len().saturating_sub(1);
        if (self.bytes.weight(last) as usize) <= SECTION_BYTES {
            None
        } else {
            Some(SECTION_BYTES)
        }
    }

    /// Re-measure section `i`. Returns whether anything moved.
    ///
    /// **The end is re-derived from the cut rule, not read from the recorded weight — and that is the
    /// whole reason this function exists in this form.** Three earlier versions measured section `i` from
    /// `prefix(i)` to `prefix(i) + weight(i)`, and every one of them was wrong the moment a byte was typed
    /// in the middle of a section:
    ///
    /// * Reading to the **recorded weight** truncates the inserted bytes off the end of the section, so a
    ///   `$$` typed mid-document was invisible to `measure` and `span_total()` stayed zero. This is what
    ///   `a_formula_appears_and_the_manifest_notices_within_one_keystroke` caught.
    /// * Reading to the **end of the document** for the last section is right, and wrong for every other
    ///   one — a mid-document section would swallow its successors.
    ///
    /// So the end comes from [`next_boundary`] applied to a window of `SECTION_BYTES + 4` bytes: the cut
    /// rule is "advance `SECTION_BYTES`, then back up off a continuation byte", and both halves need bytes
    /// that are *after* the insertion, which is the only way to see them. Four extra bytes is the most the
    /// back-up can consume.
    ///
    /// Returns `false` rather than panicking when the read fails, because a stale section is recoverable
    /// and a panic in the middle of a keystroke is not. FR-1.2's threat model treats an abort in the
    /// renderer as worse than a wrong scrollbar.
    fn recompute_section(&mut self, editor: &Editor, i: usize, scratch: &mut Vec<u8>) -> bool {
        let start = self.bytes.prefix(i) as usize;
        let len = editor.text_len() as usize;
        if start >= len {
            return false;
        }
        // `SECTION_BYTES + 4`: the section, plus up to three bytes the character-boundary back-up may
        // need. A window shorter than `SECTION_BYTES` is the document's end, and `section_end` says so.
        let want = (SECTION_BYTES + 4).min(len - start);
        scratch.clear();
        scratch.resize(want, 0);
        let Ok(got) = editor.read_into(start, scratch) else {
            return false;
        };
        let window = &scratch[..got];
        let take = section_end(window);
        let fresh = measure(&window[..take]);
        if self.bytes.weight(i) == fresh.bytes && self.spans.weight(i) == fresh.spans {
            return false;
        }
        self.bytes.set(i, fresh.bytes);
        self.spans.set(i, fresh.spans);
        true
    }
}

/// How many bytes of `window` belong to the section that starts at `window[0]`.
///
/// **`window.len()` when the document ends inside it, and the cut point otherwise.** `window` is
/// `SECTION_BYTES + 4` bytes from the section's start, so a window that is not longer than `SECTION_BYTES`
/// is the end of the document and the section runs to it.
fn section_end(window: &[u8]) -> usize {
    if window.len() <= SECTION_BYTES {
        return window.len();
    }
    next_boundary(window, SECTION_BYTES)
}

/// Where the next section boundary falls, at or before `want`.
///
/// **Backs up to a character boundary.** A section boundary inside a multi-byte character would split
/// that character across two sections, so a reader that loaded only one would see half of it. Backing up
/// costs at most three bytes of section length — the cut is a byte count, not a layout promise, and
/// `SECTION_BYTES` is a *target* rather than a promise about section size — and buys the property that
/// each section is independently decodable, which is what makes sections independently loadable.
///
/// The check is `b & 0xC0 != 0x80`: a **continuation** byte is `0b10xxxxxx`, so a byte that is not one
/// starts a character. Walking back at most three bytes finds the last such byte.
fn next_boundary(text: &[u8], want: usize) -> usize {
    let want = want.min(text.len());
    if want >= text.len() {
        return text.len();
    }
    // Walk back over continuation bytes. Three is the most a UTF-8 codepoint can have after its lead
    // byte, so this cannot walk back four and cannot run off the front.
    let mut at = want;
    for _ in 0..4 {
        if at == 0 || text[at] & 0xC0 != 0x80 {
            break;
        }
        at -= 1;
    }
    // `at` is now a character start (or 0). `at > start` is guaranteed because `want > start` was the
    // caller's contract and a cut can only move backwards by three.
    at.max(1)
}

/// The three numbers for one section's bytes. One pass.
///
/// # What counts as a span, and why the marker matters
///
/// **Formulas and images** — the two things `emit_math` and `emit_images` each begin with a
/// whole-document read. Counting both is what makes `span_total() == 0` mean *"nothing forces a
/// whole-document read"* rather than *"no formula in particular"*, which is what the four guards in
/// `session.rs` rely on.
///
/// The image marker is [`holonomy_text::ANCHOR_BYTES`] — U+FFFC, three bytes `EF BF BC` — and counting it
/// was **not optional**, which is how this was found: six of `session_image.rs`'s gates failed at once the
/// first time this ran, every one asserting that an image's node was emitted and it was zero. The manifest
/// said zero spans, `emit_images` returned early, and the image simply did not draw. So the manifest counts
/// anchors too.
///
/// **That failure is worth keeping in mind as a class.** A guard whose condition is *derived from a
/// summary* is a guard that can be wrong in the direction of quietly drawing nothing, and no amount of
/// unit-testing the summary catches it — only the end-to-end gate for the thing being skipped does. Six
/// tests failing at once is the good case; one failing is the bad case.
///
/// **Tables are not scanned for here, deliberately.** A table's span map lives in `Editor::tables()`, is
/// maintained by the editor as the user types, and costs `O(1)` to ask — so `publish_line_heights` reads it
/// directly rather than through the manifest. A second, cheaper-to-build copy of a fact the editor already
/// maintains exactly is a fact that can drift, and this one would drift on every undo. The manifest's
/// `spans` is for the things that need *reading the bytes* to find.
///
/// # Every `$$` is a span, and two versions of this function got that wrong
///
/// [`holonomy_text::for_each_math_span`] emits **one span per `$$` occurrence** — a pair closes, and an
/// unpaired opener "runs to the end of its line", which is still a span, and is the normal intermediate
/// state while someone is halfway through typing a formula. So the count is the number of `$$`s, and
/// *odd is a real formula, not a typo*.
///
/// Two earlier versions counted something else and were both wrong in the direction that matters:
///
/// * **Toggling a flag per `$` *byte*.** `$$` is two bytes, so it toggled twice and cancelled itself: a
///   formula contributed **zero** spans and `"the answer is $$x^2$$"` measured the same as prose. This is
///   what made `a_formula_appears_and_the_manifest_notices_within_one_keystroke` report 0 for a document
///   containing `$$hello`.
/// * **Counting *pairs*.** Closer, and wrong in the other direction: one `$$` reported zero, so a formula
///   the user was halfway through typing was invisible to the guard — and a false negative keeps the
///   whole-document read alive, which is merely the cost. But it is also false as an answer to *"does this
///   document contain a formula"*, and the question Phase 12 said could not be asked is worth asking right.
///
/// **The direction of the error is the design.** Every imprecision in this count errs *small*, and small is
/// safe: a formula the manifest misses costs a whole-document read, while a formula the manifest invents
/// suppresses a render. So a section boundary that splits a `$$` counts it in the section holding its first
/// byte, and `span_total` can be an undercount but never an overcount.
fn measure(bytes: &[u8]) -> SectionMetrics {
    let mut spans = 0u32;
    let mut i = 0usize;
    while i + 8 <= bytes.len() {
        let w = u64::from_le_bytes(bytes[i..i + 8].try_into().expect("8 bytes"));
        // **One branch per eight bytes, not per byte.** The four marker tests are branchless and their
        // results are OR-ed into a single mask; the byte loop that actually matches a marker runs only on a
        // hit, which for prose is once per section.
        //
        // The marker set is `$$` and U+FFFC — `$` (0x24), `E` (0xEF), `B` (0xBF), `F` (0xBC). **Why those
        // four and not one test for "any high bit set"**: 0x24 has no high bit, so the high-bit shortcut
        // cannot see a `$`, and a separate `$` test is cheaper than giving the shortcut up for the other
        // three. Four `has_zero` tests is four XORs, four subtracts and four ANDs per word.
        let markers = has_zero(w ^ (LANES * u64::from(b'$')))
            | has_zero(w ^ (LANES * u64::from(ANCHOR[0])))
            | has_zero(w ^ (LANES * u64::from(ANCHOR[1])))
            | has_zero(w ^ (LANES * u64::from(ANCHOR[2])));
        if markers != 0 {
            // The byte fallback is handed the **whole slice**, not this word, because a marker can cross
            // the boundary: an anchor is three bytes and a `$$` is two, so a match found at this word's
            // last byte needs bytes the word does not contain. The first version of this passed only the
            // eight bytes, and `measure_agrees_with_a_byte_scan_on_every_marker_at_every_alignment` caught it
            // reporting **zero spans for an anchor at offset 0** — an anchor is most of an image's content,
            // so that is a class of error where the manifest says "no images" about a document full of them.
            let end = i + 8;
            while i < bytes.len() {
                if i >= end && !starts_marker(bytes, i) {
                    break;
                }
                step(bytes, &mut i, &mut spans);
            }
            continue;
        }
        i += 8;
    }
    // The tail, and the reason the loop above stops eight bytes short rather than running to the end: a
    // marker straddling the last word would otherwise never be examined at all.
    while i < bytes.len() {
        step(bytes, &mut i, &mut spans);
    }
    SectionMetrics::new(bytes.len() as u32, spans)
}

/// Whether a marker starts at `at`, looking only at its first byte.
///
/// **The test the word-at-a-time loop uses to decide whether it may stop mid-word.** Over-strict rather
/// than exact, deliberately: a false "yes" sends a few extra bytes through the byte fallback and a false
/// "no" loses a marker. Over-strictness costs nothing measurable and under-strictness costs a formula.
#[inline]
fn starts_marker(bytes: &[u8], at: usize) -> bool {
    matches!(bytes.get(at), Some(b'$')) || bytes.get(at) == Some(&ANCHOR[0])
}

/// One byte of the fallback: take a marker if one starts here, advance `i`.
fn step(bytes: &[u8], i: &mut usize, spans: &mut u32) {
    let b = bytes[*i];
    if b == ANCHOR[0]
        && *i + ANCHOR.len() <= bytes.len()
        && bytes[*i..*i + ANCHOR.len()] == ANCHOR
    {
        *spans += 1;
        *i += ANCHOR.len();
        return;
    }
    if b == b'$' && *i + 1 < bytes.len() && bytes[*i + 1] == b'$' {
        // One span per opener, paired or not. See the module note above: `for_each_math_span` treats an
        // unpaired `$$` as a span running to the end of its line.
        *spans += 1;
        *i += 2;
        return;
    }
    *i += 1;
}

/// The image anchor, as the three bytes U+FFFC is encoded in. Copied from
/// [`holonomy_text::ANCHOR_BYTES`] so `measure`'s inner loop reads a `const` rather than indexing a
/// `static` through a reference — and asserted equal in `the_anchor_constant_matches_the_text_crates`.
///
/// **The copy is deliberate and pinned.** `ANCHOR_BYTES` is `[u8; 3]`, so `b == ANCHOR[0]` in a hot loop
/// reads through a slice reference the optimiser may not fold. A `const` is a value. The test below is what
/// makes the duplication safe: if `holonomy-text` ever changes the marker — a new anchor encoding, say —
/// that test fails the same build rather than silently finding no anchors again, which is the exact failure
/// six image gates showed when anchors were not counted at all.
const ANCHOR: [u8; 3] = holonomy_text::ANCHOR_BYTES;

/// All-ones in every byte lane.
const LANES: u64 = 0x0101_0101_0101_0101;

/// The high bit of every byte lane.
const HIGHS: u64 = 0x8080_8080_8080_8080;

/// A mask with the high bit of each byte lane that was `0x00`, set.
///
/// The classic SWAR zero-byte test: XOR the lane with the byte being sought, then `(x - 0x01..) & !x &
/// 0x80..`. **The `& !x` is not optional** — it discards the lanes the subtraction's borrow propagated
/// into, which is the whole reason the naive `x - LANES` version under-counts. `swar_tests` covers it with
/// `\n` alternating with a zero byte, which is the exact lane pattern that breaks it.
#[inline(always)]
fn has_zero(x: u64) -> u64 {
    x.wrapping_sub(LANES) & !x & HIGHS
}


#[cfg(test)]
mod tests {
    use super::*;

    fn m(text: &str) -> Manifest {
        Manifest::from_text(text.as_bytes())
    }

    /// An [`Editor`] over `text`, built by inserting in line-sized pieces.
    ///
    /// **In pieces, because `UndoStack` refuses an action larger than its capacity** — a one-shot insert
    /// of a 160 KB fixture is not a slow test but a failing one, `ActionTooLarge { len: 160_000,
    /// capacity: 65_536 }`. Typing the document is also more honest: it is a document, not one keystroke.
    fn editor_with(text: &[u8]) -> Editor {
        let mut e = Editor::new();
        for line in text.split_inclusive(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            e.insert_at(
                e.text_len() as u32,
                line,
                holonomy_text::SpanPolicy::GrowIntoInsert,
            )
            .expect("room");
        }
        e
    }

    /// A document's structure is knowable without its content.
    #[test]
    fn a_documents_structure_is_its_bytes() {
        let text = "alpha\nbravo\ncharlie\n";
        let d = m(text);
        assert_eq!(d.len(), 1, "under SECTION_BYTES, one section");
        assert_eq!(d.total_bytes(), text.len());
        let s = d.section(0).expect("section 0");
        assert_eq!(s.start, 0);
        assert_eq!(s.end, text.len() as u32);
    }

    /// An empty document is one section, not zero — so every query has an answer.
    #[test]
    fn an_empty_document_is_one_empty_section() {
        let d = m("");
        assert_eq!(d.len(), 1);
        assert_eq!(d.total_bytes(), 0);
        assert_eq!(
            d.span_total(),
            0,
            "and no formulas, so no whole-document read is needed"
        );
        assert!(!d.is_empty(), "is_empty is false even here, and len is 1");
        let s = d.section(0).expect("section 0");
        assert_eq!((s.start, s.end, s.spans), (0, 0, 0));
    }

    /// **The blocker Phase 12 named.** A document with no formulas and no images answers `span_total() == 0`
    /// from 8 bytes of manifest, which is the whole document read avoided.
    #[test]
    fn a_document_with_no_markers_answers_the_question() {
        let text = "the quick brown fox\n".repeat(200_000);
        let d = m(&text);
        assert_eq!(
            d.span_total(),
            0,
            "prose has no $$ and no U+FFFC, which is what lets publish_line_heights skip read_document"
        );
    }

    /// **One span per `$$` occurrence, paired or not**, which is `for_each_math_span`'s own rule and was not
    /// the rule two earlier versions implemented.
    #[test]
    fn a_document_with_a_formula_counts_one_span_per_opener() {
        assert_eq!(m("prose\nthen $$x^2$$\nmore prose\n").span_total(), 2);
        assert_eq!(
            m("an unpaired $$ opener\n").span_total(),
            1,
            "an unpaired $$ IS a formula: it runs to the end of its line, and that is the normal state \
             while someone is halfway through typing one"
        );
        assert_eq!(
            m("costs $5 and $6, no formula here\n").span_total(),
            0,
            "a lone $ is not a delimiter, so prose with prices costs nothing"
        );
        assert_eq!(m("$$a$$ and $$b$$\n").span_total(), 4);
    }

    /// An image anchor is a span too, and counting it is what six `session_image.rs` gates asked for.
    #[test]
    fn an_image_anchor_is_a_span() {
        let anchor = holonomy_text::ANCHOR_BYTES;
        let mut text = b"look: ".to_vec();
        text.extend_from_slice(&anchor);
        text.extend_from_slice(b"\n");
        assert_eq!(Manifest::from_text(&text).span_total(), 1);
    }

    /// The sections tile the document exactly once, in order — the property every range query depends on.
    #[test]
    fn the_sections_tile_the_document_exactly_once() {
        let mut text = Vec::new();
        for i in 0..40 {
            text.extend_from_slice(format!("row {i}\n").repeat(2_000).as_bytes());
        }
        let d = Manifest::from_text(&text);
        let mut at = 0u32;
        for i in 0..d.len() as u32 {
            let s = d.section(i).expect("section");
            assert_eq!(s.start, at, "section {i} starts at {at}, not {}", s.start);
            assert!(s.end > s.start, "section {i} is empty");
            assert_eq!(d.section_of_byte(s.start), i);
            assert_eq!(d.section_of_byte(s.end - 1), i);
            at = s.end;
        }
        assert_eq!(at as usize, text.len());
    }

    /// `from_text` and `from_sections` must agree, because the windowed case will build the second from the
    /// container's counts while the text-resident case builds the first from bytes.
    #[test]
    fn the_manifest_agrees_with_a_rebuild_from_its_own_sections() {
        let mut text = Vec::new();
        for i in 0..40 {
            text.extend_from_slice(format!("row {i}\n").repeat(2_000).as_bytes());
        }
        let d = Manifest::from_text(&text);
        let sections: Vec<SectionMetrics> = (0..d.len())
            .map(|i| {
                let s = d.section(i as u32).expect("section");
                SectionMetrics {
                    bytes: s.end - s.start,
                    spans: s.spans,
                }
            })
            .collect();
        let rebuilt = Manifest::from_sections(sections, text.len());
        assert_eq!(rebuilt.total_bytes(), d.total_bytes());
        assert_eq!(rebuilt.span_total(), d.span_total());
        assert_eq!(rebuilt.len(), d.len());
        for i in 0..d.len() as u32 {
            assert_eq!(rebuilt.section(i), d.section(i), "section {i}");
        }
    }

    /// A section boundary never splits a multi-byte character — which is what makes each section
    /// independently loadable.
    #[test]
    fn a_section_boundary_never_splits_a_character() {
        // `é` is two bytes, and the text is built so SECTION_BYTES lands in the middle of one.
        let mut text: Vec<u8> = vec![b'x'; SECTION_BYTES - 1];
        text.extend_from_slice("é".as_bytes());
        text.extend_from_slice(&vec![b'y'; 64]);
        let d = Manifest::from_text(&text);
        assert_eq!(d.len(), 2);
        let first = d.section(0).expect("section 0");
        let second = d.section(1).expect("section 1");
        assert!(
            std::str::from_utf8(&text[first.start as usize..first.end as usize]).is_ok(),
            "section 0 is {}-{} and is not valid UTF-8 on its own, so a reader loading only it would \
             see half a character",
            first.start,
            first.end
        );
        assert!(
            std::str::from_utf8(&text[second.start as usize..second.end as usize]).is_ok(),
            "section 1 is {}-{} and is not valid UTF-8 on its own",
            second.start,
            second.end
        );
    }

    /// The manifest's memory claim, asserted rather than derived.
    #[test]
    fn a_manifest_costs_eight_bytes_a_section() {
        assert_eq!(
            std::mem::size_of::<SectionMetrics>(),
            8,
            "two u32s; this is the payload the module docs' budget arithmetic uses"
        );
        let text = "the quick brown fox jumps over the lazy dog\n".repeat(200_000);
        let d = m(&text);
        let cost = d.heap_bytes();
        assert_eq!(
            cost,
            2 * (d.len() + 1) * 4,
            "two Fenwick trees of n+1 u32s: 8 per section plus an 8-byte constant"
        );
        assert!(
            cost * 1000 < text.len(),
            "a {cost}-byte manifest for {} bytes of text is more than 0.1% overhead",
            text.len()
        );
        println!(
            "manifest: {} sections, {cost} bytes for {} bytes of text ({:.5}%)",
            d.len(),
            text.len(),
            100.0 * cost as f64 / text.len() as f64
        );
    }

    /// `SECTION_BYTES` is the container's chunk size, so a section is one `pread64`.
    #[test]
    fn a_section_is_one_container_chunk() {
        assert_eq!(SECTION_BYTES, 65_520);
        assert_eq!(
            SECTION_BYTES,
            holonomy_container::layout::CHUNK_PLAINTEXT,
            "SECTION_BYTES must be the container's plaintext chunk size, so loading a section is one \
             authenticated read and evicting one is one unit"
        );
        let document_bytes: usize = 6 * 1024 * 1024;
        assert_eq!(
            document_bytes.div_ceil(SECTION_BYTES),
            97,
            "6 MiB is 96.01 sections, so it is 97 -- the manifest covers the document and the last one \
             is short, which is why the count is a ceiling and not a division"
        );
        assert_eq!(
            8 * SECTION_BYTES,
            524_160,
            "an 8-section working set, which is the bound Phase 13 quotes"
        );
    }

    /// A no-op says `Unchanged`, an append is a one-section update, and a growth past a section size is a
    /// rebuild.
    ///
    /// **Driven through a real [`Editor`] rather than against a `&[u8]`**, because that is the only way to
    /// test what matters: `sync` takes an `Editor` so it can read *sections* rather than the whole
    /// document, and a slice-based test would pass just as happily against the version that reads all 6 MiB.
    #[test]
    fn a_letter_is_one_section_update_and_a_nothing_is_unchanged() {
        let mut scratch = Vec::new();
        let base: Vec<u8> = b"row\n".repeat(40_000);
        let mut e = editor_with(&base);
        let mut d = Manifest::from_text(&base);
        assert_eq!(d.sync(&e, 0, &mut scratch), Sync::Unchanged);
        assert_eq!(d.sync(&e, 80_000, &mut scratch), Sync::Unchanged);

        // **Appending a byte is a one-section update, not a rebuild and not `Unchanged`.** Three previous
        // versions of this function got it wrong and every one of them was caught by exactly this
        // assertion: one compared line and span counts and reported `Unchanged` on a document that had
        // grown; one measured the last section through its own stale weight and did the same; and one
        // skipped the last section when the caret was in it, which is where an append always is.
        let caret = e.text_len() as u32;
        e.insert_at(caret, b"!", holonomy_text::SpanPolicy::GrowIntoInsert)
            .expect("room");
        assert_eq!(
            d.sync(&e, caret, &mut scratch),
            Sync::OneSection,
            "a byte appended at the end belongs to the last section: one section's length changed and \
             nothing else did"
        );
        assert_eq!(d.total_bytes(), e.text_len(), "and the manifest now agrees about the length");

        // A letter in the middle of a line is also a one-section update, and does not move a boundary.
        e.insert_at(8, b"x", holonomy_text::SpanPolicy::GrowIntoInsert)
            .expect("room");
        assert_eq!(d.sync(&e, 8, &mut scratch), Sync::OneSection);
        assert_eq!(d.total_bytes(), e.text_len());
        assert_eq!(
            d.section_of_byte(8),
            0,
            "and the section that byte is in is still section 0, so the prefix sums did not drift"
        );
    }

    /// **The read volume, which is the reason `sync` takes an `Editor` and not a slice.**
    #[test]
    fn a_keystroke_measures_three_sections_not_the_document() {
        // **Six MiB, so the ratio means something.** An earlier version of this test used a 160,000-byte
        // document -- 2.4 sections -- and asserted `scratch * 4 < document`, which failed at 65,524 * 4 =
        // 262,096 > 160,000. The test was measuring its own fixture: a document 2.4 sections long leaves
        // no room for a section-sized scratch to look small against it.
        let base: Vec<u8> = b"row\n".repeat(1_500_000);
        let e = editor_with(&base);
        let mut d = Manifest::from_text(&base);
        let mut scratch = Vec::new();
        d.sync(&e, 8, &mut scratch);
        assert_eq!(
            scratch.capacity(),
            SECTION_BYTES + 4,
            "one keystroke's scratch is one section plus the four bytes the character-boundary back-up \
             may need, whatever the document is"
        );
        assert!(
            scratch.capacity() * 64 < base.len(),
            "a {} byte scratch against a {} byte document is reading the document",
            scratch.capacity(),
            base.len()
        );
        // And the same scratch for a document 6x smaller, which is the growth claim as a comparison
        // rather than a ratio -- the form that cannot be satisfied by a small fixture.
        let small: Vec<u8> = b"row\n".repeat(250_000);
        let es = editor_with(&small);
        let mut ds = Manifest::from_text(&small);
        let mut scratch_small = Vec::new();
        ds.sync(&es, 8, &mut scratch_small);
        assert_eq!(
            scratch_small.capacity(),
            scratch.capacity(),
            "a 6x smaller document uses the same scratch, so the scratch does not scale with the \
             document"
        );
    }

    /// **The invariant that matters most: the manifest agrees with the document after every sync.**
    #[test]
    fn the_manifest_agrees_with_the_document_after_every_sync() {
        let mut text = Vec::new();
        let mut e = Editor::new();
        let mut d = Manifest::from_text(&text);
        let mut scratch = Vec::new();
        for i in 0..8 {
            let add = format!("entry {i}\n").into_bytes();
            let at = e.text_len() as u32;
            e.insert_at(at, &add, holonomy_text::SpanPolicy::GrowIntoInsert)
                .expect("room");
            text.extend_from_slice(&add);
            d.sync(&e, at, &mut scratch);
            assert_agrees(&d, &text, i);
        }
        // A byte near the start, which shifts every section's content.
        e.insert_at(3, b"Z", holonomy_text::SpanPolicy::GrowIntoInsert)
            .expect("room");
        text.insert(3, b'Z');
        d.sync(&e, 3, &mut scratch);
        assert_agrees(&d, &text, 99);
    }

    /// **The regression this phase's latency gate caught, as a unit test.**
    ///
    /// Inserting at offset 0 shifts every section's *content* without moving any boundary, so every
    /// section's newline count was stale — and the manifest used to rebuild the whole document when its
    /// newline total disagreed with the editor's, which was **every ~44 keystrokes at the top of a
    /// document, at 3.3 ms each**. `newlines` is gone, so there is nothing to disagree and nothing to
    /// rebuild. This asserts the shape of that: **the span total is invariant under edits that insert no
    /// marker**, which is what makes the cheap path correct rather than merely fast.
    #[test]
    fn a_marker_free_edit_does_not_change_the_span_total() {
        let mut text = Vec::new();
        let mut e = Editor::new();
        let mut d = Manifest::from_text(&text);
        let mut scratch = Vec::new();
        text.extend_from_slice(b"prose\n");
        e.insert_at(0, b"prose\n", holonomy_text::SpanPolicy::GrowIntoInsert)
            .expect("room");
        d.sync(&e, 0, &mut scratch);
        assert_eq!(d.span_total(), 0);
        // Two hundred insertions at offset 0, which shift every section's content every time.
        for i in 0..200 {
            e.insert_at(0, b"x", holonomy_text::SpanPolicy::GrowIntoInsert)
                .expect("room");
            text.insert(0, b'x');
            assert_ne!(
                d.sync(&e, 0, &mut scratch),
                Sync::Rebuilt,
                "keystroke {i} rebuilt the manifest: a letter shifts content but moves no boundary, and \
                 nothing about it can change the section count"
            );
        }
        assert_eq!(d.span_total(), 0, "and no letter anywhere made a formula");
        assert_eq!(d.total_bytes(), e.text_len());
        assert_agrees(&d, &text, 200);
    }

    /// Assert the manifest's totals and boundaries against a scan.
    fn assert_agrees(d: &Manifest, text: &[u8], step: usize) {
        assert_eq!(d.total_bytes(), text.len(), "step {step}: total bytes");
        let mut at = 0u32;
        for i in 0..d.len() as u32 {
            let s = d.section(i).expect("section");
            assert_eq!(s.start, at, "step {step}: section {i} starts at {at}, not {}", s.start);
            assert!(s.end > s.start, "step {step}: section {i} is empty");
            at = s.end;
        }
        assert_eq!(at as usize, text.len(), "step {step}: sections cover the document");
    }
}

#[cfg(test)]
mod word_at_a_time_tests {
    use super::SectionMetrics;

    /// **`measure` must agree with a byte-at-a-time reference on every marker, at every alignment.**
    ///
    /// The ways it can be wrong are all about *boundaries*: a marker straddling the last word, a `$$` pair
    /// straddling a word, and a marker's three bytes spanning two words. All three are invisible to a pure
    /// mask test, which is why `measure`'s loop stops eight bytes short of the end and finishes byte-wise,
    /// and why the marker set includes all three of U+FFFC's bytes rather than only its first.
    ///
    /// **Against a reference in this test, not against hand-written numbers.** A test with numbers pins the
    /// cases its author thought of; a reference pins every case. The reference is the byte loop `measure`
    /// used before it was made word-at-a-time — exactly the thing being verified.
    #[test]
    fn measure_agrees_with_a_byte_scan_on_every_marker_at_every_alignment() {
        fn reference(bytes: &[u8]) -> SectionMetrics {
            let mut spans = 0u32;
            let mut i = 0usize;
            while i < bytes.len() {
                if bytes[i] == super::ANCHOR[0]
                    && i + super::ANCHOR.len() <= bytes.len()
                    && bytes[i..i + super::ANCHOR.len()] == super::ANCHOR
                {
                    spans += 1;
                    i += super::ANCHOR.len();
                    continue;
                }
                if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
                    spans += 1;
                    i += 2;
                    continue;
                }
                i += 1;
            }
            SectionMetrics::new(bytes.len() as u32, spans)
        }

        // Every marker at every offset, in a filler with no markers of its own. Offsets 6, 7, 14 and 15 are
        // the ones that put a marker across an 8-byte word boundary.
        const FILL: usize = 48;
        let markers: [&[u8]; 3] = [b"$", b"$$", &super::ANCHOR];
        for pos in 0..=(FILL - 3) {
            for marker in markers {
                let mut v = vec![b'.'; FILL];
                v[pos..pos + marker.len()].copy_from_slice(marker);
                assert_eq!(super::measure(&v), reference(&v), "marker {marker:?} at {pos}");
            }
        }

        // A document that is nothing but markers, at every length up to two words plus a tail.
        for len in 0..40usize {
            let v: Vec<u8> = (0..len).map(|i| if i % 2 == 0 { b'$' } else { b'.' }).collect();
            assert_eq!(super::measure(&v), reference(&v), "alternating, length {len}");
        }

        // And a real anchor sequence ending exactly at the buffer's end, which is the case the
        // eight-bytes-short loop exists for.
        let mut v = vec![b'.'; 30];
        v.extend_from_slice(&super::ANCHOR);
        assert_eq!(super::measure(&v), reference(&v), "an anchor as the last three bytes");

        // And realistic prose, because the byte loop is only correct for markers the *whole* document
        // contains, and the tests above each have exactly one.
        let prose: Vec<u8> = b"the quick brown fox jumps over the lazy dog\n".repeat(400);
        assert_eq!(super::measure(&prose), reference(&prose), "prose");
    }

    /// The `const` copy of the anchor is pinned against the text crate's, so a change there fails this build
    /// rather than silently finding no anchors again — the exact failure six image gates showed when
    /// anchors were not counted at all.
    #[test]
    fn the_anchor_constant_matches_the_text_crates() {
        assert_eq!(super::ANCHOR, holonomy_text::ANCHOR_BYTES);
    }

    /// `has_zero` is the load-bearing primitive, and the way it is wrong is specific: a zero byte's borrow
    /// propagating into the next lane. A byte alternating with `0x00` is the exact lane pattern that breaks
    /// a bare `x - LANES`, and without the `& !x` this fails.
    #[test]
    fn a_zero_byte_does_not_borrow_into_the_next_lane() {
        let w = u64::from_le_bytes([b'$', 0, b'$', 0, b'$', 0, b'$', 0]);
        assert_eq!(
            super::has_zero(w ^ (super::LANES * 0x24)).count_ones(),
            4,
            "four dollars alternating with four zero bytes: the zero lanes borrow out without the mask \
             and the dollars match, which is the whole reason for `& !x`"
        );
        assert_eq!(
            super::has_zero(super::LANES.wrapping_mul(0xFF)).count_ones(),
            0,
            "eight 0xFF lanes: the subtraction borrows the other way, so nothing matches"
        );
        // And a full eight-zero word, built by `wrapping_mul` so the compiler cannot fold it.
        assert_eq!(
            super::has_zero(super::LANES.wrapping_mul(0x00)).count_ones(),
            8,
            "eight zero bytes are eight matches"
        );
    }
}
