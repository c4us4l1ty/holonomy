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
}

/// Build an item with no icon and no accelerator.
const fn plain(label: &'static str) -> Item {
    Item {
        label,
        icon: None,
        accelerator: None,
        submenu: false,
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
    }
}

/// Build an item with an accelerator.
const fn accel(label: &'static str, acc: &'static str) -> Item {
    Item {
        label,
        icon: None,
        accelerator: Some(acc),
        submenu: false,
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
    Item {
        label: "Close",
        icon: None,
        accelerator: Some("Ctrl+W"),
        submenu: false,
    },
    sub("Extensions", IconId::Sparkle),
];

const EDIT: &[Item] = &[
    accel("Undo", "Ctrl+Z"),
    accel("Redo", "Ctrl+Y"),
    plain("Cut"),
    plain("Copy"),
    accel("Paste", "Ctrl+V"),
    plain("Select all"),
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
    sub("Image", IconId::Image),
    sub("Table", IconId::Table),
    sub("Building blocks", IconId::Doc),
    sub("Smart chips", IconId::Sparkle),
    accel("Link", "Ctrl+K"),
    sub("Drawing", IconId::Pen),
    sub("Chart", IconId::History),
    sub("Bookmark", IconId::Bookmark),
    sub("Symbols", IconId::Sparkle),
    sub("Tab", IconId::Tab),
    accel("Horizontal line", ""),
    sub("Break", IconId::Rule),
    sub("Page elements", IconId::Doc),
];

const FORMAT: &[Item] = &[
    sub("Text", IconId::Bold),
    plain("Paragraph"),
    plain("Lists"),
    accel("Clear formatting", "Ctrl+\\"),
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
    accel("Report a problem", ""),
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

/// The item at `(menu, row)`, if there is one.
///
/// **The hit-test entry point**, and the counterpart of `items_for`: the popup's rows are numbered from
/// the same list the popup was drawn from, so a row that was drawn is a row that can be hit.
#[must_use]
pub fn item_at(menu: usize, row: usize) -> Option<&'static Item> {
    items_for(menu).get(row)
}
