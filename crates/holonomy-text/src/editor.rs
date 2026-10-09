//! The editor: the one type that keeps the rope, the span map and the undo stack consistent.
//!
//! # Why a facade and not three types the caller juggles
//!
//! An edit is three coordinated mutations:
//!
//! ```text
//! bytes     Rope::insert_at / delete_at      -- the document
//! styling   SpanMap::apply_insert / delete   -- the parallel interval map
//! history   UndoStack::push_insert / push_delete
//! ```
//!
//! Doing them in the wrong order, or forgetting one, is silent. Apply the span shift but not the byte
//! change and the spans point past the end of the text; apply the byte change but not the span shift
//! and a run of bold silently becomes plain from the edit point onward. Neither raises an error, both
//! corrupt the document, and both are the kind of bug that survives a test suite that only exercises
//! one structure at a time.
//!
//! So [`Editor`] owns all three and exposes only the coordinated operations. Its invariant is the one
//! worth stating as a single sentence: **after every `Editor` operation, the span map's coverage is
//! exactly `[0, text_len)`, and the two undo stacks' lengths agree.** `check_invariants` asserts it,
//! and the gate asserts it across leaf splits and merges.
//!
//! # The zero-allocation claim, and where it holds
//!
//! Typing at the caret allocates nothing: [`Editor::insert_at`] routes through [`Rope::insert_byte`],
//! which writes into a gap, and through [`UndoStack::push`], which writes into a pre-allocated arena.
//! Deleting *styled* text does allocate, once, to record the styling that has to come back on undo --
//! see [`Editor::delete_at`] for why that is unavoidable and why it is bounded.
//!
//! # Why undo of a styled deletion needs more than bytes
//!
//! [`UndoStack`] records `(offset, kind, bytes)`, which is what the Phase 6 directive specifies and
//! which is enough for plain text. It is not enough for styled text: FR-1.2 is destructive, so the
//! bytes a delete removed cannot be recovered from the rope, and neither can their formatting.
//!
//! So [`Editor`] keeps its own bounded ring of removed-span slices, [`STYLE_UNDO_DEPTH`] entries, pushed
//! only when a delete actually removed styled content. Unstyled deletes -- the overwhelming majority,
//! and every keystroke in a plain document -- push nothing and allocate nothing.

use crate::asset::{Asset, AssetCatalog, AssetError, AssetId};
use crate::payload::{self, PayloadError};
use crate::rope::{Rope, RopeError};
use crate::span::{SpanError, SpanMap, SpanPolicy, TextIntervalSpan};
use crate::tables::{col_widths_for, TableMap, TableMapError};
use crate::undo::{ActionKind, UndoAction, UndoError, UndoStack, UNDO_DEPTH};
use zeroize::Zeroize;

/// Removed-span slices retained for undoing a styled deletion.
pub const STYLE_UNDO_DEPTH: usize = UNDO_DEPTH;

/// Why an editor operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorError {
    /// The rope refused the byte-level edit.
    Rope(RopeError),
    /// The span map refused the coordinate shift.
    Span(SpanError),
    /// The undo stack refused to record the action.
    Undo(UndoError),
    /// Nothing to undo.
    NothingToUndo,
    /// Nothing to redo.
    ///
    /// A distinct variant rather than reusing [`EditorError::NothingToUndo`] because the two have
    /// different causes with opposite fixes: an empty undo stack means the user pressed undo too
    /// often, an empty redo stack means they started a new edit, and a keymap that shows the same
    /// message for both tells the user nothing.
    NothingToRedo,
    /// The offset is not on a UTF-8 character boundary.
    ///
    /// Checked here rather than deferred to the rope, because a mid-character offset would be accepted
    /// by the span map -- which has no idea what a character is -- and produce spans whose boundaries
    /// bisect a multi-byte sequence.
    NotCharBoundary {
        /// Requested offset.
        offset: u32,
    },
    /// An image could not be catalogued, or a payload could not be read or written.
    Asset(AssetError),
    /// A document payload was refused.
    Payload(PayloadError),
}

impl std::fmt::Display for EditorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rope(e) => write!(f, "{e}"),
            Self::Span(e) => write!(f, "{e}"),
            Self::Undo(e) => write!(f, "{e}"),
            Self::NothingToUndo => write!(f, "there is nothing to undo"),
            Self::NothingToRedo => write!(f, "there is nothing to redo"),
            Self::NotCharBoundary { offset } => {
                write!(f, "byte {offset} is inside a UTF-8 character")
            }
            Self::Asset(e) => write!(f, "{e}"),
            Self::Payload(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for EditorError {}

impl From<RopeError> for EditorError {
    fn from(e: RopeError) -> Self {
        Self::Rope(e)
    }
}
impl From<SpanError> for EditorError {
    fn from(e: SpanError) -> Self {
        Self::Span(e)
    }
}
impl From<UndoError> for EditorError {
    fn from(e: UndoError) -> Self {
        Self::Undo(e)
    }
}
impl From<AssetError> for EditorError {
    fn from(e: AssetError) -> Self {
        Self::Asset(e)
    }
}
impl From<PayloadError> for EditorError {
    fn from(e: PayloadError) -> Self {
        Self::Payload(e)
    }
}

/// The result of an edit, for a caller that needs to repaint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditOutcome {
    /// Where the edit happened, in document bytes.
    pub offset: u32,
    /// How many bytes it added (`Insert`) or removed (`Delete`).
    pub len: u32,
    /// Which way it goes.
    pub kind: ActionKind,
    /// The line the edit landed on, if the geometry is attached.
    ///
    /// `None` when no [`holonomy_geometry::LineGeometry`] is attached, which is the case for a bare
    /// editor used only for its text. With one attached, this is what
    /// [`holonomy_geometry::LineGeometry::damage_rect_for`] turns into the repaint region, and it is
    /// what FR-3.4 requires a keystroke to invalidate: one line's box.
    pub line: Option<usize>,
    /// Whether the edit changed the document's line count, so the caller must re-derive the geometry.
    ///
    /// A newline, and a line that wrapped. Both are O(n) in the geometry, which is why the flag exists:
    /// the editor cannot know whether the inserted text wrapped without measuring, so it reports the
    /// one case it *can* know (an explicit newline) and the caller re-measures on any doubt.
    pub lines_changed: bool,
}

/// A rope, its span map, and its undo history, kept consistent.
#[derive(Debug)]
pub struct Editor {
    rope: Rope,
    spans: SpanMap,
    undo: UndoStack,
    /// Removed spans for undoing a deletion, newest last. Bounded to [`STYLE_UNDO_DEPTH`].
    ///
    /// Phase 6 made this a *lossy* side-channel -- pushed only when a delete removed styled content,
    /// so that a plain-text document never allocated here at all -- and paired it with `undo`'s
    /// **unconditional** pop. The two halves disagreed: undoing a plain delete popped an earlier
    /// styled delete's record and restored it over the wrong bytes, and the following undo then found
    /// nothing and silently lost the styling that was there all along. Silent, and unreachable by
    /// testing one delete at a time, which is how it survived Phase 6's gate.
    /// `tests/style_undo.rs` is the reproduction.
    ///
    /// So it is now **total**: one entry per Delete-kind action, with an empty `Vec` meaning "this
    /// delete removed nothing styled". `Vec::new()` does not allocate, so the plain-delete case still
    /// costs zero, and in exchange push and pop pair one-to-one -- the property that was missing.
    ///
    /// Still deliberately *not* in the arena. Being total changes its size bound from
    /// "however many deletes touched styled text" to "however many deletes happened", but 500 empty
    /// `Vec`s is 12 KB of pointer triples, not 500 arena slices, and a second ring in [`UndoStack`]
    /// would have to keep the same eviction order as the first.
    style_undo: Vec<Vec<TextIntervalSpan>>,
    /// Scratch for counting anchors in the bytes before or inside an edited range, sized
    /// [`DELETE_SCRATCH`].
    ///
    /// Exists because of the same measurement as [`Editor::delete_scratch`] and for the same reason: the
    /// obvious way to find an anchor's ordinal is to read the whole prefix with `text()`, which is a
    /// `Vec` the size of the document on **every delete**. `no_alloc.rs` measured 1,000 allocations over
    /// a 1,000-keystroke delete burst and said so immediately.
    ///
    /// So the prefix and the range are counted in chunks through [`Rope::read_at`], which allocates
    /// nothing, into this buffer. What remains is `O(document)` *time* per delete -- which is what every
    /// offset in this crate already costs, and what Phase 11 exists to replace with Fenwick queries. An
    /// allocation claim and a time claim are different claims, and the allocation one is this crate's
    /// gate. It is kept green for documents with a thousand images as well as documents with none,
    /// because the keystroke path cannot afford to depend on what the document happens to contain.
    anchor_scan_scratch: Vec<u8>,
    /// Scratch space for [`delete_at`](Self::delete_at)'s captured bytes, sized [`DELETE_SCRATCH`].
    ///
    /// A field rather than a local array, and the reason is measured: a `let mut buf = [u8; 256]`
    /// inside `delete_at`, whose slice is passed to `UndoStack::push_delete`, **escapes** and is promoted
    /// to the heap by the compiler. The allocation then happens once per `delete_at` call rather than
    /// once per delete, so it is invisible in a rate and visible in a count: a 1,000-keystroke delete
    /// burst reported exactly 1 allocation.
    ///
    /// Hoisting it here makes it one allocation in [`Editor::new`], outside every measured window, and the
    /// per-delete count is then exactly zero with no caveat.
    delete_scratch: Vec<u8>,
    /// Every table in the document, as spans. Phase 9A.
    ///
    /// A field rather than something the caller keeps, because a span is an interval and every edit
    /// moves intervals: an insert inside a table has to extend it and an insert before one has to
    /// slide it. A caller-side table map would have to be updated at every edit site, and the site it
    /// forgets is a table whose cells silently shift under the caret. So the map is updated inside the
    /// five places bytes move -- [`insert_at`](Self::insert_at), [`delete_at`](Self::delete_at),
    /// [`undo`](Self::undo) and [`apply_insert_raw`](Self::apply_insert_raw) -- which is the same
    /// argument `spans` makes for the interval map, and the reason both are fields rather than
    /// collaborators is that neither can be correct without the edit.
    tables: TableMap,
    /// The table map as it was before the most recent undo, so redo can restore it.
    ///
    /// # Why the map cannot be derived from the bytes
    ///
    /// Undoing an insertion deletes the table's separators, and `TableMap::retain_intact` then drops
    /// the span -- correctly, because there is no longer a table there. Redoing re-inserts the same
    /// bytes at the same offset, and `apply_insert` can slide an *existing* span but cannot invent a
    /// vanished one. Worse, it could not invent a correct one even in principle: `rows`, `cols` and
    /// `col_widths` are not recoverable from a flat run of separators, so a 2x3 table and a 1x6 table
    /// are the same six bytes.
    ///
    /// So the pre-undo map is kept, and redo puts it back. It is a single `Option`, not a stack,
    /// because redo is only valid immediately after an undo -- any new edit calls
    /// [`drop_redo`](Self::drop_redo) -- so there is at most one undo's worth of table state to
    /// remember, and a stack would imply a depth the code cannot honour.
    undo_tables: Option<Vec<crate::TableSpan>>,
    /// Actions undone and awaiting redo, newest last. Bounded to [`UNDO_DEPTH`].
    ///
    /// Phase 8. A `Vec<UndoAction>` rather than a second arena, and the reason is that
    /// [`UndoStack::pop_for_undo`] already *hands the bytes out*: it zeroizes its arena and returns an
    /// owning `UndoAction`, because "the caller may re-push it for redo or drop it" is written into the
    /// method's own doc comment. A redo built inside the arena would have to fight that zeroize.
    /// Holding the actions here is what makes redo possible at all, and it costs one allocation per
    /// **undo** -- never per keystroke, which is the path FR-1.2 measures.
    ///
    /// A plain heap `Vec`, not a `SecureBlock`, because an undone action's payload is plaintext that
    /// must itself be scrubbed on drop. It is: [`clear_history`](Self::clear_history) zeroizes every
    /// payload on the way out, the same guarantee the undo arena gives on eviction.
    redo: Vec<UndoAction>,
    /// The document's images, one per anchor, in document order. Phase 9C.
    ///
    /// **An image's *position* is a character, not an offset.** A U+FFFC OBJECT REPLACEMENT CHARACTER
    /// in the document's own bytes ([`crate::ANCHOR`]) is what says where a picture goes, so an edit
    /// moves the anchor for free -- no anchor list to slide.
    ///
    /// **But the *pairing* is positional and has to be maintained.** Entry `i` serves the `i`-th
    /// anchor, because the payload's frozen per-entry shape (`Blake2b | w u16 | h u16 | len | PNG`) has
    /// nowhere to record which anchor an asset belongs to. So deleting an anchor removes an asset, and
    /// undoing an insertion of an image puts it back -- at the same five sites `tables` is updated from,
    /// for the same reason: the site that is forgotten is a picture nobody can account for.
    ///
    /// The first version of this field's doc comment claimed none of that was necessary, on the grounds
    /// that a content address is not an interval. It was wrong, and `AssetCatalog`'s own header now
    /// records the mistake and the symptom -- see [`AssetCatalog`].
    assets: AssetCatalog,
    /// Assets removed by the most recent delete, for undo. Phase 9C.
    ///
    /// The asset counterpart of [`Editor::undo_tables`], and shaped by the same reason: undoing a
    /// deletion re-inserts the bytes, and bytes alone do not bring an image back. Unlike the table
    /// shadow this is *only* the assets the last delete removed, not a whole map -- a `TableSpan` is 28
    /// bytes and shadowing the map is cheap, whereas an asset is a PNG and shadowing the catalog would
    /// mean holding every image in the document a second time.
    ///
    /// A `Vec`, and empty in the common case: a delete that touched no anchor removes no assets, and
    /// `Vec::new()` does not allocate, so the per-keystroke allocation count is unaffected.
    undo_assets: Vec<Asset>,
}

/// Bytes of scratch [`Editor::delete_at`] keeps, covering every single-keystroke delete.
///
/// 256 is past the size at which an array is worth avoiding on its own merits, and it covers any
/// realistic short selection too. A longer delete falls back to the heap; see
/// [`delete_at`](Editor::delete_at).
pub const DELETE_SCRATCH: usize = 256;

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// An empty document.
    pub fn new() -> Self {
        Self::empty(Rope::new(), 0)
    }

    /// A document holding `text`, with no styling and no history.
    pub fn from_text(text: &[u8]) -> Result<Self, EditorError> {
        let rope = Rope::from_text(text)?;
        let len = rope.text_len();
        Ok(Self::empty(rope, len))
    }

    /// An editor over a document of `text_len` bytes whose **bytes are not held**.
    ///
    /// # Why this exists, and the number it moves
    ///
    /// [`from_text`](Self::from_text) needs the whole document as a contiguous `&[u8]`, so opening one
    /// costs **two copies at the peak**: that slice plus every page-locked leaf. Measured on a 2 MiB
    /// document, `VmHWM` peaks at **~5.0 MiB**; this constructor peaks at **~1.1 MiB**, which is the
    /// process baseline and does not carry the document at all. **~4.6x lower, and O(leaves) rather than
    /// O(document)** — see `tests/skeleton_load.rs`, which measures `VmHWM` in a child process rather than
    /// asserting a ratio computed from a struct's field list.
    ///
    /// # Why the rest of `Editor` can be built without reading a byte
    ///
    /// **Because every other field is a function of the document's *length*, not its contents.**
    /// [`SpanMap::plain`] records that every byte is plain-styled, which is true of a document H1 has not
    /// read yet; `TableMap::new` and `AssetCatalog::new` are empty because no table or image has been seen;
    /// the undo stacks are empty because nothing has been edited. So [`empty`](Self::empty) needs only
    /// `text_len`, and this constructor is that call with a skeleton rope in place of a filled one.
    ///
    /// **This is the reassuring half of the answer to "what does absent mean for spans and undo".** Both
    /// are happy with a document they have never read, because both are index structures over lengths.
    /// What they are *not* yet happy about is a **faulted-in edit** — a leaf that arrives mid-session has
    /// to leave the span map and the undo stack indistinguishable from one that was resident throughout.
    /// That is the remaining design question, and this constructor does not address it.
    ///
    /// # What this does not give you
    ///
    /// A skeleton editor is **read-only until leaves are faulted in**: editing an absent leaf
    /// [`refuses`](Rope::leaf_mut). And **no session calls this yet** — `main.rs` still builds
    /// `Editor::new()`, an empty document. So the peak win is proven in a gate and not yet reachable from
    /// the product.
    pub fn from_skeleton(text_len: usize) -> Self {
        Self::empty(Rope::from_skeleton(text_len), text_len)
    }

    /// Bytes of document text currently held, over the resident leaves only.
    ///
    /// **The number the page-lock ceiling is spent on**, and the one a session's residency budget is
    /// enforced against. Delegated rather than reimplemented so there is one definition of "resident".
    pub fn resident_bytes(&self) -> usize {
        self.rope.resident_bytes()
    }

    /// Read `offset..offset+out.len()` into `out`, **faulting absent leaves in from `source`**.
    ///
    /// # Why this is a second method and not a change to [`read_into`](Self::read_into)
    ///
    /// **Because `read_into` is `&self` and faulting is `&mut self`, and that is structural rather than a
    /// preference.** `read_into` is called from `&self` contexts all over the paint path -- `session.rs`,
    /// `doclines.rs`, `counts.rs`, the export sinks -- so it cannot fault. Rather than put a `RefCell` in
    /// the rope and make "is this leaf resident" a runtime question behind a borrow check on the keystroke
    /// path, **the two capabilities are two methods.**
    ///
    /// The split has a real cost, and it is worth naming: **a `&self` reader cannot fault, so every `&self`
    /// reader sees `LeafAbsent` on a sparse rope and has to decide what to do with it.** The paint path
    /// currently counts that as `PaintStats::runs_missing`. That is safe and it is wrong to draw, and it
    /// stays wrong until the paint path takes the `&mut` variant. **This method is what makes that
    /// possible; it does not make it happen.**
    ///
    /// [`LeafSource`] is passed in rather than stored, because the source is the *caller's* — it owns the
    /// store, the budget and the eviction policy. An editor holding a source would have to own a container,
    /// and `holonomy-text` cannot depend on `holonomy-container`.
    pub fn read_into_faulting(
        &mut self,
        source: &mut dyn crate::rope::LeafSource,
        offset: usize,
        out: &mut [u8],
    ) -> Result<usize, EditorError> {
        let len = self.text_len();
        if offset >= len || out.is_empty() {
            return Ok(0);
        }
        let want = out.len().min(len - offset);
        self.rope
            .read_at_faulting(source, offset, want, &mut out[..want])?;
        Ok(want)
    }

    /// Release every leaf overlapping `[start, end)`, keeping each one's length.
    ///
    /// **A pass-through**, and the rope's docs are the design. Exposed because a bounded-memory commit needs
    /// both halves — [`read_into_faulting`](Self::read_into_faulting) to bring a range resident and this to shed
    /// it — and splitting them across two types would mean a caller holding a rope it cannot reach.
    pub fn evict_range(
        &mut self,
        start: usize,
        end: usize,
    ) -> Result<usize, crate::rope::RopeError> {
        self.rope.evict_range(start, end)
    }

    /// Forget every recorded edit, because the source now holds the document as it is.
    ///
    /// **Only correct immediately after every leaf has been written to the source.** An empty record claims the
    /// store's bytes are the current document's bytes; if they are not, every later fault reads the wrong
    /// document and nothing reports it.
    ///
    /// **It is public because the commit is a loop**, and a loop cannot be half-in here: the caller walks the
    /// document writing each chunk, and only at the end is the claim true. [`Rope::commit`] hides this
    /// because it performs the whole write itself. **So the naming is the guard** — `forget_record` says what
    /// it does and `record` says what must already be true, and a caller reaching for it mid-loop has to type
    /// the word.
    pub fn forget_record(&mut self) {
        self.rope.forget_record()
    }

    /// How many leaves currently hold their bytes.
    pub fn resident_count(&self) -> usize {
        self.rope.resident_count()
    }

    /// The shared constructor.
    ///
    /// Every allocation an [`Editor`] ever makes happens here, which is what lets the keystroke path
    /// claim a flat zero. It exists as one function because the field list used to be spelled out in
    /// both constructors, and Phase 6 found a field that had been added to one and not the other.
    fn empty(rope: Rope, text_len: usize) -> Self {
        Self {
            rope,
            spans: SpanMap::plain(text_len as u32),
            tables: TableMap::new(),
            undo_tables: None,
            undo_assets: Vec::new(),
            undo: UndoStack::new(),
            style_undo: Vec::with_capacity(STYLE_UNDO_DEPTH),
            delete_scratch: vec![0u8; DELETE_SCRATCH],
            anchor_scan_scratch: vec![0u8; DELETE_SCRATCH],
            redo: Vec::with_capacity(UNDO_DEPTH),
            assets: AssetCatalog::new(),
        }
    }

    /// The document's bytes, for export and for tests.
    pub fn text(&self) -> Result<Vec<u8>, EditorError> {
        Ok(self.rope.to_vec()?)
    }

    /// The document's images, in document order.
    #[must_use]
    pub fn assets(&self) -> &AssetCatalog {
        &self.assets
    }

    /// The byte offset of every image anchor, in document order.
    ///
    /// A full copy of the document to find three bytes per image, which is exactly the cost Phase 11's
    /// audit complains about elsewhere. It is here rather than being a streaming walk because the
    /// session needs the offsets and the session already holds the text; the `read_into` version
    /// belongs with that phase, not with this one.
    pub fn image_anchors(&self) -> Result<Vec<u32>, EditorError> {
        Ok(crate::asset::scan_anchors(&self.text()?))
    }

    /// Insert `png` as an image at `offset`, and return its [`AssetId`].
    ///
    /// **Two writes, and the order is the contract.** The catalog's *nth* entry serves the *n*-th
    /// anchor in the text, so the two must stay in step. A text write that failed *after* the catalog
    /// write would leave every image below the new one showing the wrong picture -- a corruption that
    /// no error message would ever name. So the catalog is written first and the text second: a
    /// refused PNG leaves nothing at all, and a failed text write leaves a catalog entry that is never
    /// reached. Both are harmless. The reverse order is not.
    ///
    /// The anchor goes in through [`insert_at`](Self::insert_at), so it is undoable, it slides
    /// correctly when text is typed in front of it, and it moves the caret past itself -- exactly as
    /// any other insertion does.
    pub fn insert_image(&mut self, offset: u32, png: &[u8]) -> Result<AssetId, EditorError> {
        // The ordinal this anchor will have is the number of anchors before `offset`, counted on the
        // text as it is *now* -- the anchor does not exist yet. Putting the asset there rather than at
        // the end is the whole of `AssetCatalog`'s ordering contract, and getting it wrong serves this
        // image the last picture in the document.
        let at = self.anchor_ordinal_at(offset as usize);
        let id = self.assets.insert_at(at, png)?;
        self.insert_at(offset, &crate::asset::ANCHOR_BYTES, SpanPolicy::Strict)?;
        Ok(id)
    }

    /// The whole document as a container payload.
    ///
    /// This is the byte string `Wavefunction::create` and `write_content` are handed. The container
    /// neither parses it nor knows its shape; see [`crate::payload`] for why the container format did
    /// not have to change to accommodate it.
    pub fn payload(&self) -> Result<Vec<u8>, EditorError> {
        let text = self.text()?;
        let text =
            String::from_utf8(text).map_err(|_| EditorError::Payload(PayloadError::NotUtf8))?;
        let tables: Vec<crate::TableSpan> = self.tables.spans().to_vec();
        Ok(payload::encode(&text, &self.spans, &tables, &self.assets))
    }

    /// Rebuild a document from a payload produced by [`payload`](Self::payload).
    ///
    /// The history is empty: an opened document has no undo stack. That is Phase 6's "undo is never
    /// persisted" taken to its conclusion rather than a gap.
    ///
    /// No invariant check is called afterwards, and that is not an oversight: `Rope::from_text`
    /// validates the rope, `SpanMap::from_spans` validated the span list on the way out of `decode`
    /// (including that it is sorted, gap-free and ends exactly at the text length), and `decode`
    /// bounds-checked every table against the text. A second validator here would be a second thing
    /// to keep in step with the first, which is the cost `from_spans` exists to avoid.
    pub fn from_payload(bytes: &[u8]) -> Result<Self, EditorError> {
        let decoded = payload::decode(bytes)?;
        let rope = Rope::from_text(decoded.text.as_bytes())?;
        let mut e = Self::empty(rope, decoded.text.len());
        e.spans = decoded.spans;
        e.tables.replace(decoded.tables);
        e.assets = decoded.assets;
        Ok(e)
    }

    /// Document length in bytes.
    #[inline]
    pub fn text_len(&self) -> usize {
        self.rope.text_len()
    }

    /// Every table in the document, as spans.
    pub fn tables(&self) -> &TableMap {
        &self.tables
    }

    /// The table containing `offset`, if any.
    pub fn table_at(&self, offset: u32) -> Option<crate::TableSpan> {
        self.tables.at(offset)
    }

    /// Insert an empty `rows` by `cols` table at the caret and return its span.
    ///
    /// # The bytes, and why there are newlines around them
    ///
    /// The table's own bytes are `rows * cols - 1` separators and nothing else -- every cell starts
    /// empty. Around them, this inserts a newline **before** when the caret is not already at the
    /// start of a line, and one **after** always.
    ///
    /// Both are load-bearing rather than cosmetic. Without the leading one, a table inserted into the
    /// middle of a paragraph would begin on that paragraph's line, and the box's top border would be
    /// drawn across the second half of a sentence. Without the trailing one, whatever text followed
    /// the caret would end up inside the table's last cell, because a table's span ends at a byte
    /// offset and the only thing that says "the table stopped here" is a newline the layout knows
    /// about. Insert one means the next character typed is *outside* the table, which is what a
    /// person expects after `Ctrl+T`.
    ///
    /// The span covers the separators only, never the newlines, so a caret on either newline is
    /// outside the table -- see [`TableMap::at`], whose end is exclusive for the same reason.
    ///
    /// The caret is left where the insertion started, which is the first byte of cell `(0, 0)`: a
    /// person who has just asked for a table wants to type into its first cell, and the session's
    /// `Ctrl+T` handler moves there explicitly rather than relying on this.
    pub fn insert_table(
        &mut self,
        rows: u16,
        cols: u16,
        measure: u32,
    ) -> Result<crate::TableSpan, TableMapError> {
        let widths = col_widths_for(measure, cols).ok_or(crate::TableError::TooWide {
            content: 0,
            needed: u32::MAX,
            measure,
        })?;
        let shape = crate::TableSpan::with_widths(rows, cols, widths, 0, 0)?;

        let at = self.caret();
        let text = self.text()?;
        // A leading newline only when there is a line to end. At offset 0 there is nothing before,
        // and a leading newline would leave the table one line down with a blank line above it.
        let leading = if at > 0 && text.get(at as usize - 1) != Some(&b'\n') {
            1u32
        } else {
            0
        };

        let mut bytes = Vec::with_capacity(shape.separator_count() as usize + 2);
        if leading > 0 {
            bytes.push(b'\n');
        }
        let table_start = at + leading;
        bytes.extend(std::iter::repeat_n(
            crate::table::CELL_SEPARATOR,
            shape.separator_count() as usize,
        ));
        let table_end = table_start + shape.separator_count();
        bytes.push(b'\n');

        // The insert happens *first*, so that `insert_at`'s own `apply_insert` moves the tables that
        // are already here, and this table's span is registered against bytes that exist.
        self.insert_at(at, &bytes, SpanPolicy::GrowIntoInsert)?;

        let span = crate::TableSpan {
            start_byte: table_start,
            end_byte: table_end,
            ..shape
        };
        self.tables.insert(span)?;
        Ok(span)
    }

    /// Append a row to `span`'s table, and return the new row's index.
    ///
    /// `cols - 1` new separators, at the end of the table, because the boundary between the old last
    /// cell and the new first cell already has one. Called by Tab at the bottom-right cell, and only
    /// there: it is an **edit**, with bytes and an undo action, which is why it is not inside the
    /// navigation rule.
    pub fn append_table_row(&mut self, span: crate::TableSpan) -> Result<u16, TableMapError> {
        let add = crate::tables::appended_row_bytes(span.cols);
        if add.is_empty() {
            return Err(crate::TableError::ZeroExtent.into());
        }
        let rows = span.rows;
        self.insert_at(span.end_byte, &add, SpanPolicy::GrowIntoInsert)?;
        let grown = crate::TableSpan {
            rows: rows + 1,
            end_byte: span.end_byte + add.len() as u32,
            ..span
        };
        // Replace rather than insert: `apply_insert` above already extended this span, and inserting a
        // second copy of it would make the map think there are two tables where there is one.
        if let Some(at) = self
            .tables
            .spans()
            .iter()
            .position(|s| s.start_byte == span.start_byte)
        {
            let mut spans = self.tables.take();
            spans[at] = grown;
            self.tables.replace(spans);
        } else {
            self.tables.insert(grown)?;
        }
        Ok(rows)
    }

    /// The span map.
    #[inline]
    pub fn spans(&self) -> &SpanMap {
        &self.spans
    }

    /// The undo stack, for depth and diagnostics.
    ///
    /// Named `undo_stack` rather than `undo` so it does not collide with
    /// [`undo`](Self::undo), the operation. An earlier version had both called `undo` and the compiler
    /// rejected the duplicate.
    #[inline]
    pub fn undo_stack(&self) -> &UndoStack {
        &self.undo
    }

    /// Number of leaves, so a test can assert a split happened.
    #[inline]
    pub fn leaf_count(&self) -> usize {
        self.rope.leaf_count()
    }

    /// Insert `bytes` at `offset`, recording the action for undo.
    ///
    /// The order is bytes, then spans, then history, and it matters:
    ///
    /// 1. The rope, because a refused byte edit must leave the span map untouched. Reversing the order
    ///    means a failed span shift after a successful byte edit, which no caller can detect.
    /// 2. The spans, which must see the *new* length to stay gap-free.
    /// 3. The history, last, because it should only record an edit that actually happened.
    ///
    /// `policy` decides what styling the inserted bytes take; see [`SpanPolicy`].
    pub fn insert_at(
        &mut self,
        offset: u32,
        bytes: &[u8],
        policy: SpanPolicy,
    ) -> Result<EditOutcome, EditorError> {
        let offset = offset as usize;
        if !self.is_char_boundary(offset) {
            return Err(EditorError::NotCharBoundary {
                offset: offset as u32,
            });
        }
        self.rope.insert_at(offset, bytes)?;
        self.spans
            .apply_insert_with(offset as u32, bytes.len() as u32, policy)?;
        self.tables.apply_insert(offset as u32, bytes.len() as u32);
        self.undo.push_insert(offset as u32, bytes)?;
        // Last, because a refused edit must leave the history exactly as it found it -- including the
        // redo branch, which is history.
        self.drop_redo();

        Ok(EditOutcome {
            offset: offset as u32,
            len: bytes.len() as u32,
            kind: ActionKind::Insert,
            line: None,
            lines_changed: bytes.contains(&b'\n'),
        })
    }

    /// Insert one byte at the caret.
    pub fn insert_char(
        &mut self,
        byte: u8,
        policy: SpanPolicy,
    ) -> Result<EditOutcome, EditorError> {
        let offset = self.rope.cursor() as u32;
        self.insert_at(offset, &[byte], policy)
    }

    /// Delete `len` bytes at `offset`, recording the action for undo.
    ///
    /// # Why this can allocate and the others cannot
    ///
    /// Undoing the deletion needs the removed bytes (they are scrubbed from the rope by FR-1.2) *and*
    /// their styling. The bytes go into [`UndoStack`]'s pre-allocated arena. The styling has nowhere to
    /// go, so it goes in [`Editor::style_undo`], which is a `Vec` and therefore allocates -- but only
    /// when the deleted region was actually styled.
    ///
    /// The alternative is to make [`UndoStack`]'s arena also hold span records, which would make typing
    /// allocation-free and deleting allocation-free at the cost of a second ring format inside the undo
    /// stack. That is the better design and it is not what is here; the trade is deliberate and this is
    /// where it is recorded.
    pub fn delete_at(&mut self, offset: u32, len: u32) -> Result<EditOutcome, EditorError> {
        let offset_u = offset as usize;
        let end = offset_u + len as usize;
        if end > self.text_len() {
            return Err(EditorError::Rope(RopeError::OutOfBounds {
                offset: end,
                text_len: self.text_len(),
            }));
        }
        if !self.is_char_boundary(offset_u) || !self.is_char_boundary(end) {
            return Err(EditorError::NotCharBoundary { offset });
        }

        // Capture the bytes before the rope scrubs them, into the editor's pre-allocated scratch buffer.
        //
        // Backspace is a keystroke and FR-1.2 measures keystrokes, so this must not allocate. Three
        // attempts, in order of what each measured:
        //
        // * `vec![0u8; len]` per delete: one allocation per keystroke. A 1,000-keystroke delete burst
        //   reported 2,000 allocations -- one here, one in the styling capture below.
        // * A local `[u8; 256]`: **one** allocation, because the slice escapes and the compiler promotes
        //   the array to the heap. 1,000 deletes reported 1 allocation, which reads as a per-call cost
        //   rather than the per-delete one it looked like.
        // * [`Editor::delete_scratch`]: one allocation in `new()`, outside every measured window, and the
        //   per-delete count is then exactly zero with no caveat.
        //
        // A delete longer than [`DELETE_SCRATCH`] -- a multi-kilobyte selection -- falls back to the heap.
        // That is the honest cost for that operation and it is documented rather than hidden.
        let removed_spans = self.spans.apply_delete(offset, len)?;
        self.tables.apply_delete(offset, len);
        // Read before the bytes go, because both halves of this are byte positions and the delete
        // renumbers them: `first` is how many anchors precede the range, and `count` is how many are
        // inside it. Neither needs the document -- the first is a chunked prefix scan into
        // `anchor_scan_scratch`, the second a count over the bytes already in `delete_scratch`.
        let first = self.anchor_ordinal_at(offset_u);
        let anchor_count = self.anchors_in_range(offset_u, len);
        self.undo_assets = self.assets.remove_n(first, anchor_count);
        // A table whose separators have all been deleted has no cells, and `cell_count()` subtracts
        // one from `rows * cols`, which underflows `u32`. Dropping it here is what stops a later
        // keystroke walking into a zero-cell table; see `TableMap::retain_intact`.
        self.tables.retain_intact();

        // Read the bytes out of the scratch buffer and hand them to the undo stack, in one scope.
        //
        // The scope is what makes the borrow checker satisfiable: `removed` borrows
        // `delete_scratch`, so it must be dead before `delete_range_in_rope` takes `&mut self`. Reading
        // and recording together is also the correct order -- the bytes are captured before the rope
        // scrubs them, and an action is recorded only once the edit it describes is under way.
        let crossed_line = {
            // Destructure so `rope`, `undo` and `delete_scratch` are disjoint fields. Reaching them as
            // `self.rope` and `self.delete_scratch` is an immutable and a mutable borrow of one `self` in
            // a single call, which the compiler rejects.
            let Self {
                rope,
                undo,
                delete_scratch,
                ..
            } = self;
            let mut heap_buf: Vec<u8> = Vec::new();
            let removed: &[u8] = if len as usize <= DELETE_SCRATCH {
                rope.read_at(offset_u, len as usize, &mut delete_scratch[..len as usize])?;
                &delete_scratch[..len as usize]
            } else {
                heap_buf.resize(len as usize, 0);
                rope.read_at(offset_u, len as usize, &mut heap_buf)?;
                &heap_buf
            };
            undo.push_delete(offset, removed)?;
            // The one other fact the caller needs from these bytes, captured while they are still live.
            removed.contains(&b'\n')
        };

        self.rope.set_cursor(offset_u + len as usize)?;
        self.delete_range_in_rope(offset_u, len as usize)?;

        // Record the styling the delete removed, **always**, so that `undo`'s pop pairs one-to-one
        // with this push.
        //
        // Phase 6 gated this on `!styled.is_empty()`, which was a real optimisation (a plain-text
        // document allocated nothing here at all) and a real bug: `undo` popped unconditionally, so
        // a plain delete's undo stole the previous styled delete's record. `Vec::new()` does not
        // allocate, so making the entry total costs a `Vec` header and nothing else.
        //
        // The `is_empty` branch is still worth spelling out rather than left implicit: an
        // unconditional `collect()` over a `filter_map` takes its capacity from the iterator's upper
        // size hint *even when it yields nothing*, which is the second allocation per delete that
        // `no_alloc.rs` measures. The filter, then the `is_empty` check, is what makes that zero.
        let styled: Vec<TextIntervalSpan> = if removed_spans
            .iter()
            .all(|s| s.style_flags == 0 && s.color_rgb == 0)
        {
            Vec::new()
        } else {
            removed_spans
                .iter()
                .copied()
                .filter(|s| s.style_flags != 0 || s.color_rgb != 0)
                .collect()
        };
        if self.style_undo.len() == STYLE_UNDO_DEPTH {
            self.style_undo.remove(0);
        }
        self.style_undo.push(styled);
        // A fresh delete invalidates the redo branch too, for the reason in `drop_redo`.
        self.drop_redo();

        Ok(EditOutcome {
            offset,
            len,
            kind: ActionKind::Delete,
            line: None,
            lines_changed: crossed_line,
        })
    }

    /// Delete the byte before the caret.
    pub fn backspace(&mut self) -> Result<EditOutcome, EditorError> {
        let cursor = self.rope.cursor();
        if cursor == 0 {
            return Err(EditorError::Rope(RopeError::OutOfBounds {
                offset: 0,
                text_len: 0,
            }));
        }
        // `delete_at` reads the bytes from the rope, so the caret must be *after* the target.
        self.delete_at((cursor - 1) as u32, 1)
    }

    /// Delete `len` bytes by caret-relative delete, so the rope's merge path runs.
    ///
    /// Separate from [`delete_at`](Self::delete_at) because deleting at an arbitrary offset and
    /// deleting before the caret take different paths through the rope: the latter is the keystroke
    /// path, and it is the one that merges leaves.
    ///
    /// # Why the cursor is set once, at the far end
    ///
    /// `Rope::delete_byte` deletes the byte *before* the cursor and leaves the cursor on it, so one
    /// `set_cursor(offset + 1)` per byte is the obvious way to walk forward. **It cannot delete a
    /// multi-byte character.** After a 3-byte U+FFFC lands at `offset`, the second iteration sets the
    /// cursor to `offset + 1`, which is the middle of that character, and `set_cursor` refuses:
    ///
    /// ```text
    /// thread 'undo' panicked: undo: Rope(NotCharBoundary { offset: 2, text_len: 4 })
    /// ```
    ///
    /// Every byte offset this crate's own tests touched was ASCII, so nothing noticed. Setting the
    /// cursor once to `offset + len` and then deleting walks *backwards* through the run, and every
    /// intermediate position is set by `delete_byte` itself rather than by us -- so there is no moment
    /// at which a cursor can land mid-character. `offset + len` is a boundary because the run was
    /// inserted as whole characters at a boundary, which is the only thing that puts it in a payload.
    fn delete_range_in_rope(&mut self, offset: usize, len: usize) -> Result<(), EditorError> {
        self.rope.set_cursor(offset + len)?;
        for _ in 0..len {
            self.rope.delete_byte()?;
        }
        Ok(())
    }

    /// Whether `offset` is on a UTF-8 character boundary.
    fn is_char_boundary(&self, offset: usize) -> bool {
        if offset == 0 || offset == self.text_len() {
            return true;
        }
        if offset > self.text_len() {
            return false;
        }
        // The byte **at** `offset`, not the one before it. A boundary is the *absence* of a continuation
        // byte at that index: in `héllo`, offset 2 holds `0xA9`, continuing the character that began at
        // 1, so 2 is not a boundary.
        //
        // Reading `offset - 1` is the same off-by-one the leaf's `is_char_boundary` had, reintroduced
        // here. It calls offset 2 a boundary -- byte 1 is `0xC3`, not a continuation -- and lets a
        // split land inside `é`.
        let mut one = [0u8; 1];
        self.rope.read_at(offset, 1, &mut one).is_ok() && (one[0] & 0xC0) != 0x80
    }

    /// Style `[start, end)`.
    pub fn style_range(
        &mut self,
        start: u32,
        end: u32,
        style_flags: u16,
        color_rgb: u32,
    ) -> Result<(), EditorError> {
        Ok(self.spans.style_range(start, end, style_flags, color_rgb)?)
    }

    /// The style in effect at `offset`.
    #[inline]
    pub fn style_at(&self, offset: u32) -> TextIntervalSpan {
        self.spans.style_at(offset)
    }

    /// Undo the most recent action.
    ///
    /// Both directions restore bytes *and* styling, which is the whole point of routing edits through
    /// this type.
    pub fn undo(&mut self) -> Result<EditOutcome, EditorError> {
        // Stashed before the undo, not after: after it, the spans that this undo is about to delete
        // are already gone, and stashing then would preserve the loss rather than undo it.
        self.undo_tables = Some(self.tables.spans().to_vec());
        let stash_assets = core::mem::take(&mut self.undo_assets);
        let action = self
            .undo
            .pop_for_undo()
            .map_err(|_| EditorError::NothingToUndo)?;
        let outcome = match action.kind {
            ActionKind::Insert => {
                // The action inserted these bytes, so undoing removes them.
                self.spans
                    .apply_delete(action.offset, action.bytes.len() as u32)?;
                self.tables
                    .apply_delete(action.offset, action.bytes.len() as u32);
                self.tables.retain_intact();
                // The asset that was inserted with those bytes goes too, or the anchor-to-asset pairing
                // shifts by one and every image below this point shows the wrong picture. **Kept** rather
                // than dropped, because `redo` replays these same bytes and has to put the asset back
                // -- and the stash is the only record of it that exists.
                let first = self.anchor_ordinal_at(action.offset as usize);
                self.undo_assets = self
                    .assets
                    .remove_n(first, AssetCatalog::count_in(&action.bytes));
                self.rope
                    .set_cursor(action.offset as usize + action.bytes.len())?;
                self.delete_range_in_rope(action.offset as usize, action.bytes.len())?;
                EditOutcome {
                    offset: action.offset,
                    len: action.bytes.len() as u32,
                    kind: ActionKind::Delete,
                    line: None,
                    lines_changed: action.bytes.contains(&b'\n'),
                }
            }
            ActionKind::Delete => {
                // The action deleted these bytes, so undoing re-inserts them, and the styling comes
                // back from the style record. The table map needs no restore on this arm: the
                // `apply_insert` below extends whatever span these bytes landed in, and a table that
                // survived the delete is still a span. Only redo's `Insert` arm needs the stash,
                // because undoing an insertion deletes a table outright.
                let removed = self.style_undo.pop();
                let start = action.offset;
                let end = action.offset + action.bytes.len() as u32;
                self.rope.set_cursor(action.offset as usize)?;
                self.rope.insert_at(action.offset as usize, &action.bytes)?;
                // `GrowIntoInsert` so the restored bytes take the preceding run's style as a starting
                // point, then the recorded spans overwrite it exactly.
                self.spans.apply_insert_with(
                    start,
                    action.bytes.len() as u32,
                    SpanPolicy::GrowIntoInsert,
                )?;
                self.tables.apply_insert(start, action.bytes.len() as u32);
                // The bytes are back, so the assets for the anchors among them go back too. `restore_at`
                // replaces rather than appends, so the assets of *later* anchors stay where they are.
                if !stash_assets.is_empty() {
                    let (first, after) =
                        AssetCatalog::anchors_in(&self.text()?, start, action.bytes.len() as u32);
                    let want = after.saturating_sub(first);
                    self.undo_assets = self.assets.restore_at(first, stash_assets, want);
                }
                match removed {
                    Some(spans) if !spans.is_empty() => {
                        for s in spans {
                            self.spans.style_range(
                                s.start_byte,
                                s.end_byte,
                                s.style_flags,
                                s.color_rgb,
                            )?;
                        }
                    }
                    _ => {
                        // An empty record -- and *only* an empty record, now that `style_undo` is total
                        // -- means these bytes carried no styling of their own. So they must come back
                        // plain, not with the `GrowIntoInsert` guess above.
                        //
                        // Before Phase 8 this arm could not tell "was plain" from "the record went
                        // missing", so it took the guess, and undoing a plain delete that sat next to a
                        // bold run restored the text bold. Restoring a delete should restore what was
                        // deleted; `GrowIntoInsert` is the right default for the *caret* and the wrong
                        // one for *history*, and this is where the two come apart.
                        self.spans.style_range(start, end, 0, 0)?;
                    }
                }
                EditOutcome {
                    offset: action.offset,
                    len: action.bytes.len() as u32,
                    kind: ActionKind::Insert,
                    line: None,
                    lines_changed: action.bytes.contains(&b'\n'),
                }
            }
        };
        // Park it for redo, *after* the inverse has been applied -- the action describes the original
        // edit, so its bytes are still needed to replay it forward.
        //
        // Bounded exactly as `style_undo` is. The eviction matters more here than there: dropping an
        // evicted redo action discards its payload, so it is zeroized rather than freed with plaintext
        // still in it. An evicted redo is a redo that can no longer happen, which is the same guarantee
        // `UndoStack`'s own arena gives on overflow.
        if self.redo.len() == UNDO_DEPTH {
            let mut evicted = self.redo.remove(0);
            evicted.bytes.zeroize();
        }
        self.redo.push(action);
        Ok(outcome)
    }

    /// Number of actions that can be undone.
    #[inline]
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// Whether anything can be undone.
    #[inline]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Discard the history, zeroizing both rings.
    pub fn clear_history(&mut self) {
        self.undo.clear();
        self.style_undo.clear();
        // The redo payloads are plaintext that outlived their undo, so they get scrubbed rather than
        // dropped. `UndoAction` holds a `Vec<u8>`, and this is the only path that discards one.
        for action in &mut self.redo {
            action.bytes.zeroize();
        }
        self.redo.clear();
    }

    // ---------------------------------------------------------------- caret, Phase 8

    /// Copy a range of the document into `out`, returning how many bytes were copied.
    ///
    /// Phase 8, for the exporters. [`text`](Self::text) is the honest whole-document API and allocates
    /// a `Vec` the size of the document; a writer that streams wants a bounded buffer instead, and
    /// inside the jail memory is the scarce resource.
    ///
    /// Short reads at the end are normal and reported as a short count rather than an error, so a
    /// caller loops on `offset += n` until `n == 0`. A range that starts past the end copies nothing
    /// and returns 0.
    ///
    /// `out` is the caller's, so this allocates nothing. The caret is not moved.
    pub fn read_into(&self, offset: usize, out: &mut [u8]) -> Result<usize, EditorError> {
        let len = self.text_len();
        if offset >= len || out.is_empty() {
            return Ok(0);
        }
        let want = out.len().min(len - offset);
        self.rope.read_at(offset, want, &mut out[..want])?;
        Ok(want)
    }

    /// Where the caret is, in document bytes. Always on a UTF-8 character boundary.
    #[inline]
    pub fn caret(&self) -> u32 {
        self.rope.cursor() as u32
    }

    /// Move the caret to `offset`, snapped into the document and onto a character boundary.
    ///
    /// Returns where it landed, which is not necessarily `offset`: an offset past the end clamps to
    /// the end, and one that falls inside a multi-byte character snaps *backwards* to that
    /// How many anchors are inside the byte range `offset..offset + len`, without allocating.
    ///
    /// Reads through [`Editor::anchor_scan_scratch`], the same buffer [`anchor_ordinal_at`](Self)
    /// uses -- so a delete does one pass over the prefix and one over the range, both into memory
    /// allocated once in `Editor::empty`. That is the whole point of `no_alloc.rs` staying green: a
    /// document with a thousand images must delete a character as cheaply as one with none, because
    /// the keystroke path cannot afford to depend on how much the document happens to contain.
    ///
    /// A range longer than the scratch is not a keystroke -- it is a multi-kilobyte selection -- so it
    /// takes one heap buffer, which is the honest cost and is documented rather than pretended away.
    fn anchors_in_range(&mut self, offset: usize, len: u32) -> usize {
        let len = len as usize;
        if len == 0 {
            return 0;
        }
        if len > self.anchor_scan_scratch.len() {
            let mut heap = vec![0u8; len];
            let Ok(()) = self.rope.read_at(offset, len, &mut heap) else {
                return 0;
            };
            return AssetCatalog::count_in(&heap);
        }
        let Ok(()) = self
            .rope
            .read_at(offset, len, &mut self.anchor_scan_scratch[..len])
        else {
            return 0;
        };
        AssetCatalog::count_in(&self.anchor_scan_scratch[..len])
    }

    /// How many anchors there are before byte `offset`, without copying the prefix.
    ///
    /// The ordinal an image inserted at `offset` will take, and the ordinal a delete starting at
    /// `offset` begins removing from. Reads the prefix in [`DELETE_SCRATCH`]-sized chunks through
    /// [`Rope::read_at`] -- see [`Editor::anchor_scan_scratch`] for why this is not `text()`.
    ///
    /// `&mut self` rather than `&self` because it writes the scratch, and a scratch that had to be
    /// taken with `mem::take` to satisfy the borrow checker would be a heap allocation -- the very thing
    /// the scratch exists to avoid.
    fn anchor_ordinal_at(&mut self, offset: usize) -> usize {
        let mut n = 0usize;
        let mut at = 0usize;
        let end = offset.min(self.text_len());
        let chunk = self.anchor_scan_scratch.len();
        while at < end {
            let take = chunk.min(end - at);
            if self
                .rope
                .read_at(at, take, &mut self.anchor_scan_scratch[..take])
                .is_err()
            {
                // A failed read means the rope disagrees with `text_len`, which is an invariant
                // violation rather than an edit case. Counting what was read and stopping is the
                // conservative direction: it can under-count an ordinal, and an under-count surfaces as a
                // refused insert or an over-eager removal -- both recoverable -- where an over-count
                // would silently show the wrong picture.
                return n;
            }
            n += AssetCatalog::count_in(&self.anchor_scan_scratch[..take]);
            at += take;
        }
        n
    }

    /// character's start.
    ///
    /// Snapping backwards rather than forwards is the choice worth stating. Forward would put the
    /// caret after a character the caller meant to address; backwards puts it before, which is the
    /// only reading where the character at `offset` is still reachable with a Right. Both are
    /// defensible and only one is reversible.
    pub fn caret_to(&mut self, offset: usize) -> Result<u32, EditorError> {
        let target = offset.min(self.text_len());
        let mut snapped = target;
        while snapped > 0 && !self.is_char_boundary(snapped) {
            snapped -= 1;
        }
        self.rope.set_cursor(snapped)?;
        Ok(snapped as u32)
    }

    /// Move the caret left one character.
    ///
    /// Errors at offset 0 rather than clamping, because "left at the start" is a command the keymap
    /// should swallow rather than a document operation that failed.
    pub fn caret_left(&mut self) -> Result<u32, EditorError> {
        let cursor = self.rope.cursor();
        if cursor == 0 {
            return Err(EditorError::NothingToUndo);
        }
        let mut target = cursor - 1;
        while target > 0 && !self.is_char_boundary(target) {
            target -= 1;
        }
        self.rope.set_cursor(target)?;
        Ok(target as u32)
    }

    /// Move the caret right one character.
    pub fn caret_right(&mut self) -> Result<u32, EditorError> {
        let cursor = self.rope.cursor();
        let end = self.text_len();
        if cursor >= end {
            return Err(EditorError::NothingToUndo);
        }
        let mut target = cursor + 1;
        while target < end && !self.is_char_boundary(target) {
            target += 1;
        }
        self.rope.set_cursor(target)?;
        Ok(target as u32)
    }

    /// Delete the character *after* the caret. The Delete key, as opposed to
    /// [`backspace`](Self::backspace).
    ///
    /// Deletes a whole codepoint, not a byte. Deleting a byte of a multi-byte character would leave
    /// the document invalid UTF-8, and the rope's own `is_char_boundary` would then refuse every
    /// subsequent edit at that point -- one Delete key turning into a document that cannot be edited
    /// at all.
    pub fn delete_forward(&mut self) -> Result<EditOutcome, EditorError> {
        let cursor = self.rope.cursor() as u32;
        if !self.is_char_boundary(cursor as usize) {
            return Err(EditorError::NotCharBoundary { offset: cursor });
        }
        let mut len = 1usize;
        while cursor as usize + len < self.text_len()
            && !self.is_char_boundary(cursor as usize + len)
        {
            len += 1;
        }
        if cursor as usize + len > self.text_len() {
            return Err(EditorError::Rope(RopeError::OutOfBounds {
                offset: cursor as usize + len,
                text_len: self.text_len(),
            }));
        }
        self.delete_at(cursor, len as u32)
    }

    // ---------------------------------------------------------------- redo, Phase 8

    /// Number of actions that can be redone.
    #[inline]
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    #[inline]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Re-apply the most recently undone action.
    ///
    /// # Why this cannot reuse `insert_at` / `delete_at`
    ///
    /// Those push to the undo stack, because for a fresh edit that is what they must do. A redo is
    /// not a fresh edit: it is the *same* edit being replayed, so recording it again would make the
    /// two histories disagree -- undo would then be able to undo a redo, and the depths reported by
    /// [`undo_depth`](Self::undo_depth) and [`redo_depth`](Self::redo_depth) would not sum to the
    /// number of edits made. So a redo applies the bytes and the span shift directly, and puts the
    /// action back on the undo stack exactly once, itself.
    ///
    /// The `Delete` arm additionally captures the styling of the region it removes, mirroring what
    /// [`delete_at`](Self::delete_at) does for a fresh delete, so that an undo of this redo can put
    /// the styling back.
    pub fn redo(&mut self) -> Result<EditOutcome, EditorError> {
        // Restored after the bytes go back in, so the spans are the ones the re-inserted bytes belong
        // to rather than a guess about where they should be.
        let restore = self.undo_tables.take();
        let restore_assets = core::mem::take(&mut self.undo_assets);
        let mut action = self.redo.pop().ok_or(EditorError::NothingToRedo)?;
        let offset = action.offset as usize;
        let len = action.bytes.len();
        let lines_changed = action.bytes.contains(&b'\n');

        let outcome = match action.kind {
            ActionKind::Insert => {
                self.apply_insert_raw(offset, &action.bytes, SpanPolicy::Strict)?;
                // The bytes of a table are back; the shape that goes with them is what `undo` stashed.
                if let Some(spans) = restore {
                    self.tables.replace(spans);
                }
                if !restore_assets.is_empty() {
                    let (first, after) = AssetCatalog::anchors_in(
                        &self.text()?,
                        action.offset,
                        action.bytes.len() as u32,
                    );
                    let want = after.saturating_sub(first);
                    self.undo_assets = self.assets.restore_at(first, restore_assets, want);
                }
                EditOutcome {
                    offset: action.offset,
                    len: len as u32,
                    kind: ActionKind::Insert,
                    line: None,
                    lines_changed,
                }
            }
            ActionKind::Delete => {
                // Capture the styling this redo is about to destroy, so an undo of the redo can put
                // it back. Pushed to the same `style_undo` as a fresh delete, for the same reason
                // that vector is total: both are Delete actions on one undo stack, and `undo` cannot
                // tell them apart.
                let removed = self.capture_spans(action.offset, len as u32)?;
                // No table restore here. Redoing a delete removes bytes again, and `apply_delete`
                // below shortens the same spans the original delete did, which is the same answer.
                // The stash is for the `Insert` arm alone.
                let _ = &restore;
                if self.style_undo.len() == STYLE_UNDO_DEPTH {
                    self.style_undo.remove(0);
                }
                self.style_undo.push(removed);
                self.delete_range_in_rope(offset, len)?;
                EditOutcome {
                    offset: action.offset,
                    len: len as u32,
                    kind: ActionKind::Delete,
                    line: None,
                    lines_changed,
                }
            }
        };

        // Back on the undo stack, so undo and redo can alternate indefinitely. `push` may evict the
        // oldest entries to make room, which is the same trade a fresh edit makes and the reason redo
        // is not guaranteed forever: it is bounded exactly as deeply as undo is.
        self.undo
            .push(action.offset, action.kind, &action.bytes)
            .map_err(EditorError::Undo)?;
        // The arena owns a copy now, so scrub ours rather than dropping plaintext to the heap. Order
        // matters: this is after the push for exactly that reason.
        action.bytes.zeroize();
        Ok(outcome)
    }

    /// Apply a byte insertion to the rope and the span map, with no history record.
    ///
    /// The shared half of [`insert_at`](Self::insert_at) and [`redo`](Self::redo): the two differ
    /// only in whether the action is recorded.
    fn apply_insert_raw(
        &mut self,
        offset: usize,
        bytes: &[u8],
        policy: SpanPolicy,
    ) -> Result<(), EditorError> {
        self.rope.insert_at(offset, bytes)?;
        self.spans
            .apply_insert_with(offset as u32, bytes.len() as u32, policy)?;
        self.tables.apply_insert(offset as u32, bytes.len() as u32);
        Ok(())
    }

    /// Discard the redo history, scrubbing the payloads on the way out.
    ///
    /// Called by every *fresh* edit. The branch that keeps the undone bytes replayable would otherwise
    /// be one that every word processor takes: undo, type something, redo would splice the replayed
    /// text at a caret that has since moved, which is not an edit anyone asked for.
    ///
    /// Not called by [`redo`](Self::redo), which is the whole point -- redo pushes *onto* the undo
    /// stack and must leave this vector alone. The two are kept distinct precisely so `redo` can
    /// bypass it; a single `record()` helper that both used would have been the bug.
    fn drop_redo(&mut self) {
        for action in &mut self.redo {
            action.bytes.zeroize();
        }
        self.redo.clear();
    }

    /// The span records covering `[offset, offset+len)`, for restoring them on undo.
    ///
    /// Returns an empty vector when the region is plain, which is the case for almost every
    /// keystroke in an unstyled document.
    fn capture_spans(&self, offset: u32, len: u32) -> Result<Vec<TextIntervalSpan>, EditorError> {
        if len == 0 {
            return Ok(Vec::new());
        }
        Ok(self
            .spans
            .spans()
            .iter()
            .filter(|s| s.start_byte < offset + len && s.end_byte > offset)
            .copied()
            .collect())
    }

    /// Assert the editor's cross-structure invariants. Test-only.
    #[cfg(test)]
    pub(crate) fn check_invariants(&self) {
        self.rope.check_invariants();
        self.spans.check_invariants();
        assert_eq!(
            self.spans.text_len() as usize,
            self.rope.text_len(),
            "the span map covers {} bytes but the rope holds {}",
            self.spans.text_len(),
            self.rope.text_len()
        );
        // Span lengths must tile the document exactly.
        let sum: u32 = self.spans.spans().iter().map(TextIntervalSpan::len).sum();
        assert_eq!(
            sum,
            self.spans.text_len(),
            "the spans do not tile the document"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::{STYLE_BOLD, STYLE_CODE};

    const RED: u32 = 0x00FF_0000;

    fn editor(text: &str) -> Editor {
        Editor::from_text(text.as_bytes()).expect("load")
    }

    #[test]
    fn an_empty_editor_is_consistent() {
        let e = Editor::new();
        e.check_invariants();
        assert_eq!(e.text_len(), 0);
        assert_eq!(e.leaf_count(), 1);
        assert!(!e.can_undo());
        assert!(e.undo_stack().is_empty());
    }

    #[test]
    fn insert_and_delete_round_trip_through_all_three_structures() {
        let mut e = editor("hello");
        e.check_invariants();
        e.insert_at(5, b" world", SpanPolicy::Strict)
            .expect("insert");
        e.check_invariants();
        assert_eq!(e.text().unwrap(), b"hello world");
        assert_eq!(e.spans().text_len(), 11);

        e.delete_at(5, 6).expect("delete");
        e.check_invariants();
        assert_eq!(e.text().unwrap(), b"hello");
        assert_eq!(e.spans().text_len(), 5);
    }

    /// FR-1.2's allocation claim, at the editor level: typing changes nothing but the gap.
    #[test]
    fn typing_keeps_the_span_map_a_single_plain_run() {
        let mut e = Editor::new();
        for b in b"the quick brown fox" {
            e.insert_char(*b, SpanPolicy::Strict).expect("room");
        }
        e.check_invariants();
        assert_eq!(e.spans().len(), 1, "no styling, so no run boundaries");
        assert_eq!(e.spans().spans()[0].len(), 19);
        assert_eq!(e.text_len(), 19);
    }

    /// Styling, then editing around the styled region, must keep the spans consistent.
    #[test]
    fn edits_around_a_styled_region_keep_the_spans_tiling_the_text() {
        let mut e = editor("the quick brown fox");
        e.style_range(4, 9, STYLE_BOLD, RED).expect("style");
        e.check_invariants();

        e.insert_at(0, b">> ", SpanPolicy::Strict)
            .expect("insert before");
        e.check_invariants();
        assert!(
            e.style_at(4).style_flags & STYLE_BOLD == 0,
            "the run moved right"
        );
        assert!(
            e.style_at(7).style_flags & STYLE_BOLD != 0,
            "and kept its style"
        );

        // At the *end*, which is 22 bytes: ">> " plus the 19-byte phrase. Offset 20 lands inside "fox"
        // -- an earlier version used 20 and produced "...fo!x", which the test then read as a bug in
        // the editor rather than an arithmetic slip in itself.
        let end = e.text_len() as u32;
        e.insert_at(end, b"!", SpanPolicy::Strict)
            .expect("insert after");
        e.check_invariants();

        e.delete_at(0, 3).expect("delete the prefix");
        e.check_invariants();
        assert_eq!(e.text().unwrap(), b"the quick brown fox!");
        assert!(
            e.style_at(4).style_flags & STYLE_BOLD != 0,
            "back where it was"
        );
    }

    /// The gate's span-consistency requirement, across a leaf split.
    #[test]
    fn spans_stay_consistent_across_a_leaf_split() {
        // More than one leaf's worth, so the load splits.
        let long: String = "abcdefghij".repeat(600);
        let mut e = Editor::from_text(long.as_bytes()).expect("load");
        e.check_invariants();
        assert!(
            e.leaf_count() > 1,
            "{} bytes must span several leaves, got {}",
            e.text_len(),
            e.leaf_count()
        );

        // Style a range that straddles a leaf boundary.
        let mid = (e.text_len() / 2) as u32;
        e.style_range(mid - 10, mid + 10, STYLE_CODE, RED)
            .expect("style across the seam");
        e.check_invariants();

        // Now type enough to force further splits, and check after each.
        for i in 0..(crate::leaf::LEAF_CAPACITY as u32) {
            e.insert_char(b'x', SpanPolicy::Strict).expect("room");
            e.check_invariants();
            assert!(
                e.style_at(mid).style_flags & STYLE_CODE != 0 || i == 0,
                "the styled run lost its style at byte {mid} after {i} inserts"
            );
        }
    }

    /// Spans must survive a *merge*, which is the direction that destroys text if it is wrong.
    #[test]
    fn spans_stay_consistent_across_a_leaf_merge() {
        let mut e = Editor::from_text(&vec![b'a'; crate::leaf::LEAF_CAPACITY * 3]).expect("load");
        e.check_invariants();
        assert!(e.leaf_count() >= 3);

        // Style a run *inside the bytes that will survive*. Deleting from the front until 64 bytes
        // remain removes everything before offset 12,224, so a run styled at 4,091 -- where an earlier
        // version put it, to straddle a leaf boundary -- is itself deleted, and then asserting it
        // "survived the merges" asserts the wrong thing entirely.
        //
        // The leaves this run sits in are still exercised: offset 30 is in the last leaf, and the
        // deletes that bring the document down to 64 bytes merge every leaf above it, repeatedly.
        e.style_range(30, 40, STYLE_BOLD, RED).expect("style");
        e.check_invariants();
        assert!(
            e.style_at(35).style_flags & STYLE_BOLD != 0,
            "styled before the deletes"
        );

        // Delete from the **end** down to 64 bytes, merging leaves repeatedly on the way.
        //
        // Two earlier versions got this wrong in ways worth recording:
        //
        // * Deleting from offset 0 removes the head, and the styled run at 30..40 is in the head -- so
        //   asserting it "survived" asserts that deleted bytes came back. It reported one span left
        //   where three were expected, which was the correct answer to a wrong question.
        // * The loop also required `leaf_count() > 1`, and exited as soon as the leaves had merged,
        //   leaving 2,688 bytes and reporting "left: 2688, right: 64". Merging stops when two leaves'
        //   combined length exceeds `crate::leaf::LEAF_CAPACITY - GAP_MINIMUM`, so one leaf can hold several
        //   thousand bytes; the two conditions are not the same.
        while e.text_len() > 64 {
            let last = (e.text_len() - 1) as u32;
            e.delete_at(last, 1).expect("delete from the end");
            e.check_invariants();
        }
        assert!(e.leaf_count() < 3, "leaves merged: {}", e.leaf_count());
        // The surviving text is still styled where it was, and only where it was.
        assert_eq!(e.text_len(), 64);
        assert_eq!(e.spans().len(), 3, "plain 0..30, bold 30..40, plain 40..64");
        assert!(
            e.style_at(35).style_flags & STYLE_BOLD != 0,
            "the styled run did not survive the merges"
        );
        assert!(
            e.style_at(45).style_flags & STYLE_BOLD == 0,
            "and did not spread to its neighbours"
        );
    }

    #[test]
    fn undo_restores_bytes_and_styling() {
        let mut e = editor("hello world");
        e.style_range(0, 5, STYLE_BOLD, RED).expect("style");

        e.delete_at(0, 6).expect("delete 'hello '");
        e.check_invariants();
        assert_eq!(e.text().unwrap(), b"world");
        assert_eq!(e.spans().len(), 1);

        let outcome = e.undo().expect("undo");
        assert_eq!(outcome.kind, ActionKind::Insert);
        e.check_invariants();
        assert_eq!(e.text().unwrap(), b"hello world", "the bytes came back");
        // And the styling, which is the part a byte-only undo loses.
        assert!(
            e.style_at(0).style_flags & STYLE_BOLD != 0,
            "the bold run must come back with the bytes"
        );
        assert_eq!(e.spans().len(), 2, "bold 0..5, plain 5..11");
    }

    #[test]
    fn undo_of_an_insert_removes_exactly_those_bytes() {
        let mut e = editor("hello");
        e.insert_at(5, b"!!!", SpanPolicy::Strict).expect("insert");
        assert_eq!(e.text().unwrap(), b"hello!!!");
        e.undo().expect("undo");
        e.check_invariants();
        assert_eq!(e.text().unwrap(), b"hello");
        assert_eq!(e.spans().text_len(), 5);
    }

    /// Five hundred undo operations, the requirement's number.
    #[test]
    fn five_hundred_undo_operations() {
        let text: Vec<u8> = (0..600u32).map(|i| b'a' + (i % 26) as u8).collect();
        let mut e = Editor::from_text(&text).expect("load");
        e.check_invariants();

        // 600 edits on top of the load; the stack holds the newest 500.
        for _ in 0..600u32 {
            e.insert_at(e.text_len() as u32, b".", SpanPolicy::Strict)
                .expect("append");
        }
        e.check_invariants();
        assert_eq!(e.undo_depth(), 500, "bounded at 500");
        assert_eq!(e.text_len(), 600 + 600);

        for _ in 0..500 {
            e.undo().expect("undo");
            e.check_invariants();
        }
        assert_eq!(e.text_len(), 600 + 100, "600 appended, 500 undone");
        assert_eq!(e.text().unwrap(), {
            let mut want = text;
            want.extend(std::iter::repeat_n(b'.', 100));
            want
        });
    }

    #[test]
    fn undo_with_nothing_recorded_is_an_error_not_a_panic() {
        let mut e = Editor::new();
        assert_eq!(e.undo(), Err(EditorError::NothingToUndo));
        assert!(!e.can_undo());
    }

    #[test]
    fn a_mid_character_offset_is_refused() {
        let mut e = Editor::from_text("héllo".as_bytes()).expect("load");
        // `é` is bytes 1 and 2.
        let err = e
            .insert_at(2, b"x", SpanPolicy::Strict)
            .expect_err("inside é");
        assert!(
            matches!(err, EditorError::NotCharBoundary { offset: 2 }),
            "got {err:?}"
        );
        e.check_invariants();
        assert_eq!(e.text().unwrap(), "héllo".as_bytes(), "and nothing changed");
    }

    #[test]
    fn a_delete_past_the_end_is_refused_without_touching_anything() {
        let mut e = editor("hello");
        let before = e.text().unwrap();
        assert!(e.delete_at(3, 100).is_err());
        e.check_invariants();
        assert_eq!(e.text().unwrap(), before);
        assert_eq!(e.undo_depth(), 0, "a refused edit must not be recorded");
    }

    #[test]
    fn backspace_deletes_the_byte_before_the_caret() {
        let mut e = editor("abc");
        e.backspace().expect("backspace");
        e.check_invariants();
        assert_eq!(e.text().unwrap(), b"ab");
        e.backspace().expect("backspace");
        assert_eq!(e.text().unwrap(), b"a");
        e.backspace().expect("backspace");
        assert_eq!(e.text().unwrap(), b"");
        assert!(e.backspace().is_err(), "nothing left to delete");
        e.check_invariants();
    }

    /// The long randomised sequence, which is the only thing that composes the three structures.
    #[test]
    fn invariants_hold_under_a_random_edit_sequence() {
        let mut state = 0xDEAD_BEEF_CAFE_1234u64;
        let mut next = move |n: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % u64::from(n.max(1))) as u32
        };
        let mut e = Editor::new();
        let mut text: Vec<u8> = Vec::new();
        // The model needs an undo stack too. An earlier version kept only `text` and called
        // `e.undo()` on one branch of the match, so the model never unwound: it diverged at step 2 with
        // the editor holding `[97]` and the model holding `[97, 98, 99, 100, 101, 102]`. The failure was
        // in the model, not the editor.
        //
        // Each entry is the *inverse* of the edit, so undoing is a splice with no case analysis.
        let mut model_undo: Vec<(usize, Vec<u8>, bool)> = Vec::new();

        for step in 0..3_000u32 {
            match next(4) {
                0 | 1 => {
                    let len = 1 + next(6);
                    let at = next((text.len() + 1) as u32) as usize;
                    let bytes: Vec<u8> = (0..len)
                        .map(|i| b'a' + ((at as u8).wrapping_add(i as u8)) % 26)
                        .collect();
                    e.insert_at(at as u32, &bytes, SpanPolicy::Strict)
                        .unwrap_or_else(|err| panic!("insert at {at} at step {step}: {err}"));
                    text.splice(at..at, bytes.iter().copied());
                    model_undo.push((at, bytes, true));
                }
                2 => {
                    if text.is_empty() {
                        continue;
                    }
                    let at = next(text.len() as u32) as usize;
                    let len = 1 + next(4) as usize;
                    let len = len.min(text.len() - at);
                    e.delete_at(at as u32, len as u32)
                        .unwrap_or_else(|err| panic!("delete {at}+{len} at step {step}: {err}"));
                    let removed: Vec<u8> = text.drain(at..at + len).collect();
                    model_undo.push((at, removed, false));
                }
                _ => {
                    if e.can_undo() {
                        e.undo()
                            .unwrap_or_else(|err| panic!("undo at step {step}: {err}"));
                        if let Some((at, bytes, was_insert)) = model_undo.pop() {
                            if was_insert {
                                text.drain(at..at + bytes.len());
                            } else {
                                text.splice(at..at, bytes);
                            }
                        }
                    }
                }
            }
            e.check_invariants();
            assert_eq!(
                e.text().unwrap(),
                text,
                "the editor's text diverged from the model at step {step}"
            );
        }
    }
}
