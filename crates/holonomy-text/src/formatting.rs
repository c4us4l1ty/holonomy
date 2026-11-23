//! Rich-text style layer and toolbar state over Spine byte coordinates (ROADMAP Block 7).
//!
//! Constitutional UI law `U0` (`docs/UI-SPEC.md` §1) requires that formatting is a property of
//! the document's Spine rather than hidden editor widget state:
//!
//! * Unstyled documents hold `StyleMap::empty()`, which carries `None` and therefore adds **zero
//!   heap bytes** to 30 M-word opens (`tests/open_memory.rs`).
//! * Non-default formatting lives in an `Arc`-shared, sorted, non-overlapping table of
//!   [`DocStyleSpan`] runs plus a deduplicated link-target table, path-copied alongside every
//!   [`crate::Spine`] structural operation without cutting underlying [`crate::Slab`] leaves
//!   (`AUDIT.md` `OQ-9`).
//! * Because a [`crate::Spine`] value owns both its slab root and its [`StyleMap`], root-swap undo
//!   and redo revert or restore formatting across arbitrary slab boundaries in one `O(1)` step.
//! * [`ToolbarState`] inspects any caret or byte selection and yields [`TriState::Mixed`] (`—`)
//!   whenever an attribute takes more than one value across the selection (`UI-SPEC.md` §3).

use core::num::NonZeroU32;
use std::sync::Arc;

use crate::spine::SpineError;

/// Packed inline boolean flag: bold weight.
pub const FLAG_BOLD: u8 = 0x01;
/// Packed inline boolean flag: italic posture.
pub const FLAG_ITALIC: u8 = 0x02;
/// Packed inline boolean flag: underline decoration.
pub const FLAG_UNDERLINE: u8 = 0x04;
/// Packed inline boolean flag: strikethrough decoration.
pub const FLAG_STRIKETHROUGH: u8 = 0x08;
/// Packed inline boolean flag: inline monospace code span.
pub const FLAG_CODE_SPAN: u8 = 0x10;
/// Packed inline boolean flag: horizontal-rule block divider.
pub const FLAG_HORIZONTAL_RULE: u8 = 0x20;

/// Document font family selector (`UI-SPEC.md` §3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FontFamily {
    /// Proportional sans-serif body/UI family (default).
    #[default]
    Sans,
    /// Proportional serif editorial family.
    Serif,
    /// Fixed-width monospace code/tabular family.
    Mono,
}

impl FontFamily {
    /// Wire code (`0..=2`) stored in `RECORD_MANIFEST` v2.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Sans => 0,
            Self::Serif => 1,
            Self::Mono => 2,
        }
    }

    /// Decodes a wire code (`0..=2`).
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Sans),
            1 => Some(Self::Serif),
            2 => Some(Self::Mono),
            _ => None,
        }
    }
}

/// Paragraph heading hierarchy (`UI-SPEC.md` §2–§3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Heading {
    /// Body text (`20 px` at `1.0x`, `1_000` milli-em).
    #[default]
    None,
    /// Document title (`41 px` / `20 px` = `2_050` milli-em).
    Title,
    /// Level-1 heading (`31 px` / `20 px` = `1_550` milli-em).
    H1,
    /// Level-2 heading (`25 px` / `20 px` = `1_250` milli-em).
    H2,
    /// Level-3 heading (`21 px` / `20 px` = `1_050` milli-em).
    H3,
}

impl Heading {
    /// Wire code (`0..=4`) stored in `RECORD_MANIFEST` v2.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Title => 1,
            Self::H1 => 2,
            Self::H2 => 3,
            Self::H3 => 4,
        }
    }

    /// Decodes a wire code (`0..=4`).
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::None),
            1 => Some(Self::Title),
            2 => Some(Self::H1),
            3 => Some(Self::H2),
            4 => Some(Self::H3),
            _ => None,
        }
    }

    /// Default typographic scale in thousandths of body size (`UI-SPEC.md` §2).
    #[must_use]
    pub const fn default_size_milli(self) -> u32 {
        match self {
            Self::None => 1_000,
            Self::Title => 2_050,
            Self::H1 => 1_550,
            Self::H2 => 1_250,
            Self::H3 => 1_050,
        }
    }

    /// Whether this heading level renders with semibold/bold weight by default.
    #[must_use]
    pub const fn is_bold_by_default(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Paragraph horizontal alignment (`UI-SPEC.md` §3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Alignment {
    /// Start-aligned along the paragraph's base direction (default).
    #[default]
    Start,
    /// Horizontally centered within the measure.
    Center,
    /// End-aligned along the paragraph's base direction.
    End,
    /// Justified across full lines (ragged on final line).
    Justify,
}

impl Alignment {
    /// Wire code (`0..=3`) stored in `RECORD_MANIFEST` v2.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Start => 0,
            Self::Center => 1,
            Self::End => 2,
            Self::Justify => 3,
        }
    }

    /// Decodes a wire code (`0..=3`).
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Start),
            1 => Some(Self::Center),
            2 => Some(Self::End),
            3 => Some(Self::Justify),
            _ => None,
        }
    }
}

/// Structural list membership (`UI-SPEC.md` §3: real list runs, never literal `"- "` text).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ListKind {
    /// Ordinary paragraph (not a list item).
    #[default]
    None,
    /// Unordered bullet item at nesting `depth` (`1..=8`).
    Bullet {
        /// Visual/structural nesting level (`1..=8`).
        depth: u8,
    },
    /// Ordered numbered item at nesting `depth` (`1..=8`) starting at `start` (`>= 1`).
    Ordered {
        /// Visual/structural nesting level (`1..=8`).
        depth: u8,
        /// Ordinal of this item (`>= 1`).
        start: u32,
    },
}

impl ListKind {
    /// Encodes `(list_kind, list_depth, list_start)` for `RECORD_MANIFEST` v2.
    #[must_use]
    pub const fn to_wire(self) -> (u8, u8, u32) {
        match self {
            Self::None => (0, 0, 0),
            Self::Bullet { depth } => (1, if depth == 0 { 1 } else { depth }, 0),
            Self::Ordered { depth, start } => (
                2,
                if depth == 0 { 1 } else { depth },
                if start == 0 { 1 } else { start },
            ),
        }
    }

    /// Decodes `(list_kind, list_depth, list_start)` from `RECORD_MANIFEST` v2.
    #[must_use]
    pub const fn from_wire(kind: u8, depth: u8, start: u32) -> Option<Self> {
        match kind {
            0 if depth == 0 && start == 0 => Some(Self::None),
            1 if depth >= 1 && depth <= 8 && start == 0 => Some(Self::Bullet { depth }),
            2 if depth >= 1 && depth <= 8 && start >= 1 => Some(Self::Ordered { depth, start }),
            _ => None,
        }
    }

    /// Nesting depth (`0` when [`Self::None`]).
    #[must_use]
    pub const fn depth(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Bullet { depth } | Self::Ordered { depth, .. } => depth,
        }
    }
}

/// Complete rich-text style carried by a [`DocStyleSpan`] in the Spine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocStyle {
    /// Font family (`Sans`, `Serif`, `Mono`).
    pub family: FontFamily,
    /// Explicit size in thousandths of body size (`1_000` = `1.0x`).
    pub size_milli: u32,
    /// Bold weight flag.
    pub bold: bool,
    /// Italic posture flag.
    pub italic: bool,
    /// Underline decoration flag.
    pub underline: bool,
    /// Strikethrough decoration flag.
    pub strikethrough: bool,
    /// Inline monospace code-span flag (`18 px / 20 px = 900` milli-em when `size_milli == 1_000`).
    pub code_span: bool,
    /// Paragraph heading level.
    pub heading: Heading,
    /// Paragraph horizontal alignment.
    pub alignment: Alignment,
    /// Structural list item kind and depth.
    pub list: ListKind,
    /// Blockquote nesting depth (`0..=8`).
    pub blockquote: u8,
    /// Horizontal-rule divider flag.
    pub horizontal_rule: bool,
    /// Optional 1-based index into [`StyleMap::links`].
    pub link_id: Option<NonZeroU32>,
}

impl Default for DocStyle {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl DocStyle {
    /// Unstyled body defaults (`Sans`, `1_000` milli-em, start-aligned, no inline/block marks).
    pub const DEFAULT: Self = Self {
        family: FontFamily::Sans,
        size_milli: 1_000,
        bold: false,
        italic: false,
        underline: false,
        strikethrough: false,
        code_span: false,
        heading: Heading::None,
        alignment: Alignment::Start,
        list: ListKind::None,
        blockquote: 0,
        horizontal_rule: false,
        link_id: None,
    };

    /// Whether every attribute matches [`Self::DEFAULT`].
    #[must_use]
    pub const fn is_default(&self) -> bool {
        matches!(self.family, FontFamily::Sans)
            && self.size_milli == 1_000
            && !self.bold
            && !self.italic
            && !self.underline
            && !self.strikethrough
            && !self.code_span
            && matches!(self.heading, Heading::None)
            && matches!(self.alignment, Alignment::Start)
            && matches!(self.list, ListKind::None)
            && self.blockquote == 0
            && !self.horizontal_rule
            && self.link_id.is_none()
    }

    /// Effective font size in thousandths, honoring heading and inline-code defaults when no
    /// explicit custom `size_milli` override has been set (`UI-SPEC.md` §2).
    #[must_use]
    pub const fn effective_size_milli(&self) -> u32 {
        if self.size_milli != 1_000 {
            self.size_milli
        } else if !matches!(self.heading, Heading::None) {
            self.heading.default_size_milli()
        } else if self.code_span {
            900
        } else {
            1_000
        }
    }

    /// Effective bold weight (`true` when explicitly bold or inside a heading).
    #[must_use]
    pub const fn effective_bold(&self) -> bool {
        self.bold || self.heading.is_bold_by_default()
    }

    /// Effective font family (`Mono` when `code_span` is set and `family` is `Sans`).
    #[must_use]
    pub const fn effective_family(&self) -> FontFamily {
        if self.code_span && matches!(self.family, FontFamily::Sans) {
            FontFamily::Mono
        } else {
            self.family
        }
    }

    /// Packs the six boolean attributes into one manifest flags byte.
    #[must_use]
    pub const fn flags_byte(&self) -> u8 {
        let mut flags = 0u8;
        if self.bold {
            flags |= FLAG_BOLD;
        }
        if self.italic {
            flags |= FLAG_ITALIC;
        }
        if self.underline {
            flags |= FLAG_UNDERLINE;
        }
        if self.strikethrough {
            flags |= FLAG_STRIKETHROUGH;
        }
        if self.code_span {
            flags |= FLAG_CODE_SPAN;
        }
        if self.horizontal_rule {
            flags |= FLAG_HORIZONTAL_RULE;
        }
        flags
    }

    /// Reconstructs a [`DocStyle`] from wire manifest fields, returning `None` if any value is
    /// out of range.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub const fn from_wire(
        flags: u8,
        family: u8,
        heading: u8,
        alignment: u8,
        list_kind: u8,
        list_depth: u8,
        blockquote: u8,
        size_milli: u32,
        list_start: u32,
        link_id: u32,
    ) -> Option<Self> {
        if (flags & !0x3F) != 0 || blockquote > 8 || size_milli == 0 {
            return None;
        }
        let Some(family) = FontFamily::from_code(family) else {
            return None;
        };
        let Some(heading) = Heading::from_code(heading) else {
            return None;
        };
        let Some(alignment) = Alignment::from_code(alignment) else {
            return None;
        };
        let Some(list) = ListKind::from_wire(list_kind, list_depth, list_start) else {
            return None;
        };
        Some(Self {
            family,
            size_milli,
            bold: (flags & FLAG_BOLD) != 0,
            italic: (flags & FLAG_ITALIC) != 0,
            underline: (flags & FLAG_UNDERLINE) != 0,
            strikethrough: (flags & FLAG_STRIKETHROUGH) != 0,
            code_span: (flags & FLAG_CODE_SPAN) != 0,
            heading,
            alignment,
            list,
            blockquote,
            horizontal_rule: (flags & FLAG_HORIZONTAL_RULE) != 0,
            link_id: NonZeroU32::new(link_id),
        })
    }
}

/// Partial style update applied over a byte selection (`[start, end)`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StylePatch {
    /// Optional font-family override.
    pub family: Option<FontFamily>,
    /// Optional explicit size in thousandths (`> 0`).
    pub size_milli: Option<u32>,
    /// Optional bold override.
    pub bold: Option<bool>,
    /// Optional italic override.
    pub italic: Option<bool>,
    /// Optional underline override.
    pub underline: Option<bool>,
    /// Optional strikethrough override.
    pub strikethrough: Option<bool>,
    /// Optional inline code-span override.
    pub code_span: Option<bool>,
    /// Optional paragraph heading override.
    pub heading: Option<Heading>,
    /// Optional paragraph alignment override.
    pub alignment: Option<Alignment>,
    /// Optional list-kind override.
    pub list: Option<ListKind>,
    /// Optional blockquote depth override (`0..=8`).
    pub blockquote: Option<u8>,
    /// Optional horizontal-rule override.
    pub horizontal_rule: Option<bool>,
    /// Optional link-id override (`Some(None)` removes an existing link).
    pub link_id: Option<Option<NonZeroU32>>,
}

impl StylePatch {
    /// An empty patch that leaves every attribute unchanged.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            family: None,
            size_milli: None,
            bold: None,
            italic: None,
            underline: None,
            strikethrough: None,
            code_span: None,
            heading: None,
            alignment: None,
            list: None,
            blockquote: None,
            horizontal_rule: None,
            link_id: None,
        }
    }

    /// A patch that resets every formatting attribute back to [`DocStyle::DEFAULT`].
    #[must_use]
    pub const fn clear_all() -> Self {
        Self {
            family: Some(FontFamily::Sans),
            size_milli: Some(1_000),
            bold: Some(false),
            italic: Some(false),
            underline: Some(false),
            strikethrough: Some(false),
            code_span: Some(false),
            heading: Some(Heading::None),
            alignment: Some(Alignment::Start),
            list: Some(ListKind::None),
            blockquote: Some(0),
            horizontal_rule: Some(false),
            link_id: Some(None),
        }
    }

    /// Overrides font family.
    #[must_use]
    pub const fn with_family(mut self, family: FontFamily) -> Self {
        self.family = Some(family);
        self
    }

    /// Overrides explicit size in thousandths (`clamped >= 1`).
    #[must_use]
    pub const fn with_size_milli(mut self, size_milli: u32) -> Self {
        self.size_milli = Some(if size_milli == 0 { 1 } else { size_milli });
        self
    }

    /// Overrides bold weight.
    #[must_use]
    pub const fn with_bold(mut self, bold: bool) -> Self {
        self.bold = Some(bold);
        self
    }

    /// Overrides italic posture.
    #[must_use]
    pub const fn with_italic(mut self, italic: bool) -> Self {
        self.italic = Some(italic);
        self
    }

    /// Overrides underline decoration.
    #[must_use]
    pub const fn with_underline(mut self, underline: bool) -> Self {
        self.underline = Some(underline);
        self
    }

    /// Overrides strikethrough decoration.
    #[must_use]
    pub const fn with_strikethrough(mut self, strikethrough: bool) -> Self {
        self.strikethrough = Some(strikethrough);
        self
    }

    /// Overrides inline code-span mark.
    #[must_use]
    pub const fn with_code_span(mut self, code_span: bool) -> Self {
        self.code_span = Some(code_span);
        self
    }

    /// Overrides paragraph heading level.
    #[must_use]
    pub const fn with_heading(mut self, heading: Heading) -> Self {
        self.heading = Some(heading);
        self
    }

    /// Overrides paragraph horizontal alignment.
    #[must_use]
    pub const fn with_alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = Some(alignment);
        self
    }

    /// Overrides structural list membership.
    #[must_use]
    pub const fn with_list(mut self, list: ListKind) -> Self {
        self.list = Some(list);
        self
    }

    /// Overrides blockquote nesting depth (`clamped <= 8`).
    #[must_use]
    pub const fn with_blockquote(mut self, depth: u8) -> Self {
        self.blockquote = Some(if depth > 8 { 8 } else { depth });
        self
    }

    /// Overrides horizontal-rule mark.
    #[must_use]
    pub const fn with_horizontal_rule(mut self, hr: bool) -> Self {
        self.horizontal_rule = Some(hr);
        self
    }

    /// Overrides link reference (`None` clears link).
    #[must_use]
    pub const fn with_link_id(mut self, link_id: Option<NonZeroU32>) -> Self {
        self.link_id = Some(link_id);
        self
    }

    /// Applies every specified field of this patch onto `base`.
    #[must_use]
    pub const fn apply_to(&self, mut base: DocStyle) -> DocStyle {
        if let Some(family) = self.family {
            base.family = family;
        }
        if let Some(size_milli) = self.size_milli {
            base.size_milli = if size_milli == 0 { 1 } else { size_milli };
        }
        if let Some(bold) = self.bold {
            base.bold = bold;
        }
        if let Some(italic) = self.italic {
            base.italic = italic;
        }
        if let Some(underline) = self.underline {
            base.underline = underline;
        }
        if let Some(strikethrough) = self.strikethrough {
            base.strikethrough = strikethrough;
        }
        if let Some(code_span) = self.code_span {
            base.code_span = code_span;
        }
        if let Some(heading) = self.heading {
            base.heading = heading;
        }
        if let Some(alignment) = self.alignment {
            base.alignment = alignment;
        }
        if let Some(list) = self.list {
            base.list = list;
        }
        if let Some(blockquote) = self.blockquote {
            base.blockquote = if blockquote > 8 { 8 } else { blockquote };
        }
        if let Some(hr) = self.horizontal_rule {
            base.horizontal_rule = hr;
        }
        if let Some(link_id) = self.link_id {
            base.link_id = link_id;
        }
        base
    }
}

/// One non-default rich-text style span `[start, end)` in Spine byte coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocStyleSpan {
    /// Inclusive start byte offset (`start < end`).
    pub start: u64,
    /// Exclusive end byte offset.
    pub end: u64,
    /// Style attributes over `[start, end)`.
    pub style: DocStyle,
}

/// Toolbar inspection state for a single attribute (`UI-SPEC.md` §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriState<T> {
    /// Every byte in the selection shares this exact attribute value.
    Uniform(T),
    /// The selection spans at least two distinct values (`—` indeterminate dash in the Ribbon).
    Mixed,
}

impl<T: PartialEq> TriState<T> {
    fn fold(&mut self, next: T) {
        if let Self::Uniform(current) = self {
            if *current != next {
                *self = Self::Mixed;
            }
        }
    }

    /// Returns `Some(value)` when uniform, or `None` when mixed.
    #[must_use]
    pub fn uniform(self) -> Option<T> {
        match self {
            Self::Uniform(v) => Some(v),
            Self::Mixed => None,
        }
    }

    /// Whether the selection carries multiple distinct values.
    #[must_use]
    pub const fn is_mixed(&self) -> bool {
        matches!(self, Self::Mixed)
    }

    /// Renders `fmt(v)` when uniform or the `UI-SPEC.md` §3 indeterminate em-dash `"—"` when mixed.
    #[must_use]
    pub fn display_or_dash(&self, fmt: impl FnOnce(&T) -> String) -> String {
        match self {
            Self::Uniform(v) => fmt(v),
            Self::Mixed => "\u{2014}".to_owned(),
        }
    }
}

/// Complete toolbar read-out over a caret or byte selection (`UI-SPEC.md` §3, Law `U0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolbarState {
    /// Font family across the selection.
    pub family: TriState<FontFamily>,
    /// Effective font size (milli-em) across the selection.
    pub size_milli: TriState<u32>,
    /// Bold state across the selection.
    pub bold: TriState<bool>,
    /// Italic state across the selection.
    pub italic: TriState<bool>,
    /// Underline state across the selection.
    pub underline: TriState<bool>,
    /// Strikethrough state across the selection.
    pub strikethrough: TriState<bool>,
    /// Inline code-span state across the selection.
    pub code_span: TriState<bool>,
    /// Paragraph heading state across the selection.
    pub heading: TriState<Heading>,
    /// Paragraph alignment state across the selection.
    pub alignment: TriState<Alignment>,
    /// Structural list state across the selection.
    pub list: TriState<ListKind>,
    /// Blockquote depth across the selection.
    pub blockquote: TriState<u8>,
    /// Horizontal-rule state across the selection.
    pub horizontal_rule: TriState<bool>,
    /// Link URL target across the selection (`None` when unlinked).
    pub link: TriState<Option<String>>,
}

impl ToolbarState {
    fn from_single(style: DocStyle, links: &[String]) -> Self {
        let link_url = style
            .link_id
            .and_then(|id| links.get(id.get() as usize - 1).cloned());
        Self {
            family: TriState::Uniform(style.family),
            size_milli: TriState::Uniform(style.effective_size_milli()),
            bold: TriState::Uniform(style.bold),
            italic: TriState::Uniform(style.italic),
            underline: TriState::Uniform(style.underline),
            strikethrough: TriState::Uniform(style.strikethrough),
            code_span: TriState::Uniform(style.code_span),
            heading: TriState::Uniform(style.heading),
            alignment: TriState::Uniform(style.alignment),
            list: TriState::Uniform(style.list),
            blockquote: TriState::Uniform(style.blockquote),
            horizontal_rule: TriState::Uniform(style.horizontal_rule),
            link: TriState::Uniform(link_url),
        }
    }

    fn fold_style(&mut self, style: DocStyle, links: &[String]) {
        let link_url = style
            .link_id
            .and_then(|id| links.get(id.get() as usize - 1).cloned());
        self.family.fold(style.family);
        self.size_milli.fold(style.effective_size_milli());
        self.bold.fold(style.bold);
        self.italic.fold(style.italic);
        self.underline.fold(style.underline);
        self.strikethrough.fold(style.strikethrough);
        self.code_span.fold(style.code_span);
        self.heading.fold(style.heading);
        self.alignment.fold(style.alignment);
        self.list.fold(style.list);
        self.blockquote.fold(style.blockquote);
        self.horizontal_rule.fold(style.horizontal_rule);
        self.link.fold(link_url);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StyleInner {
    runs: Vec<DocStyleSpan>,
    links: Vec<String>,
}

/// Immutable, `Arc`-shared style layer carried by [`crate::Spine`].
///
/// An unstyled document holds `None` and performs zero heap allocations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StyleMap {
    inner: Option<Arc<StyleInner>>,
}

impl StyleMap {
    /// Creates an empty style map with zero heap allocations.
    #[must_use]
    pub const fn empty() -> Self {
        Self { inner: None }
    }

    /// Whether the document carries zero non-default style spans.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_none()
    }

    /// Whether `self` and `other` share the exact same underlying style allocation in `O(1)`.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (&self.inner, &other.inner) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Canonical sorted, non-overlapping style spans.
    #[must_use]
    pub fn runs(&self) -> &[DocStyleSpan] {
        self.inner.as_deref().map_or(&[], |inner| &inner.runs)
    }

    /// Deduplicated link-target table referenced by 1-based [`DocStyle::link_id`].
    #[must_use]
    pub fn links(&self) -> &[String] {
        self.inner.as_deref().map_or(&[], |inner| &inner.links)
    }

    /// Resolves a 1-based [`DocStyle::link_id`] to its target URL.
    #[must_use]
    pub fn link_target(&self, id: NonZeroU32) -> Option<&str> {
        self.links().get(id.get() as usize - 1).map(String::as_str)
    }

    /// Builds a normalized [`StyleMap`] from raw spans and link targets, coalescing adjacent equal
    /// spans, dropping default-styled spans, compacting unreferenced links, and collapsing to
    /// `None` when empty.
    #[must_use]
    pub fn from_raw(runs: Vec<DocStyleSpan>, links: Vec<String>) -> Self {
        Self::normalize(runs, links)
    }

    fn normalize(mut runs: Vec<DocStyleSpan>, links: Vec<String>) -> Self {
        runs.retain(|r| r.start < r.end && !r.style.is_default());
        if runs.is_empty() {
            return Self::empty();
        }
        runs.sort_by_key(|r| (r.start, r.end));

        let mut merged: Vec<DocStyleSpan> = Vec::with_capacity(runs.len());
        for run in runs {
            if let Some(prev) = merged.last_mut() {
                if prev.end > run.start {
                    // Clamp any overlap deterministically in favour of the later span.
                    prev.end = run.start;
                }
                if prev.start == prev.end {
                    merged.pop();
                }
            }
            if let Some(prev) = merged.last_mut() {
                if prev.end == run.start && prev.style == run.style {
                    prev.end = run.end;
                    continue;
                }
            }
            merged.push(run);
        }
        if merged.is_empty() {
            return Self::empty();
        }

        // Compact link table so only referenced links remain, in deterministic first-use order.
        let mut new_links: Vec<String> = Vec::new();
        let mut remap = vec![None; links.len()];
        for run in &mut merged {
            if let Some(old_id) = run.style.link_id {
                let idx = old_id.get() as usize - 1;
                if let Some(url) = links.get(idx) {
                    let mapped = if let Some(existing) = remap[idx] {
                        existing
                    } else if let Some(pos) = new_links.iter().position(|u| u == url) {
                        let id = NonZeroU32::new((pos + 1) as u32).expect("pos + 1 >= 1");
                        remap[idx] = Some(id);
                        id
                    } else {
                        new_links.push(url.clone());
                        let id = NonZeroU32::new(new_links.len() as u32).expect("len >= 1");
                        remap[idx] = Some(id);
                        id
                    };
                    run.style.link_id = Some(mapped);
                } else {
                    run.style.link_id = None;
                }
            }
        }
        // Different input link IDs can resolve to the same target. Coalesce once more after
        // remapping so split/concat and save/reopen preserve canonical, byte-identical runs.
        let mut normalized: Vec<DocStyleSpan> = Vec::with_capacity(merged.len());
        for run in merged.into_iter().filter(|r| !r.style.is_default()) {
            if let Some(prev) = normalized.last_mut() {
                if prev.end == run.start && prev.style == run.style {
                    prev.end = run.end;
                    continue;
                }
            }
            normalized.push(run);
        }
        if normalized.is_empty() {
            return Self::empty();
        }

        Self {
            inner: Some(Arc::new(StyleInner {
                runs: normalized,
                links: new_links,
            })),
        }
    }

    /// Returns the active [`DocStyle`] at caret `offset`.
    #[must_use]
    pub fn style_at(&self, offset: u64) -> DocStyle {
        let runs = self.runs();
        if runs.is_empty() {
            return DocStyle::DEFAULT;
        }
        if offset > 0 {
            let idx = runs.partition_point(|r| r.end < offset);
            if let Some(run) = runs.get(idx) {
                if run.start < offset && offset <= run.end {
                    return run.style;
                }
            }
        }
        let idx = runs.partition_point(|r| r.end <= offset);
        if let Some(run) = runs.get(idx) {
            if run.start <= offset && offset < run.end {
                return run.style;
            }
        }
        DocStyle::DEFAULT
    }

    /// Returns non-default spans overlapping `[start, end)`, clipped to `[start, end)`, in
    /// `O(log S + K)`.
    #[must_use]
    pub fn spans_in(&self, start: u64, end: u64) -> Vec<DocStyleSpan> {
        if start >= end {
            return Vec::new();
        }
        let runs = self.runs();
        if runs.is_empty() {
            return Vec::new();
        }
        let first = runs.partition_point(|r| r.end <= start);
        let mut out = Vec::new();
        for run in &runs[first..] {
            if run.start >= end {
                break;
            }
            out.push(DocStyleSpan {
                start: run.start.max(start),
                end: run.end.min(end),
                style: run.style,
            });
        }
        out
    }

    /// Computes the [`ToolbarState`] for caret (`start == end`) or selection `[start, end)`.
    #[must_use]
    pub fn toolbar_state(&self, doc_len: u64, start: u64, end: u64) -> ToolbarState {
        let start = start.min(doc_len);
        let end = end.min(doc_len);
        let (lo, hi) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let links = self.links();
        if lo == hi {
            return ToolbarState::from_single(self.style_at(lo), links);
        }

        let clipped = self.spans_in(lo, hi);
        if clipped.is_empty() {
            return ToolbarState::from_single(DocStyle::DEFAULT, links);
        }

        let mut cursor = lo;
        let mut state: Option<ToolbarState> = None;
        for span in clipped {
            if cursor < span.start {
                match &mut state {
                    None => state = Some(ToolbarState::from_single(DocStyle::DEFAULT, links)),
                    Some(s) => s.fold_style(DocStyle::DEFAULT, links),
                }
            }
            match &mut state {
                None => state = Some(ToolbarState::from_single(span.style, links)),
                Some(s) => s.fold_style(span.style, links),
            }
            cursor = span.end;
        }
        if cursor < hi {
            match &mut state {
                None => state = Some(ToolbarState::from_single(DocStyle::DEFAULT, links)),
                Some(s) => s.fold_style(DocStyle::DEFAULT, links),
            }
        }
        state.unwrap_or_else(|| ToolbarState::from_single(DocStyle::DEFAULT, links))
    }

    /// Splits the style map at `at` into `[0, at)` and `[at, len)` (shifted by `-at`).
    #[must_use]
    pub fn split(&self, at: u64) -> (Self, Self) {
        let Some(inner) = &self.inner else {
            return (Self::empty(), Self::empty());
        };
        let mut left = Vec::new();
        let mut right = Vec::new();
        for run in &inner.runs {
            if run.end <= at {
                left.push(*run);
            } else if run.start >= at {
                right.push(DocStyleSpan {
                    start: run.start - at,
                    end: run.end - at,
                    style: run.style,
                });
            } else {
                left.push(DocStyleSpan {
                    start: run.start,
                    end: at,
                    style: run.style,
                });
                right.push(DocStyleSpan {
                    start: 0,
                    end: run.end - at,
                    style: run.style,
                });
            }
        }
        (
            Self::normalize(left, inner.links.clone()),
            Self::normalize(right, inner.links.clone()),
        )
    }

    /// Concatenates `other` after `self` (whose document byte length is `left_len`).
    #[must_use]
    pub fn concat(&self, left_len: u64, other: &Self) -> Self {
        match (&self.inner, &other.inner) {
            (None, None) => Self::empty(),
            (Some(_), None) => self.clone(),
            (None, Some(right)) => {
                if left_len == 0 {
                    return other.clone();
                }
                let shifted = right
                    .runs
                    .iter()
                    .map(|r| DocStyleSpan {
                        start: r.start + left_len,
                        end: r.end + left_len,
                        style: r.style,
                    })
                    .collect();
                Self::normalize(shifted, right.links.clone())
            }
            (Some(left), Some(right)) => {
                let mut links = left.links.clone();
                let link_base = links.len() as u32;
                links.extend(right.links.iter().cloned());
                let mut runs = left.runs.clone();
                runs.reserve(right.runs.len());
                for r in &right.runs {
                    let mut style = r.style;
                    if let Some(id) = style.link_id {
                        style.link_id = NonZeroU32::new(id.get().saturating_add(link_base));
                    }
                    runs.push(DocStyleSpan {
                        start: r.start + left_len,
                        end: r.end + left_len,
                        style,
                    });
                }
                Self::normalize(runs, links)
            }
        }
    }

    /// Inserts `len` bytes at `at`, extending a run when `at` sits strictly inside it and shifting
    /// runs at or after `at` right by `len`.
    #[must_use]
    pub fn insert_gap(&self, at: u64, len: u64) -> Self {
        if len == 0 {
            return self.clone();
        }
        let Some(inner) = &self.inner else {
            return Self::empty();
        };
        let mut runs = Vec::with_capacity(inner.runs.len());
        for r in &inner.runs {
            if r.end <= at {
                runs.push(*r);
            } else if r.start >= at {
                runs.push(DocStyleSpan {
                    start: r.start + len,
                    end: r.end + len,
                    style: r.style,
                });
            } else {
                runs.push(DocStyleSpan {
                    start: r.start,
                    end: r.end + len,
                    style: r.style,
                });
            }
        }
        Self::normalize(runs, inner.links.clone())
    }

    /// Removes byte range `[start, end)` and shifts subsequent spans left by `end - start`.
    #[must_use]
    pub fn remove_range(&self, start: u64, end: u64) -> Self {
        if start >= end {
            return self.clone();
        }
        let Some(inner) = &self.inner else {
            return Self::empty();
        };
        let removed = end - start;
        let mut runs = Vec::with_capacity(inner.runs.len());
        for r in &inner.runs {
            if r.end <= start {
                runs.push(*r);
            } else if r.start >= end {
                runs.push(DocStyleSpan {
                    start: r.start - removed,
                    end: r.end - removed,
                    style: r.style,
                });
            } else {
                if r.start < start {
                    runs.push(DocStyleSpan {
                        start: r.start,
                        end: start,
                        style: r.style,
                    });
                }
                if r.end > end {
                    runs.push(DocStyleSpan {
                        start,
                        end: r.end - removed,
                        style: r.style,
                    });
                }
            }
        }
        Self::normalize(runs, inner.links.clone())
    }

    /// Replaces `[start, end)` with `new_len` bytes, stretching a uniform style across `new_len`
    /// when `[start, end)` carries one uniform style (as when sealing a drafted open leaf).
    #[must_use]
    pub fn replace_range(&self, start: u64, end: u64, new_len: u64) -> Self {
        if self.is_empty() {
            return Self::empty();
        }
        if new_len == 0 {
            return self.remove_range(start, end);
        }
        if start == end {
            return self.insert_gap(start, new_len);
        }
        let existing = self.spans_in(start, end);
        let uniform = if existing.is_empty() {
            Some(DocStyle::DEFAULT)
        } else if existing.len() == 1 && existing[0].start == start && existing[0].end == end {
            Some(existing[0].style)
        } else {
            None
        };
        let links = self.links().to_vec();
        let stripped = self.remove_range(start, end);
        let Some(style) = uniform else {
            return stripped.insert_gap(start, new_len);
        };
        if style.is_default() {
            return stripped.insert_gap(start, new_len);
        }
        // Re-insert `[start, start + new_len)` carrying `style` (preserving its link target).
        let mut merged_links = stripped.links().to_vec();
        let mut placed_style = style;
        if let Some(old_id) = style.link_id {
            if let Some(url) = links.get(old_id.get() as usize - 1) {
                let pos = merged_links
                    .iter()
                    .position(|u| u == url)
                    .unwrap_or_else(|| {
                        merged_links.push(url.clone());
                        merged_links.len() - 1
                    });
                placed_style.link_id = NonZeroU32::new((pos + 1) as u32);
            } else {
                placed_style.link_id = None;
            }
        }
        let gapped = stripped.insert_gap(start, new_len);
        let mut runs = gapped.runs().to_vec();
        // Remap `placed_style.link_id` into `gapped`'s link table if needed.
        let mut final_links = gapped.links().to_vec();
        if let Some(old_id) = style.link_id {
            if let Some(url) = links.get(old_id.get() as usize - 1) {
                let pos = final_links
                    .iter()
                    .position(|u| u == url)
                    .unwrap_or_else(|| {
                        final_links.push(url.clone());
                        final_links.len() - 1
                    });
                placed_style.link_id = NonZeroU32::new((pos + 1) as u32);
            }
        }
        runs.push(DocStyleSpan {
            start,
            end: start + new_len,
            style: placed_style,
        });
        Self::normalize(runs, final_links)
    }

    /// Applies `patch` over `[start, end)` within a document of `doc_len` bytes.
    pub fn apply_patch(
        &self,
        doc_len: u64,
        start: u64,
        end: u64,
        patch: &StylePatch,
    ) -> Result<Self, SpineError> {
        if start > end || end > doc_len {
            return Err(SpineError::RangeOutOfRange {
                start,
                end,
                len: doc_len,
            });
        }
        if start == end {
            return Ok(self.clone());
        }
        let links = self.links().to_vec();
        let mut out: Vec<DocStyleSpan> = Vec::new();
        for r in self.runs() {
            if r.end <= start || r.start >= end {
                out.push(*r);
            } else {
                if r.start < start {
                    out.push(DocStyleSpan {
                        start: r.start,
                        end: start,
                        style: r.style,
                    });
                }
                if r.end > end {
                    out.push(DocStyleSpan {
                        start: end,
                        end: r.end,
                        style: r.style,
                    });
                }
            }
        }

        let overlapping = self.spans_in(start, end);
        let mut cursor = start;
        for span in overlapping {
            if cursor < span.start {
                let patched = patch.apply_to(DocStyle::DEFAULT);
                if !patched.is_default() {
                    out.push(DocStyleSpan {
                        start: cursor,
                        end: span.start,
                        style: patched,
                    });
                }
            }
            let patched = patch.apply_to(span.style);
            if !patched.is_default() {
                out.push(DocStyleSpan {
                    start: span.start,
                    end: span.end,
                    style: patched,
                });
            }
            cursor = span.end;
        }
        if cursor < end {
            let patched = patch.apply_to(DocStyle::DEFAULT);
            if !patched.is_default() {
                out.push(DocStyleSpan {
                    start: cursor,
                    end,
                    style: patched,
                });
            }
        }
        Ok(Self::normalize(out, links))
    }

    /// Sets or clears (`url = None` or `Some("")`) the link target over `[start, end)`.
    pub fn set_link(
        &self,
        doc_len: u64,
        start: u64,
        end: u64,
        url: Option<&str>,
    ) -> Result<Self, SpineError> {
        if start > end || end > doc_len {
            return Err(SpineError::RangeOutOfRange {
                start,
                end,
                len: doc_len,
            });
        }
        if start == end {
            return Ok(self.clone());
        }
        let clean = url.filter(|u| !u.is_empty());
        let Some(target) = clean else {
            return self.apply_patch(doc_len, start, end, &StylePatch::new().with_link_id(None));
        };
        let mut links = self.links().to_vec();
        let pos = links.iter().position(|u| u == target).unwrap_or_else(|| {
            links.push(target.to_owned());
            links.len() - 1
        });
        let id = NonZeroU32::new((pos + 1) as u32).expect("pos + 1 >= 1");
        // Seed `links` so `apply_patch` can reference `id` before `normalize` compacts.
        let seeded = Self {
            inner: Some(Arc::new(StyleInner {
                runs: self.runs().to_vec(),
                links,
            })),
        };
        seeded.apply_patch(
            doc_len,
            start,
            end,
            &StylePatch::new().with_link_id(Some(id)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_selection_reports_indeterminate_tristate_and_toggle_collapses_to_empty() {
        let base = StyleMap::empty();
        let styled = base
            .apply_patch(20, 4, 10, &StylePatch::new().with_bold(true))
            .unwrap();
        assert_eq!(styled.runs().len(), 1);

        let inside = styled.toolbar_state(20, 4, 10);
        assert_eq!(inside.bold, TriState::Uniform(true));
        assert_eq!(inside.italic, TriState::Uniform(false));

        let mixed = styled.toolbar_state(20, 2, 12);
        assert_eq!(mixed.bold, TriState::Mixed);
        assert_eq!(mixed.bold.display_or_dash(|b| b.to_string()), "\u{2014}");
        assert_eq!(mixed.family, TriState::Uniform(FontFamily::Sans));

        let cleared = styled
            .apply_patch(20, 0, 20, &StylePatch::new().with_bold(false))
            .unwrap();
        assert!(cleared.is_empty());
    }

    #[test]
    fn link_compaction_and_split_concat_preserve_offsets_and_urls() {
        let map = StyleMap::empty()
            .set_link(30, 5, 15, Some("https://holonomy.invalid/a"))
            .unwrap()
            .set_link(30, 20, 28, Some("https://holonomy.invalid/b"))
            .unwrap();
        assert_eq!(map.links().len(), 2);

        let (left, right) = map.split(12);
        let rejoined = left.concat(12, &right);
        assert_eq!(rejoined, map);

        let unlinked_first = rejoined.set_link(30, 5, 15, None).unwrap();
        assert_eq!(unlinked_first.links(), &["https://holonomy.invalid/b"]);
    }
}
