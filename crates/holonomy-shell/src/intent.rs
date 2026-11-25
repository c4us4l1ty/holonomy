//! Intents and effects: the one door a window speaks through (`docs/TO-APP.md` M2).
//!
//! A window should not hold twenty references into the editor loop. It turns each platform event
//! into one [`Intent`], hands it to [`Editor::apply`], and gets back the [`Effect`]s — the things
//! only the window can do — plus, when it wants to draw, a frame.
//!
//! The point of the indirection is that it is **behaviour-preserving**: an intent does exactly what
//! the corresponding method does, so the window cannot invent an editing rule the headless core
//! does not have. `crates/holonomy-shell/tests/intents.rs` asserts that over a randomised stream —
//! same bytes, same geometry, same caret, same selection, intent by intent — and that no intent
//! sequence can lose the caret or put it outside the document.

use crate::{Editor, ShellError};
use holonomy_eye::{Fixed, Point, Shaper, View};
use holonomy_text::{SegmentSink, SegmentSource, StylePatch};

/// Where a caret move asks the caret to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// One cluster to the previous boundary.
    Left,
    /// One cluster to the next boundary.
    Right,
    /// One word to the left (`Ctrl+Left` / `Alt+Left`).
    WordLeft,
    /// One word to the right (`Ctrl+Right` / `Alt+Right`).
    WordRight,
    /// To the line above, keeping the column.
    Up,
    /// To the line below, keeping the column.
    Down,
    /// To the start of the caret's line.
    LineStart,
    /// To the end of the caret's line.
    LineEnd,
    /// To the document's first byte.
    DocumentStart,
    /// To the document's last byte.
    DocumentEnd,
}

/// One thing a user asked for, in the window's vocabulary.
///
/// Every variant names an operation the editor loop already has; none of them carries an editing
/// rule of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Type `text` at the caret, replacing the selection first if there is one.
    Type(String),
    /// Delete the character before the caret, or the selection.
    Backspace,
    /// Delete the character after the caret, or the selection.
    DeleteForward,
    /// Delete the word before the caret, or the selection (`Ctrl+Backspace`).
    DeleteWordBefore,
    /// Delete the word after the caret, or the selection (`Ctrl+Delete`).
    DeleteWordAfter,
    /// Delete the selection, if there is one.
    DeleteSelection,
    /// Select the whole document.
    SelectAll,
    /// Put the caret where a click landed, dropping the selection.
    Click(Point),
    /// Extend the selection to where a drag landed, keeping the anchor.
    DragTo(Point),
    /// Scroll so that `top` is the viewport's first logical unit.
    ScrollTo(Fixed),
    /// Scroll by `delta` logical units.
    ScrollBy(Fixed),
    /// A new wrap width and viewport height, in logical units.
    Resize {
        /// The wrap width.
        width: Fixed,
        /// The visible height.
        viewport: Fixed,
    },
    /// Zoom and DPI — a view transform, never a second geometry.
    SetView(View),
    /// Move the caret, extending the selection when `extend` (shift).
    Move {
        /// Where the caret is asked to go.
        to: Move,
        /// Whether the selection's anchor stays where it was.
        extend: bool,
    },
    /// Scroll the smallest amount that puts the caret on screen; `false` when it already is.
    RevealCaret,
    /// Undo the last step.
    Undo,
    /// Redo the next step.
    Redo,
    /// Write the open bytes through to the sink.
    Flush,
    /// Measure every leaf, one at a time.
    Reflow,
    /// Apply a rich-text [`StylePatch`] across the active selection as one undoable transaction.
    Format(StylePatch),
    /// Set or clear (`None`) a hyperlink URL across the active selection as one undoable step.
    SetLink(Option<String>),
    /// Atomically replace every occurrence of `needle` with `replacement` across the document.
    ReplaceAll {
        /// Plaintext needle to match.
        needle: String,
        /// Replacement text.
        replacement: String,
    },
}

/// Something the window, and only the window, can do in response.
///
/// Effects are not a log: they are what the editor could not finish itself. Clipboard policy, the
/// IME, timers and the paint callback are the window's; everything else the editor already did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Something a window draws has changed: ask for a frame and paint it.
    Redraw,
    /// The caret is at this document byte — for an IME, a caret-blink timer, or accessibility.
    Caret(u64),
    /// The open bytes reached the sink. A window may say so; nothing needs reading back.
    Saved,
}

impl<S: SegmentSink, R: SegmentSource, H: Shaper> Editor<S, R, H> {
    /// Apply `intent`, and report what the window has to do about it.
    ///
    /// This is the only entry point a window needs. It calls the same method a headless caller
    /// would, so there is no second implementation of any editing rule; the effects are the
    /// residue — a repaint, a caret to watch, a write that landed.
    ///
    /// # Errors
    ///
    /// Whatever the underlying operation refuses: a segment the sink would not take, a leaf the
    /// resident set would not read.
    pub fn apply(&mut self, intent: Intent) -> Result<Vec<Effect>, ShellError> {
        let mut effects = Vec::new();
        match intent {
            Intent::Type(text) => {
                self.type_text(&text)?;
                effects.push(Effect::Redraw);
                effects.push(Effect::Caret(self.caret()));
            }
            Intent::Backspace => {
                if self.backspace()? {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::DeleteForward => {
                if self.delete_forward()? {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::DeleteWordBefore => {
                if self.delete_word_before()? {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::DeleteWordAfter => {
                if self.delete_word_after()? {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::DeleteSelection => {
                if self.delete_selection()? {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::SelectAll => {
                self.select_all();
                effects.push(Effect::Redraw);
                effects.push(Effect::Caret(self.caret()));
            }
            Intent::Click(point) => {
                if self.click(point)?.is_some() {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::DragTo(point) => {
                if self.drag_to(point)?.is_some() {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::ScrollTo(top) => {
                let before = self.scroll();
                self.scroll_to(top);
                if self.scroll() != before {
                    effects.push(Effect::Redraw);
                }
            }
            Intent::ScrollBy(delta) => {
                let before = self.scroll();
                self.scroll_by(delta);
                if self.scroll() != before {
                    effects.push(Effect::Redraw);
                }
            }
            Intent::Resize { width, viewport } => {
                let changed = self.width() != width || self.viewport() != viewport;
                self.resize(width, viewport);
                if changed {
                    effects.push(Effect::Redraw);
                }
            }
            Intent::SetView(view) => {
                let changed = self.view() != view;
                self.set_view(view);
                if changed {
                    effects.push(Effect::Redraw);
                }
            }
            Intent::Move { to, extend } => {
                if self.move_caret_to(to, extend)? {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::RevealCaret => {
                if self.reveal_caret()? {
                    effects.push(Effect::Redraw);
                }
            }
            Intent::Undo => {
                if self.undo()? {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::Redo => {
                if self.redo()? {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::Flush => {
                self.flush()?;
                effects.push(Effect::Saved);
            }
            Intent::Reflow => {
                self.reflow()?;
                effects.push(Effect::Redraw);
            }
            Intent::Format(patch) => {
                if self.format_selection(&patch)? {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::SetLink(url) => {
                if self.set_selection_link(url.as_deref())? {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                }
            }
            Intent::ReplaceAll {
                needle,
                replacement,
            } => {
                let count = self.replace_all(&needle, &replacement)?;
                if count > 0 {
                    effects.push(Effect::Redraw);
                    effects.push(Effect::Caret(self.caret()));
                    effects.push(Effect::Saved);
                }
            }
        }
        Ok(effects)
    }
}
