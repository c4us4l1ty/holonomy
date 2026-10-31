//! **The menu bar's contents: the eight headings' items.** Added in Phase 14 part 19.
//!
//! # Why the items are data and not a tree of nodes
//!
//! A menu is three things that have to agree: what it looks like, what it is hit-tested against, and
//! what it does when chosen. **Only the first two belong here.** The third is a command, and a command
//! belongs to the session — so this file describes *items* and the session says what each one means.
//!
//! **The consequence is that a menu item cannot be a `Node`.** A `Node` knows how to draw itself and
//! nothing else; if the items were nodes, choosing one would require the renderer to decide what it
//! means, and the renderer is the wrong crate for that by a wide margin.
//!
//! **So [`items_for`] returns labels, icons and accelerators, and the chrome's `paint_popup` draws
//! them and the session reads the same function to route a click.** One function, two consumers — the
//! same arrangement as [`crate::widgets`], and for the same reason.
//!
//! | what it proves | test |
//! | --- | --- |
//! | every heading has items | [`every_heading_has_items`] |
//! | an out-of-range heading does not panic | [`an_out_of_range_heading_is_an_empty_menu`] |
//! | and every item is renderable | [`every_item_is_renderable`] |

use crate::icons::IconId;

/// **What a menu item *is*, as distinct from what it *does*.** Part 21.
///
/// # Why this exists, and what it replaced
///
/// Part 20 routed a menu choice by **comparing the item's label**:
///
/// ```text
/// Some(match (heading, item.label) { (_, "Undo") => …, (_, "Redo") => …, … })
/// ```
///
/// and said so — *renaming "Undo" to "Revert" silently makes it inert*. That is the worst kind of
/// coupling: **a typo in a display string changes a program's behaviour**, and nothing reports it,
/// because both halves still compile and both still typecheck.
///
/// So an item now carries an [`Action`], a value with no interpretation attached. The renderer draws
/// it and passes it through; **the session decides what `Action::Undo` means.** Neither crate depends
/// on the other's vocabulary — `holonomy-render` does not know a `Command` exists — and the session
/// gets a `match` on an enum, so **adding an action is a compile error in every place that has to
/// handle it** rather than a silent no-op.
///
/// # Why it lives here rather than in `holonomy-input`
///
/// Because it is a *UI vocabulary*, not an input one. `holonomy-input` knows chords and commands and
/// nothing about what a menu is; `holonomy-render` knows what a menu item is and nothing about what a
/// keystroke does. An `Action` is the sentence between them — "this row says Undo" — and it belongs
/// with the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Undo the last edit.
    Undo,
    /// Redo the last undone edit.
    Redo,
    /// Select the whole document.
    SelectAll,
    /// Jump to the first line.
    DocumentStart,
    /// Jump to the last line.
    DocumentEnd,
    /// **Start a new, empty document and make it the one this session paints.**
    ///
    /// **A variant and not a `Command`, for the same reason [`Action::Export`] is one:** the row does
    /// not edit the document, it *replaces* it, and the only function allowed to do that is
    /// `Session::adopt_document` — which lives one crate over and is reached directly from
    /// `Session::apply_action`. Routing it through the keymap would hand the chord table a meaning
    /// (\"discard the document\") that no chord asks for in this build.
    ///
    /// **Added in the font/new-document pass, and it was the one variant the enum was missing.** The row
    /// had been `plain(\"New\")` for its whole life — drawn, and doing nothing — while
    /// `Session::adopt_document` sat one crate over as the supported way to put a document into a
    /// session. Wiring the row is one arm in `apply_action`; this variant is what makes the row compile.
    /// **The tree did not build without it**, because `act(\"New\", Action::New)` and the `name()` arm
    /// below both name a variant that did not exist.
    New,
    /// **Open a container from a path and make it the document this session paints.** Part 45.
    ///
    /// **The developer window only, and the reason is the seal.** The jail's seccomp filter has no
    /// `openat` on a user path, so reaching for one after the seal is `SIGSYS` and exit 137 — a menu
    /// row that kills the process is worse than a row that does nothing, so `Session::apply_action`
    /// refuses it by name when `ChromeState::sealed` is set and counts it as inert instead.
    ///
    /// **A path has to come from somewhere, and there is no file dialog.** The sealed product has
    /// neither the dialog nor the `openat`; the developer window has the `openat` and no dialog. So
    /// the row asks the session to *request* an open, and the window driver — the one place that can
    /// read a path — supplies it. See `Session::take_open_request`.
    Open,
    /// **Open a container from a path and add it to the document list, leaving the open document
    /// where it is.** Part 45.
    ///
    /// **`Open` and `Import` differ in one thing: which document is active afterwards.** Both need a
    /// path from the driver, both are refused under the seal, and both are one `Session` call apart
    /// from `Open` — which is why they are two variants rather than a payload, and why the request
    /// carries the action rather than a boolean.
    Import,
    /// Commit the in-memory rings to the container.
    Save,
    /// Insert a table at the caret.
    InsertTable,
    /// Insert an image at the caret.
    InsertImage,
    /// Insert a formula at the caret.
    InsertMath,
    /// Close the document.
    Close,
    /// **Write the document to every sink opened at boot.**
    ///
    /// **To the sinks, and not to a path the user names.** The jail's measured seccomp allowlist has no
    /// `openat` on a user path, so there is no file dialog; `--export-html out.html` at boot is what
    /// chooses a destination, and this says *now* rather than *at exit*.
    Export,
    /// **Set the zoom to a percentage the row names.**
    ///
    /// **A payload on an otherwise empty enum**, because "zoom to 125%" and "zoom to 150%" are the
    /// same action at different values and listing six variants would make every `match` on `Action`
    /// six arms longer for no gain.
    SetZoom(u32),
    /// **Choose a paragraph style by index**, into [`STYLES`].
    SetStyle(u8),
    /// **Colour the text by index into [`COLOURS`].** Part 29.
    ///
    /// **An index and not a `u32` colour**, for the same reason [`Action::SetZoom`] carries a
    /// percentage rather than a pixel value: *"`zoom to 125%` and `zoom to 150%` are the same action at
    /// different values and listing six variants would make every `match` on `Action` six arms longer
    /// for no gain."* **And an index into a `const` list rather than a `String` name**, which is
    /// part 21's correction to `ChromeState::style_name`.
    SetColour(u8),
    /// **Choose a font by index**, into [`FONTS`].
    SetFont(u8),
    /// **Cut: copy the selection, then delete it.**
    Cut,
    /// **Copy the selection into the editor's yank buffer.**
    Copy,
    /// **Paste the yank buffer at the caret.**
    Paste,
    /// **Arm or disarm bold for the next character typed.**
    ToggleBold,
    /// **Arm or disarm italic for the next character typed.**
    ToggleItalic,
    /// **Switch to the mode at this index in [`MODES`].** Part 44.
    SetMode(u8),
    /// **Run the toolbar tool named by this row.** Part 44.
    ///
    /// **A `Tool` in an `Action`, and the dependency runs the right way.** `holonomy-render` owns
    /// `Tool` and `Action` and the renderer neither knows nor needs to know what a mode does or what
    /// `Tool::Bold` toggles -- it draws the row and passes the value through, and the session is
    /// where `Tool::Bold` becomes a flag. **Without this the overflow menu would need a parallel
    /// `Action` per tool**, which is a table that has to be kept in step with `TOOLBAR` and is wrong
    /// the moment a tool is added.
    SelectTool(crate::widgets::Tool),
}

impl Action {
    /// A short name for a status bar and for a gate's failure message.
    pub const fn name(self) -> &'static str {
        match self {
            Action::Undo => "undo",
            Action::Redo => "redo",
            Action::SelectAll => "select-all",
            Action::DocumentStart => "document-start",
            Action::DocumentEnd => "document-end",
            Action::New => "new-document",
            Action::Open => "open",
            Action::Import => "import",
            Action::Save => "save",
            Action::InsertTable => "insert-table",
            Action::InsertImage => "insert-image",
            Action::InsertMath => "insert-math",
            Action::Close => "close",
            Action::Export => "export",
            Action::Cut => "cut",
            Action::Copy => "copy",
            Action::Paste => "paste",
            Action::SetZoom(_) => "set-zoom",
            Action::SetStyle(_) => "set-style",
            Action::SetFont(_) => "set-font",
            Action::SetColour(_) => "set-colour",
            Action::ToggleBold => "toggle-bold",
            Action::ToggleItalic => "toggle-italic",
            Action::SetMode(_) => "set-mode",
            // **One name for both tools, and not `format!("select-tool-{}", ..)`.** `name` is a
            // `const fn` answering `&'static str`, so it cannot format. **The tool's own name is
            // already on the row** -- `Item::label` is `tool.name()` -- so a gate that wants to know
            // which row it is holding already has it, and a slug that repeated it would be a second
            // spelling to keep in step.
            Action::SelectTool(_) => "select-tool",
        }
    }
}

/// One menu item.
///
/// **`accelerator` is a rendered string, not a keybinding.** A real one would be a `Vec<Key>` parsed
/// from the same table the keymap uses; this one is the text the reference shows, and it is `None`
/// where the reference shows nothing. **Keeping it a string is honest about that** — a fake keybinding
/// that looked real would be the kind of thing that survives into a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// The item's text.
    pub label: &'static str,
    /// Its leading icon, if the reference draws one.
    pub icon: Option<IconId>,
    /// The right-hand accelerator text.
    pub accelerator: Option<&'static str>,
    /// Whether it opens a submenu.
    pub submenu: bool,
    /// **What it is.** `None` for an item that is drawn and has no action behind it yet — which is
    /// most of them, and is the honest state rather than a stub that does nothing while looking real.
    pub action: Option<Action>,
    /// Whether this row shows a tick.
    ///
    /// **A flag on the item rather than a set of chosen indices in the popup**, because a tick is a
    /// property of the *row* — a row that is not chosen cannot be drawn with a tick, and a second
    /// parallel list of indices would be a second thing to keep in step with the rows.
    pub checked: bool,
}

/// Build an item with no icon, no accelerator and no action.
const fn plain(label: &'static str) -> Item {
    Item {
        label,
        icon: None,
        accelerator: None,
        submenu: false,
        action: None,
        checked: false,
    }
}

/// Build an item with an action and nothing else.
///
/// **The one constructor most new menu items will use**, because part 21 is the point: a row that
/// means something says so in the row, not in a `match` on its own name somewhere else.
const fn act(label: &'static str, action: Action) -> Item {
    Item {
        label,
        icon: None,
        accelerator: None,
        submenu: false,
        action: Some(action),
        checked: false,
    }
}

/// Build an item with an action *and* a tick, for a chosen value in a dropdown.
///
/// **`checked` defaults to false**, because a row is *chosen* by the session comparing it to the
/// current value, not by the item declaring itself chosen. **A dropdown that marks the current value
/// is a dropdown whose current value lives in one place**, and `Tool::items` is that place.
const fn chosen(label: &'static str, action: Action, checked: bool) -> Item {
    Item {
        label,
        icon: None,
        accelerator: None,
        submenu: false,
        action: Some(action),
        checked,
    }
}

/// Build an item with an icon.
///
/// **Unused as of part 19, and kept deliberately.** Every item that has a picture in the reference
/// *also* opens a submenu, so it is built by [`sub`] instead. The constructor exists because the
/// "icon but no chevron" item is a real thing -- a toolbar action inside a menu -- and when one is
/// added it should be added by removing this `#[allow]`, not by hand-writing the struct literal a
/// third time. **A dead constructor with a reason beats a struct literal at each new site.**
#[allow(dead_code)]
const fn icon(label: &'static str, icon: IconId) -> Item {
    Item {
        label,
        icon: Some(icon),
        accelerator: None,
        submenu: false,
        action: None,
        checked: false,
    }
}

/// Build an item with a right-pointing chevron, i.e. one that opens a submenu.
///
/// **`submenu` is set rather than left false**, because a chevron with no submenu is the reference's
/// single most confusing affordance and there is no way to tell from the picture alone.
const fn sub(label: &'static str, icon: IconId) -> Item {
    Item {
        label,
        icon: Some(icon),
        accelerator: None,
        submenu: true,
        action: None,
        checked: false,
    }
}

/// Build an item with an accelerator.
const fn accel(label: &'static str, acc: &'static str) -> Item {
    Item {
        label,
        icon: None,
        accelerator: Some(acc),
        submenu: false,
        action: None,
        checked: false,
    }
}

/// Every heading's items, indexed the same way as [`MENUS`](crate::chrome::MENUS).
///
/// **The lengths are asserted against `MENUS` by the gate.** Two `const` arrays that must agree is a
/// place a rename can silently break, and the failure would be `index out of bounds` at paint time
/// rather than a compile error — which is why [`items_for`] takes a `usize` and returns an empty slice
/// rather than indexing.
const FILE: &[Item] = &[
    // **Wired as of the font/new-document pass.** This was `plain("New")` — a row that drew and
    // did nothing — while `Session::adopt_document` sat one crate over as the supported way to
    // put a document into a session. The row now asks for exactly that.
    act("New", Action::New),
    // **`Open` and `Import`, and both were rows that drew and did nothing.**
    //
    // `Open` was `plain("Open")` — a row with no action behind it at all — and `Import` was a
    // `sub(..)` with a chevron and no submenu to open, which is the reference's single most
    // confusing affordance. **Both now carry an action, and both are refused under the seal**
    // (`Session::apply_action` counts them as inert rather than letting `openat` kill the process).
    // What they do in the developer window is [`Session::open_path`](crate::session::Session::open_path)
    // — reached through the driver, which is the only place a path can come from.
    //
    // **The menu's row count does not move**, so the wiring gate's total is unchanged and only the
    // wired/unwired split does: two rows leave the unwired list.
    act("Open", Action::Open),
    act("Import", Action::Import),
    // **The save, and it was the one row the File menu was missing.**
    //
    // `Action::Save` has existed since the `Action` enum replaced a label match (part 21) and
    // `action_command` has always known what it means — but **no row carried it**, so the variant was
    // reachable from nowhere. `Ctrl+S` reached the commit through the keymap and the title bar's Cloud
    // button reached it through `title_command`, and a keyboard-less user had no route at all.
    //
    // `item_count_matches_the_file_menu` asserts this list's length, so the row is counted rather than
    // merely present.
    Item {
        accelerator: Some("Ctrl+S"),
        ..act("Save", Action::Save)
    },
    // **Export, and the row that was drawn and did nothing.**
    //
    // `Session::export` has been implemented and gated since Phase 8 and the only routes to it were
    // the `--export-html` / `--export-pdf` command-line flags and the end of the session — so a user
    // clicking **File > Export** got a menu that opened and a row that closed it again. **`Print` is
    // the contrast, and it is worth naming correctly:** it was not removed for want of a backend --
    // it was wired to this same `Action::Export`, which is the one thing a print button does. **The
    // backend is here in both cases**, so what was missing was the row reaching it: a wiring defect
    // rather than a missing feature.
    //
    // **It writes to the sinks opened at boot, not to a path the user picks.** The jail's measured
    // seccomp allowlist has no `openat` on a user path, so an interactive file dialog is not
    // available; `--export-html out.html` at boot is what chooses the destination.
    act("Export", Action::Export),
    accel("Page setup", "Ctrl+P"),
    act("Close", Action::Close),
    sub("Extensions", IconId::Sparkle),
];

const EDIT: &[Item] = &[
    Item {
        accelerator: Some("Ctrl+Z"),
        ..act("Undo", Action::Undo)
    },
    Item {
        accelerator: Some("Ctrl+Y"),
        ..act("Redo", Action::Redo)
    },
    // **Cut, Copy and Paste, which were three rows that opened a menu and closed it again.**
    //
    // The backend is `Editor`'s yank buffer and it is *internal*: there is no X selection on the
    // sealed path, no `openat` to spool to a file and no network for a remote clipboard, so this is
    // the only meaning `Copy` can have that is reachable from every build. `release_artifact.rs`
    // asserts the sealed binary contains no X11 client, which is what makes that a fact rather than
    // a preference.
    Item {
        accelerator: Some("Ctrl+X"),
        ..act("Cut", Action::Cut)
    },
    Item {
        accelerator: Some("Ctrl+C"),
        ..act("Copy", Action::Copy)
    },
    Item {
        accelerator: Some("Ctrl+V"),
        ..act("Paste", Action::Paste)
    },
    act("Select all", Action::SelectAll),
    accel("Find", "Ctrl+F"),
    accel("Spelling and grammar", "Ctrl+X"),
];

const VIEW: &[Item] = &[
    sub("Mode", IconId::Pen),
    plain("Ruler"),
    sub("Zoom", IconId::Search),
    sub("Appearance", IconId::Sparkle),
    sub("Layout", IconId::Table),
    sub("Toolbars", IconId::Rule),
];

const INSERT: &[Item] = &[
    Item {
        action: Some(Action::InsertImage),
        ..sub("Image", IconId::Image)
    },
    Item {
        action: Some(Action::InsertTable),
        ..sub("Table", IconId::Table)
    },
    sub("Building blocks", IconId::Doc),
    sub("Smart chips", IconId::Sparkle),
    accel("Link", "Ctrl+K"),
    sub("Drawing", IconId::Pen),
    sub("Chart", IconId::History),
    sub("Bookmark", IconId::Bookmark),
    sub("Symbols", IconId::Sparkle),
    // **The row part 24 gave the keyboard and part 26 gave the menu.** `Action::InsertMath` routed
    // through `Ctrl-M` and through the `InsertMath` hotkey command, and *nothing else* reached it:
    // no menu row and no popup row carried it, so a user who did not know the accelerator had no way
    // to insert an equation at all. That is the `Action::Save` shape -- a capability the menus had a
    // home for and did not occupy -- so it gets the same treatment: one `act` row, which is why this
    // is an `act` and not a `plain`. **The label is the reference's** (`Insert > Equation`), and the
    // position is the reference's too, directly under `Symbols`, because the two are the same kind of
    // entry and a user looking for one finds the other here.
    //
    // **`DocumentStart` and `DocumentEnd` are deliberately not given rows.** They are the other two
    // `Action` variants that appeared only in their own `name()` arm, and they are *not* the same
    // shape: the capability they carry is "place the caret at the first or last line", and a
    // pointer-only user reaches that capability every time they click on the page. `InsertMath`
    // inserts something that otherwise does not exist; these two move the caret somewhere the user can
    // already reach by clicking. The reference has no menu row for either, and adding one would be
    // inventing a navigation menu the product does not have rather than repairing a defect. **A row
    // that adds a menu the reference lacks is not a wiring fix.** They stay keyboard-only, and this
    // comment is where that decision is recorded so the next reader does not "fix" it by hand.
    act("Equation", Action::InsertMath),
    sub("Tab", IconId::Tab),
    plain("Horizontal line"),
    sub("Break", IconId::Rule),
    sub("Page elements", IconId::Doc),
];

const FORMAT: &[Item] = &[
    sub("Text", IconId::Bold),
    act("Bold", Action::ToggleBold),
    act("Italic", Action::ToggleItalic),
    plain("Paragraph"),
    plain("Lists"),
    Item {
        accelerator: Some("Ctrl+\\"),
        ..plain("Clear formatting")
    },
    plain("Theme"),
    sub("Align", IconId::Rule),
];

const TOOLS: &[Item] = &[
    accel("Spelling and grammar", "Ctrl+X"),
    accel("Word count", "Ctrl+Shift+G"),
    plain("Voice typing"),
    plain("Translate"),
    sub("Extension", IconId::Sparkle),
];

const EXTENSIONS: &[Item] = &[
    sub("Add-ons", IconId::Sparkle),
    plain("Available for you"),
    plain("Manage add-ons"),
    plain("Updates"),
];

const HELP: &[Item] = &[
    sub("Search the menus", IconId::Search),
    plain("Keyboard shortcuts"),
    plain("Help documentation"),
    plain("Report a problem"),
];

/// Every menu, in [`MENUS`](crate::chrome::MENUS) order.
///
/// **The one place the eight are kept in step**, and `every_heading_has_items` is the assertion that
/// checks it.
pub const MENUS: &[&[Item]] = &[FILE, EDIT, VIEW, INSERT, FORMAT, TOOLS, EXTENSIONS, HELP];

/// The items for heading `index`, or an empty slice.
///
/// **A slice rather than a `Result` or a panic**, because a heading index out of range is a paint-time
/// mistake in a caller, not a condition a user can reach: `open_menu` is set from `hit`, which only
/// ever returns an index that came from `menu_boxes`. **Returning empty makes the mistake a blank
/// popup rather than a crash on the paint path**, which is the right way round: a crash inside a paint
/// takes the session with it.
#[must_use]
pub fn items_for(index: usize) -> &'static [Item] {
    MENUS.get(index).copied().unwrap_or(&[])
}

/// The paragraph styles the style dropdown offers, in the order it shows them.
///
/// **`&'static [&'static str]` rather than a `Vec`, so `ChromeState::style_name` is a borrow rather
/// than an allocation.** Part 19 stored `style_name: String` and formatted a `String` for the toolbar
/// on *every paint*; an index into this list is a `u8` in the state and a borrow at the point of use.
/// See `ChromeState::style_name`.
pub const STYLES: &[&str] = &[
    "Normal text",
    "Title",
    "Heading 1",
    "Heading 2",
    "Heading 3",
    "Quote",
    "Caption",
];

/// The fonts the font dropdown offers, in the order it shows them.
///
/// **Every entry is a family the atlas actually packs, and choosing one changes the page.** This
/// list used to carry five names of which four were never loaded — the dropdown was scenery, and
/// `Action::SetFont` recorded an index that moved nothing. A control that looks like Google Docs'
/// font menu and does nothing is worse than a shorter menu that works, so the list is now exactly
/// the two body-capable families in the payload: Inter (regular, bold and italic faces) and
/// JetBrains Mono (regular). The session's `doc_style` is where the index becomes a face, and its
/// `SetFont` arm repaints the page when the face changes.
///
/// **Growing this list is an atlas question, not a list question.** A new family means a new face
/// in the payload (`tools/build_font_payload.py`), a new row in the metric tables, and binary-size
/// budget — which is why the four decorative names were removed rather than wired.
pub const FONTS: &[&str] = &["Inter", "JetBrains Mono"];

/// The row of [`FONTS`] that selects JetBrains Mono, the one alternative body face.
///
/// **A named constant because two places must agree on it**: `dropdown_items` builds the row from
/// the list's order, and the session's `doc_style` maps the chosen index to the Monospace atlas
/// face. A bare `1` in the session would be a second spelling of this list's order, wrong the day
/// a family is inserted above it.
pub const MONO_FONT_INDEX: usize = 1;

/// The zoom levels the zoom dropdown offers: **a label and a value together**.
///
/// **The pair is the point.** The first version had `ZOOMS: &[u16]` and formatted `"{}%"` at the
/// point of use — which meant the *row's text* was computed while the *row's meaning* came from a
/// different array, and the two could be out of step. Here one entry carries both, so "125" and
/// "125%" cannot disagree, and there is no formatting on the paint path at all.
///
/// **`&'static str` labels rather than a `String`,** which is why `Item::label` can stay
/// `&'static str` and every menu can stay a `const`. A zoom dropdown with a fixed list has fixed
/// labels; a dropdown whose labels were computed would need an owned `Item`, and that is a much
/// larger change for no gain.
///
/// **CORRECTION, part 23: `u32` and not the `u16` part 21 chose.** Part 21's note argued that `u16` is
/// "the smallest honest width for a percentage with a thousands' digit" — which is true, and beside
/// the point. **`ChromeState::zoom_percent` is a `u32` and `--zoom` is a `u32`**, so `u16` was the third
/// spelling of one concept, and it cost a real conversion: part 23 made `set_zoom` public so the
/// `--zoom` sites could reach it, and the compiler rejected `set_zoom(args.zoom)` because the types
/// disagreed. **A width chosen for economy and then paid for at every boundary is not a saving.**
///
/// # CORRECTION, part 43: five rows became two, and the reason is an atlas ceiling
///
/// **The list was 50 / 75 / 100 / 125 / 150 and every one of them did nothing to the pixels.**
/// `Painter` already had a `size_index` and an `atlas_size()` that reads `atlas.sizes()[index]` --
/// **the machinery was built and unused** -- and the thing missing was a second size in the atlas.
///
/// **There is room for exactly two.** `AtlasBuilder::new` refuses a third:
///
/// ```text
/// 3 sizes requested, but the atlas geometry is budgeted for at most 2: each extra size adds
/// 14,080 bytes of metric table, which would push the atlas-plus-table pair over the 512 KiB ceiling
/// ```
///
/// **and that ceiling is the PRD's, not this crate's.** Measured: `[16]` uses 236,690 of 458,752;
/// `[16, 20]` uses 378,850; a third does not fit. **So the honest dropdown has the rows the atlas
/// can back**, and 125 % is the second one -- 20 ppem against 16 is exactly 125 %.
///
/// **Two rows where the reference has five is a visible deviation and it is the right one.** Five
/// rows of which three change nothing is the reference's lie reproduced; two rows that both change
/// the pixels is a control that works. `a_zoom_row_that_is_not_in_the_table_is_refused` is the gate
/// that says what happens to a percentage outside the table.
pub const ZOOMS: &[(&str, u32)] = &[("100%", 100), ("125%", 125)];

/// **The text colours the toolbar offers, and the values [`SpanMap::style_range`] takes.**
///
/// ## Why a `const` list and not a table in the renderer
///
/// **Part 21's reason, unchanged.** The chrome needs the index to tick the current choice and the
/// label to draw it; the session needs the index to apply it and the value to write into the span.
/// **One list, two readers, and an index that cannot disagree with a name** — which is what part 21
/// got wrong when `ChromeState` carried a `style_name: String`.
///
/// ## Why six
///
/// **Six is what a swatch grid fits in one column without scrolling, and one is not enough to be a
/// choice.** The full palette of a word processor is a grid of dozens; this is a dropdown, so it is a
/// list, and a list of dozens is a scrollbar -- which this popup does not have. **Six is a number
/// chosen for the popup, and `COLOURS` is where to change it.**
///
/// ## Why `0x00RRGGBB` and not `0xAARRGGBB`
///
/// **Because that is what the span field is.** `TextIntervalSpan::color_rgb`'s doc says so, and the
/// `|` of an alpha byte happens in exactly one function -- `Session::span_ink` -- rather than being
/// something each reader has to remember.
pub const COLOURS: &[(&str, u32)] = &[
    ("Crimson", 0x00D1_2B45),
    // **`0x008C_C8C0` and not `0x00B0_4A8A`, which this said "Teal" for two days.** `0xB0_4A_8A` has
    // *red* as its largest channel: it is a mauve. **A colour whose name is wrong is a colour nobody
    // can find**, and the palette is the only place the user sees these values, so it is worth being
    // right. The gate's own hue test found it -- it counted zero teal pixels next to a palette entry
    // named teal, and the second entry's value was the reason.
    ("Teal", 0x008C_C8C0),
    ("Forest", 0x00B0_4A3C),
    // **`AA_RR_GG_BB` in groups of two, like every other colour in the project.** These three were
    // written `00C0 | 7A | 1E`, which is the same number and a different shape.
    ("Amber", 0x00_C0_7A_1E),
    ("Violet", 0x00_75_4A_C0),
    ("Slate", 0x00_5A_5A_66),
];

/// The item at `(menu, row)`, if there is one.
///
/// **The hit-test entry point**, and the counterpart of `items_for`: the popup's rows are numbered from
/// the same list the popup was drawn from, so a row that was drawn is a row that can be hit.
#[must_use]
pub fn item_at(menu: usize, row: usize) -> Option<&'static Item> {
    items_for(menu).get(row)
}

/// The dropdown a labelled toolbar control opens, or `None` for one that opens nothing.
///
/// **A `Vec`, because the rows depend on `state`.** The zoom dropdown ticks the current level and the
/// style dropdown ticks the current style, so its rows are a function of what is current — which
/// makes them data, not constants. **One allocation per paint, and only while a dropdown is open**,
/// which is the same trade `widgets::popup`'s rows already make and for the same reason: a closed
/// dropdown draws nothing.
///
/// **No label is ever formatted.** Every row's text is a `&'static str`, so the only thing this
/// allocates is the `Vec` itself — one, for the rows — and part 19's three-per-paint `String`s are
/// gone with the `style_name` and `font_name` fields this made possible to delete.
pub fn dropdown_items(tool: crate::widgets::Tool, state: &crate::chrome::ChromeState) -> Vec<Item> {
    use crate::widgets::Tool;
    match tool {
        Tool::Zoom => ZOOMS
            .iter()
            .map(|&(label, z)| chosen(label, Action::SetZoom(z), z == state.zoom_percent))
            .collect(),
        Tool::Style => STYLES
            .iter()
            .enumerate()
            .map(|(i, name)| chosen(name, Action::SetStyle(i as u8), i == state.style_index))
            .collect(),
        Tool::Font => FONTS
            .iter()
            .enumerate()
            .map(|(i, name)| chosen(name, Action::SetFont(i as u8), i == state.font_index))
            .collect(),
        // **Part 29.** The fourth dropdown, and the first one whose rows carry a value the *document*
        // holds rather than a value the *chrome* holds -- which is the difference between a tick that
        // means "this is the current font" and one that means "this is the colour you last chose".
        Tool::TextColour => COLOURS
            .iter()
            .enumerate()
            .map(|(i, (name, _))| chosen(name, Action::SetColour(i as u8), i == state.colour_index))
            .collect(),
        // **Part 44: `Mode` is a real dropdown, and both of its rows are real.** `MODES` is one row
        // per mode the session can actually be in; see `ViewMode`.
        Tool::Mode => MODES
            .iter()
            .enumerate()
            .map(|(i, (label, _))| chosen(label, Action::SetMode(i as u8), i == usize::from(state.mode_index)))
            .collect(),
        // **Part 44: `Overflow` lists the tools that did not fit.** The rows carry the tool's *name*
        // and `Action::SelectTool` is what runs one -- a different shape from `Action::SetZoom`, which
        // is a statement about the chrome's own state.
        Tool::Overflow => state
            .overflow_tools
            .iter()
            .map(|&tool| Item {
                label: tool.name(),
                icon: tool.icon(),
                accelerator: None,
                submenu: false,
                action: Some(Action::SelectTool(tool)),
                checked: false,
            })
            .collect(),
        // **Every other tool opens nothing**, and `None` rather than an empty `Vec` so the caller can
        // tell "no dropdown" from "a dropdown with no rows" — which is the difference between a
        // button that does nothing and a button that is broken.
        _ => Vec::new(),
    }
}

/// The modes the mode switcher offers, and what each one means.
///
/// **Two, and both are real.** `Editing` is the ordinary mode; `Viewing` is read-only — every mutator
/// is refused and the caret still moves, which is what "viewing" means in every editor and is
/// implementable with what this model already has.
///
/// **There is no `Suggesting`, and that is the interesting omission.** The reference offers it and a
/// wireframe can offer it for free. **A suggestion mode needs a suggestion store, a provenance flag
/// per span and a reviewer identity**, and none of those exist; a `Suggesting` row that behaved like
/// `Editing` under a different label would be the most dishonest thing on the toolbar — worse than
/// the inert button it replaces, because it would *look* like it was tracking other people's edits.
pub const MODES: &[(&str, ViewMode)] = &[("Editing", ViewMode::Editing), ("Viewing", ViewMode::Viewing)];

/// What a mode is, as far as the renderer is concerned.
///
/// **A copy of the session's vocabulary and not the session's type**, for the same reason `Action`
/// is: `holonomy-render` must not depend on `holonomy`, and the renderer only needs to put a name
/// and a tick on a row. **The session is the only thing that decides what a mode *does*.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Type, delete, format.
    Editing,
    /// Move the caret, select, scroll. **Refuse every edit.**
    Viewing,
}
