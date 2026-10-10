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
    /// **Set the zoom to a percentage the row names.**
    ///
    /// **A payload on an otherwise empty enum**, because "zoom to 125%" and "zoom to 150%" are the
    /// same action at different values and listing six variants would make every `match` on `Action`
    /// six arms longer for no gain.
    SetZoom(u32),
    /// **Choose a paragraph style by index**, into [`STYLES`].
    SetStyle(u8),
    /// **Choose a font by index**, into [`FONTS`].
    SetFont(u8),
    /// **Arm or disarm bold for the next character typed.**
    ToggleBold,
    /// **Arm or disarm italic for the next character typed.**
    ToggleItalic,
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
            Action::Save => "save",
            Action::InsertTable => "insert-table",
            Action::InsertImage => "insert-image",
            Action::InsertMath => "insert-math",
            Action::Close => "close",
            Action::SetZoom(_) => "set-zoom",
            Action::SetStyle(_) => "set-style",
            Action::SetFont(_) => "set-font",
            Action::ToggleBold => "toggle-bold",
            Action::ToggleItalic => "toggle-italic",
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
    plain("New"),
    plain("Open"),
    sub("Import", IconId::Folder),
    plain("Export"),
    plain("Print"),
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
    plain("Cut"),
    plain("Copy"),
    accel("Paste", "Ctrl+V"),
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
/// **Every one of them is a name, not a claim that the face is loaded.** The body face is the one the
/// atlas was built with; the rest are here because the reference's dropdown lists a family and a
/// dropdown of one entry is not a dropdown. Choosing one that is not loaded has no effect on the
/// pixels, and `Action::SetFont` changing `font_index` without changing the atlas is stated in the
/// session's handler rather than implied here.
pub const FONTS: &[&str] = &[
    "Inter",
    "Inter Display",
    "Roboto Mono",
    "Georgia",
    "IBM Plex Mono",
];

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
pub const ZOOMS: &[(&str, u32)] = &[
    ("50%", 50),
    ("75%", 75),
    ("100%", 100),
    ("125%", 125),
    ("150%", 150),
    ("200%", 200),
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
        // **Every other tool opens nothing**, and `None` rather than an empty `Vec` so the caller can
        // tell "no dropdown" from "a dropdown with no rows" — which is the difference between a
        // button that does nothing and a button that is broken.
        _ => Vec::new(),
    }
}
