//! Opt-in GPUI editor: native vector glyphs, with the existing encrypted session underneath.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod design;

use gpui::{
    canvas, div, fill, point, prelude::*, px, rgb, size, App, Application, Bounds, Context,
    FocusHandle, KeyDownEvent, MouseButton, PathBuilder, Pixels, Window, WindowBounds,
    WindowOptions,
};
use holonomy_native::{BoxRect, SecretInput, Session, VectorFrame};
use holonomy_shell::{Intent, Move};
use holonomy_text::{Alignment, Heading, ListKind, StylePatch, TriState};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;
use zeroize::Zeroize;

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Home,
    Unlock,
    Editor,
    Find,
    Bookmark,
}

struct UnlockWorker(Arc<AtomicBool>);

impl Drop for UnlockWorker {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct PendingUnlock {
    receiver: mpsc::Receiver<std::result::Result<holonomy_gui::store::PreparedDocument, String>>,
    timeout: Option<Duration>,
}

struct NativeHome {
    focus: FocusHandle,
    fonts: holonomy_eye::FontSet,
    dark: bool,
    screen: Screen,
    create: bool,
    path: String,
    passphrase: SecretInput,
    preedit: SecretInput,
    editing_path: bool,
    query: SecretInput,
    search_index: u64,
    session: Option<Session>,
    pending_unlock: Option<PendingUnlock>,
    unlock_worker_active: Arc<AtomicBool>,
    status: String,
    bounds: Bounds<Pixels>,
    dragging: bool,
    show_history: bool,
}

fn button(id: &'static str, label: impl Into<gpui::SharedString>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .h(px(design::TOOL_HEIGHT))
        .px_2()
        .rounded_md()
        .text_size(px(12.))
        .whitespace_nowrap()
        .hover(|style| style.bg(gpui::rgba(0x88888818)))
        .child(label.into())
}

fn field(
    id: &'static str,
    label: String,
    active: bool,
    palette: design::Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .cursor_text()
        .flex()
        .items_center()
        .w_full()
        .min_w_0()
        .h(px(design::CONTROL_HEIGHT + 8.))
        .flex_shrink_0()
        .px_3()
        .rounded_lg()
        .border_1()
        .border_color(rgb(if active {
            palette.muted
        } else {
            palette.border
        }))
        .bg(rgb(palette.page))
        .text_size(px(14.))
        .overflow_hidden()
        .whitespace_nowrap()
        .child(label)
}

impl NativeHome {
    fn new(cx: &mut Context<Self>) -> Self {
        let path = std::env::args().nth(1).unwrap_or_default();
        let screen = if path.is_empty() {
            Screen::Home
        } else {
            Screen::Unlock
        };
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(50))
                .await;
            if this
                .update(cx, |this, cx| {
                    this.poll_unlock(cx);
                    if this.session.as_ref().is_some_and(Session::tripped) {
                        this.wipe(cx);
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
        Self {
            focus: cx.focus_handle(),
            fonts: holonomy_eye::FontSet::embedded(17).expect("embedded fonts"),
            dark: false,
            screen,
            create: false,
            path,
            passphrase: SecretInput::default(),
            preedit: SecretInput::default(),
            editing_path: true,
            query: SecretInput::default(),
            search_index: 0,
            session: None,
            pending_unlock: None,
            unlock_worker_active: Arc::new(AtomicBool::new(false)),
            status: String::new(),
            bounds: Bounds::default(),
            dragging: false,
            show_history: false,
        }
    }

    fn begin_document(&mut self, create: bool) {
        if !self.composition_ready() {
            return;
        }
        if self.session.is_some() {
            self.home();
            if self.session.is_some() {
                return;
            }
        }
        self.create = create;
        if create {
            self.path.clear();
        }
        self.passphrase.zeroize();
        self.editing_path = true;
        self.screen = Screen::Unlock;
        self.status.clear();
    }

    fn report(&mut self, result: holonomy_native::Result<()>, success: &str) {
        self.status = match result {
            Ok(()) => success.into(),
            Err(error) => error.to_string(),
        };
    }

    // A composition belongs to the current field, selection and document view. Never route
    // its later platform commit into a different destination or move its insertion point.
    fn composition_ready(&mut self) -> bool {
        if self.pending_unlock.is_some() {
            self.status =
                "Unlocking securely… You can cancel while the derivation finishes.".into();
            return false;
        }
        composition_ready(&self.preedit, &mut self.status)
    }

    fn apply(&mut self, intent: Intent) {
        if !self.composition_ready() {
            return;
        }
        if let Some(session) = self.session.as_mut() {
            let navigation = matches!(intent, Intent::RevealCaret);
            let result = session.apply(intent);
            if !navigation || result.is_err() {
                self.report(result, "");
            }
        }
    }

    fn save(&mut self) -> bool {
        if self.pending_unlock.is_some() {
            self.status = "Cancel the in-progress unlock before closing.".into();
            return false;
        }
        if let Err(error) = ensure_no_composition(&self.preedit) {
            self.status = error.to_string();
            return false;
        }
        let result = self
            .session
            .as_mut()
            .map_or(Ok(()), |session| session.prepare_close());
        let success = result.is_ok();
        self.report(result, "Saved to an authenticated witness.");
        success
    }

    fn home(&mut self) {
        if !self.save() {
            return;
        }
        self.session.take();
        self.preedit.zeroize();
        self.passphrase.zeroize();
        self.query.zeroize();
        self.show_history = false;
        self.screen = Screen::Home;
    }

    fn unlock(&mut self) {
        if self.pending_unlock.is_some() {
            return;
        }
        if let Err(error) = ensure_no_composition(&self.preedit) {
            self.status = error.to_string();
            return;
        }
        let timeout = match std::env::var("HOLONOMY_DEADMAN_MS") {
            Ok(value) => holonomy_native::parse_deadman_timeout(Some(&value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(error) => Err(error.into()),
        };
        let timeout = match timeout {
            Ok(timeout) => timeout,
            Err(error) => {
                self.status = error.to_string();
                return;
            }
        };
        if self.path.trim().is_empty() {
            self.status = "Choose a document path.".into();
            return;
        }
        if self.unlock_worker_active.swap(true, Ordering::AcqRel) {
            self.status =
                "The cancelled derivation is still clearing. Please wait a moment.".into();
            return;
        }
        let worker = UnlockWorker(Arc::clone(&self.unlock_worker_active));
        // Move, never copy, the bounded passphrase. No locked key owners or editor Rc state
        // cross threads. Dropping the receiver on Cancel makes a late result zeroize on drop.
        let passphrase = std::mem::take(&mut self.passphrase);
        let path = self.path.clone();
        let create = self.create;
        let (sender, receiver) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("holonomy-unlock".into())
            .spawn(move || {
                let _worker = worker;
                let result = holonomy_native::prepare_document(
                    Path::new(&path),
                    passphrase.as_bytes(),
                    create,
                );
                drop(passphrase);
                let _ = sender.send(result);
            }) {
            Ok(_) => {
                self.pending_unlock = Some(PendingUnlock { receiver, timeout });
                self.status = if create {
                    "Preparing encryption…"
                } else {
                    "Unlocking securely…"
                }
                .into();
            }
            Err(error) => self.status = format!("Could not start unlock worker: {error}"),
        }
    }

    fn poll_unlock(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = &self.pending_unlock else {
            return;
        };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Unlock worker stopped. Please retry.".into())
            }
        };
        let timeout = self
            .pending_unlock
            .take()
            .and_then(|pending| pending.timeout);
        let result = result.and_then(|prepared| {
            let document = prepared.finish().map_err(|error| error.to_string())?;
            let mut session = Session::from_document(Path::new(&self.path), document)
                .map_err(|error| error.to_string())?;
            if let Some(timeout) = timeout {
                session
                    .set_deadman(timeout)
                    .map_err(|error| error.to_string())?;
            }
            Ok(session)
        });
        match result {
            Ok(session) => {
                self.session = Some(session);
                self.screen = Screen::Editor;
                self.status.clear();
            }
            Err(error) => {
                self.status = error;
                self.editing_path = false;
            }
        }
        cx.notify();
    }

    fn cancel_unlock(&mut self) {
        self.pending_unlock.take();
        self.preedit.zeroize();
        self.passphrase.zeroize();
        self.screen = Screen::Home;
        self.status =
            "Cancelled. Any in-flight derivation clears its temporary secret when it finishes."
                .into();
    }

    fn format(&mut self, kind: u8) {
        if let Some(session) = &self.session {
            let toolbar = session.toolbar();
            let mut patch = StylePatch::new();
            match kind {
                0 => patch.bold = Some(!matches!(toolbar.bold, TriState::Uniform(true))),
                1 => patch.italic = Some(!matches!(toolbar.italic, TriState::Uniform(true))),
                2 => patch.underline = Some(!matches!(toolbar.underline, TriState::Uniform(true))),
                3 => patch.strikethrough = Some(!matches!(toolbar.strikethrough, TriState::Uniform(true))),
                4 => patch.code_span = Some(!matches!(toolbar.code_span, TriState::Uniform(true))),
                _ => return,
            }
            self.apply(Intent::Format(patch));
        }
    }

    fn find(&mut self) {
        if let Some(session) = &mut self.session {
            self.status = match session.find(&self.query, self.search_index) {
                Ok(count) => {
                    self.search_index = if count == 0 {
                        0
                    } else {
                        (self.search_index + 1) % count
                    };
                    format!("{count} matches")
                }
                Err(error) => error.to_string(),
            };
        }
    }

    fn wipe(&mut self, cx: &mut Context<Self>) {
        let cancelled_worker = self.pending_unlock.take().is_some()
            || self.unlock_worker_active.load(Ordering::Acquire);
        let verified = self.session.as_mut().map_or(true, Session::wipe);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new()));
        self.session.take();
        self.preedit.zeroize();
        self.passphrase.zeroize();
        self.query.zeroize();
        self.screen = Screen::Home;
        self.status = if cancelled_worker {
            "Unlock cancelled. In-flight derivation cleanup completes when its worker returns; no document was exposed."
        } else if verified {
            "Resident text/keys wiped; clipboard cleared. GPU memory and clipboard history are not qualified."
        } else {
            "Key wipe could not be confirmed."
        }
        .into();
    }

    fn clipboard(&mut self, cut: bool, cx: &mut Context<Self>) {
        if !self.composition_ready() {
            return;
        }
        if let Some(session) = &mut self.session {
            match session.selected_text() {
                Ok(text) => {
                    if !text.is_empty() {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string()));
                        self.status =
                            "PLAINTEXT EGRESS: selection copied to the system clipboard.".into();
                        if cut {
                            self.apply(Intent::DeleteSelection);
                        }
                    }
                }
                Err(error) => self.status = error.to_string(),
            }
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;
        let command = if cfg!(target_os = "macos") {
            mods.platform
        } else {
            mods.control
        };
        if command && mods.shift && key == "t" {
            self.dark = !self.dark;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if command && mods.shift && key == "escape" {
            self.wipe(cx);
            cx.notify();
            return;
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.activity().is_err())
        {
            self.wipe(cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.pending_unlock.is_some() {
            if key == "escape" {
                self.cancel_unlock();
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if command && key == "q" {
            if self.save() {
                window.remove_window();
            }
            cx.notify();
            return;
        }
        if !self.preedit.is_empty() {
            if command {
                self.composition_ready();
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if command && (key == "n" || key == "o") {
            self.begin_document(key == "n");
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if form_paste(self.screen, command, key) {
            if let Some(item) = cx.read_from_clipboard() {
                if let Some(mut text) = item.text() {
                    self.commit_text(&text);
                    text.zeroize();
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.screen == Screen::Unlock {
            match key {
                "escape" => {
                    self.preedit.zeroize();
                    self.passphrase.zeroize();
                    self.screen = Screen::Home;
                }
                "tab" => self.editing_path = !self.editing_path,
                "enter" => self.unlock(),
                "backspace" => {
                    if self.editing_path {
                        self.path.pop();
                    } else {
                        self.passphrase.pop();
                    }
                }
                _ => {}
            }
        } else if matches!(self.screen, Screen::Find | Screen::Bookmark) {
            match key {
                "escape" => {
                    self.query.zeroize();
                    self.screen = Screen::Editor;
                }
                "enter" => {
                    if self.screen == Screen::Find {
                        self.find();
                    } else if let Some(session) = &mut self.session {
                        let result = session.bookmark(&self.query);
                        self.report(result, "Bookmark added.");
                        self.query.zeroize();
                        self.screen = Screen::Editor;
                    }
                }
                "backspace" => {
                    self.query.pop();
                    self.search_index = 0;
                }
                _ => {}
            }
        } else if self.screen == Screen::Editor {
            if command {
                match key {
                    "left" => self.apply(Intent::Move {
                        to: Move::WordLeft,
                        extend: mods.shift,
                    }),
                    "right" => self.apply(Intent::Move {
                        to: Move::WordRight,
                        extend: mods.shift,
                    }),
                    "home" => self.apply(Intent::Move {
                        to: Move::DocumentStart,
                        extend: mods.shift,
                    }),
                    "end" => self.apply(Intent::Move {
                        to: Move::DocumentEnd,
                        extend: mods.shift,
                    }),
                    "backspace" => self.apply(Intent::DeleteWordBefore),
                    "delete" => self.apply(Intent::DeleteWordAfter),
                    "s" => {
                        self.save();
                    }
                    "z" => self.apply(if mods.shift {
                        Intent::Redo
                    } else {
                        Intent::Undo
                    }),
                    "y" => self.apply(Intent::Redo),
                    "a" => self.apply(Intent::SelectAll),
                    "b" => self.format(0),
                    "i" => self.format(1),
                    "u" => self.format(2),
                    "c" => self.clipboard(false, cx),
                    "x" => self.clipboard(true, cx),
                    "v" => {
                        if let Some(item) = cx.read_from_clipboard() {
                            if let Some(text) = item.text() {
                                if text.len() <= 1024 * 1024 {
                                    self.apply(Intent::Type(text));
                                } else {
                                    self.status = "Paste exceeds the 1 MiB limit.".into();
                                }
                            }
                        }
                    }
                    "f" => {
                        self.query.zeroize();
                        self.search_index = 0;
                        self.screen = Screen::Find;
                    }
                    "h" => self.show_history = !self.show_history,
                    _ => {}
                }
            } else {
                let movement = match key {
                    "left" => Some(Move::Left),
                    "right" => Some(Move::Right),
                    "up" => Some(Move::Up),
                    "down" => Some(Move::Down),
                    "home" => Some(Move::LineStart),
                    "end" => Some(Move::LineEnd),
                    _ => None,
                };
                if let Some(to) = movement {
                    self.apply(Intent::Move {
                        to,
                        extend: mods.shift,
                    });
                } else {
                    match key {
                        "backspace" => self.apply(Intent::Backspace),
                        "delete" => self.apply(Intent::DeleteForward),
                        "enter" => self.apply(Intent::Type("\n".into())),
                        "tab" => self.apply(Intent::Type("\t".into())),
                        _ => {}
                    }
                }
                self.apply(Intent::RevealCaret);
            }
        }
        if command
            || matches!(
                key,
                "escape"
                    | "tab"
                    | "enter"
                    | "backspace"
                    | "delete"
                    | "left"
                    | "right"
                    | "up"
                    | "down"
                    | "home"
                    | "end"
            )
        {
            cx.stop_propagation();
        }
        cx.notify();
    }

    fn commit_text(&mut self, text: &str) {
        if self.pending_unlock.is_some() {
            return;
        }
        if let Some(session) = &self.session {
            if let Err(error) = session.activity() {
                self.status = error.to_string();
                return;
            }
        }
        match self.screen {
            Screen::Unlock if self.editing_path => {
                if !append_path(&mut self.path, text) {
                    self.status = "Document path exceeds the 4096-byte limit.".into();
                }
            }
            Screen::Unlock => {
                if !self.passphrase.push_str(text) {
                    self.status = "Passphrase input exceeds the 4096-byte limit.".into();
                }
            }
            Screen::Find | Screen::Bookmark => {
                if !self.query.push_str(text) {
                    self.status = "Search/bookmark input exceeds the 4096-byte limit.".into();
                }
                self.search_index = 0;
            }
            Screen::Editor => {
                if text.len() <= 1024 * 1024 {
                    self.apply(Intent::Type(text.into()));
                    self.apply(Intent::RevealCaret);
                } else {
                    self.status = "Text input exceeds the 1 MiB limit.".into();
                }
            }
            Screen::Home => {}
        }
    }

    fn pointer(&mut self, position: gpui::Point<Pixels>, extend: bool) {
        if !self.composition_ready() {
            return;
        }
        let x = f32::from(position.x - self.bounds.origin.x) - 24.;
        let y = f32::from(position.y - self.bounds.origin.y) - 24.;
        if let Some(session) = &mut self.session {
            let result = session.pointer(x, y, extend);
            self.report(result, "");
        }
    }
}

fn paint_rect(bounds: Bounds<Pixels>, rect: BoxRect, color: u32, window: &mut Window) {
    window.paint_quad(fill(
        Bounds::new(
            point(
                bounds.origin.x + px(24. + rect.x),
                bounds.origin.y + px(24. + rect.y),
            ),
            size(px(rect.width.max(0.)), px(rect.height.max(0.))),
        ),
        rgb(color),
    ));
}

fn paint_frame(bounds: Bounds<Pixels>, frame: VectorFrame, ink: u32, window: &mut Window) {
    window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
        for rect in &frame.selection {
            paint_rect(bounds, *rect, 0x719bb8, window);
        }
        for glyph in &frame.glyphs {
            let mut builder = PathBuilder::fill();
            for contour in &glyph.contours {
                if let Some((first, rest)) = contour.split_first() {
                    builder.move_to(point(
                        bounds.origin.x + px(24. + first.0),
                        bounds.origin.y + px(24. + first.1),
                    ));
                    for &(x, y) in rest {
                        builder.line_to(point(
                            bounds.origin.x + px(24. + x),
                            bounds.origin.y + px(24. + y),
                        ));
                    }
                    builder.close();
                }
            }
            if let Ok(path) = builder.build() {
                window.paint_path(path, rgb(ink));
            }
        }
        for rect in &frame.decorations {
            paint_rect(bounds, *rect, ink, window);
        }
        if let Some(mut caret) = frame.caret {
            caret.width = 1. / window.scale_factor();
            paint_rect(bounds, caret, ink, window);
        }
    });
}

impl Render for NativeHome {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title(&self.session.as_ref().map_or_else(
            || {
                if self.pending_unlock.is_some() {
                    format!(
                        "Holonomy Native - Working - {}",
                        if self.dark { "Dark" } else { "Light" }
                    )
                } else {
                    "Holonomy Native".to_string()
                }
            },
            |session| {
                format!(
                    "Holonomy Native — {}",
                    session
                        .path()
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                )
            },
        ));
        let palette = design::Palette::new(self.dark);
        let (bg, panel, ink, muted) = (palette.page, palette.page, palette.ink, palette.muted);
        let viewport = window.viewport_size();
        let sidebar_width = design::sidebar_width(f32::from(viewport.width));
        let hero_size = if sidebar_width == 0. { 28. } else { 36. };
        let mut body = div()
            .id("workspace")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .px_6()
            .py_4();
        match self.screen {
            Screen::Home => {
                body = body.max_w(px(760.)).w_full().mx_auto().justify_center()
                    .child(div().text_size(px(12.)).text_color(rgb(muted)).child("YOUR PRIVATE WORKSPACE"))
                    .child(div().text_size(px(hero_size)).child("Room for your thoughts."))
                    .child(div().text_size(px(14.)).text_color(rgb(muted)).child("A quiet page. An encrypted document. Nothing sent anywhere."))
                    .child(div().flex().flex_wrap().gap_3().mt_4()
                        .child(button("new", "+  New document").h(px(40.)).px_4().bg(rgb(palette.primary)).text_color(rgb(palette.on_primary))
                            .on_click(cx.listener(|this,_,_,cx| { this.begin_document(true); cx.notify(); })))
                        .child(button("open", "Open document").h(px(40.)).px_4().border_1().border_color(rgb(palette.border))
                            .on_click(cx.listener(|this,_,_,cx| { this.begin_document(false); cx.notify(); }))))
                    .child(div().mt_6().text_size(px(12.)).text_color(rgb(muted)).child("Experimental build · Start with a test document. Your passphrase is never stored."));
            }
            Screen::Unlock => {
                let busy = self.pending_unlock.is_some();
                let form_hint = if busy {
                    "Deriving your encryption key. Escape cancels.".to_string()
                } else if self.status.is_empty() {
                    "Tab to switch fields · Enter to continue".to_string()
                } else {
                    self.status.clone()
                };
                body =
                    body.w_full()
                        .max_w(px(design::FORM_WIDTH + 48.))
                        .mx_auto()
                        .justify_center()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(muted))
                                .child("ENCRYPTED DOCUMENT"),
                        )
                        .child(div().text_size(px(28.)).child(if self.create {
                            "Make space for a new idea."
                        } else {
                            "Welcome back."
                        }))
                        .child(div().text_size(px(13.)).text_color(rgb(muted)).child(
                            if self.create {
                                "Choose a location and a strong passphrase."
                            } else {
                                "Your document stays private. Unlock it to begin."
                            },
                        ))
                        .child(div().mt_4().text_size(px(12.)).child("Document location"))
                        .child(
                            field(
                                "path",
                                if self.path.is_empty() {
                                    "/home/you/Documents/notes.wavefunction".into()
                                } else {
                                    self.path.clone()
                                },
                                self.editing_path && !busy,
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.composition_ready() {
                                    this.editing_path = true;
                                }
                                cx.notify();
                            })),
                        )
                        .child(div().text_size(px(12.)).child("Passphrase"))
                        .child(
                            field(
                                "passphrase",
                                if self.passphrase.is_empty() {
                                    "Enter your passphrase".into()
                                } else {
                                    "•".repeat(self.passphrase.chars().count().min(64))
                                },
                                !self.editing_path && !busy,
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.composition_ready() {
                                    this.editing_path = false;
                                }
                                cx.notify();
                            })),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .items_center()
                                .mt_2()
                                .child(
                                    button("cancel", "Cancel")
                                        .h(px(design::CONTROL_HEIGHT))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cancel_unlock();
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    button(
                                        "unlock",
                                        if busy {
                                            "Working securely…"
                                        } else if self.create {
                                            "Create document"
                                        } else {
                                            "Unlock document"
                                        },
                                    )
                                    .h(px(design::CONTROL_HEIGHT))
                                    .px_4()
                                    .bg(rgb(palette.primary))
                                    .text_color(rgb(palette.on_primary))
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.unlock();
                                            cx.notify();
                                        },
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(muted))
                                .child(form_hint),
                        );
            }
            Screen::Editor | Screen::Find | Screen::Bookmark => {
                let title = self
                    .session
                    .as_ref()
                    .map(|s| {
                        format!(
                            "{}{}{}",
                            s.path().file_name().unwrap_or_default().to_string_lossy(),
                            if s.dirty() { " · unsaved" } else { "" },
                            if s.in_history() {
                                " · READ ONLY HISTORY"
                            } else {
                                ""
                            }
                        )
                    })
                    .unwrap_or_default();
                body = body
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(14.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(muted))
                                    .child("Encrypted · Local only"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .py_2()
                            .border_b_1()
                            .border_color(rgb(palette.border))
                            .child(button("home", "Home").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.home();
                                    cx.notify();
                                },
                            )))
                            .child(button("save", "Save").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.save();
                                    cx.notify();
                                },
                            )))
                            .child(button("undo", "↶ Undo").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.apply(Intent::Undo);
                                    cx.notify();
                                },
                            )))
                            .child(button("redo", "↷ Redo").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.apply(Intent::Redo);
                                    cx.notify();
                                },
                            )))
                            .child(button("bold", "B").on_click(cx.listener(|this, _, _, cx| {
                                this.format(0);
                                cx.notify();
                            })))
                            .child(
                                button("italic", "I").on_click(cx.listener(|this, _, _, cx| {
                                    this.format(1);
                                    cx.notify();
                                })),
                            )
                            .child(button("underline", "U").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.format(2);
                                    cx.notify();
                                },
                            )))
                            .child(button("find", "Find").on_click(cx.listener(
                                |this, _, _, cx| {
                                    if !this.composition_ready() {
                                        cx.notify();
                                        return;
                                    }
                                    this.screen = Screen::Find;
                                    this.query.zeroize();
                                    this.search_index = 0;
                                    cx.notify();
                                },
                            )))
                            .child(button("history", "History").on_click(cx.listener(
                                |this, _, _, cx| {
                                    if !this.composition_ready() {
                                        cx.notify();
                                        return;
                                    }
                                    this.show_history = !this.show_history;
                                    cx.notify();
                                },
                            )))
                            .child(button("bookmark", "Bookmark").on_click(cx.listener(
                                |this, _, _, cx| {
                                    if !this.composition_ready() {
                                        cx.notify();
                                        return;
                                    }
                                    this.query.zeroize();
                                    this.screen = Screen::Bookmark;
                                    cx.notify();
                                },
                            )))
                            .child(button("verify", "Verify").on_click(cx.listener(
                                |this, _, _, cx| {
                                    if let Some(s) = &this.session {
                                        let result = s.verify();
                                        this.report(result, "World-line authenticated.");
                                    }
                                    cx.notify();
                                },
                            )))
                            .child(button("wipe", "Emergency wipe").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.wipe(cx);
                                    cx.notify();
                                },
                            ))),
                    );
                let mut formatting = div().flex().flex_wrap().items_center().gap_1()
                    .py_2().border_b_1().border_color(rgb(palette.border));
                for (id, label, kind) in [("strike", "Strike", 3), ("code-span", "Code", 4)] {
                    formatting = formatting.child(button(id, label).on_click(cx.listener(move |this, _, _, cx| {
                        this.format(kind);
                        cx.notify();
                    })));
                }
                for (id, label, patch) in formatting_presets() {
                    formatting = formatting.child(button(id, label).on_click(cx.listener(move |this, _, _, cx| {
                        this.apply(Intent::Format(patch.clone()));
                        cx.notify();
                    })));
                }
                body = body.child(formatting);
                if self.screen != Screen::Editor {
                    body = body.child(format!(
                        "{} · Enter confirms · Escape cancels",
                        if self.screen == Screen::Find {
                            "Find"
                        } else {
                            "Bookmark name"
                        }
                    ));
                    let entity = cx.entity();
                    body = body.child(
                        canvas(
                            move |bounds, window, cx| {
                                entity.update(cx, |this, _| {
                                    holonomy_native::vector_text(
                                        &this.fonts,
                                        &this.query,
                                        f32::from(bounds.size.width) - 48.,
                                        window.scale_factor(),
                                    )
                                    .unwrap_or_default()
                                })
                            },
                            move |bounds, frame, window, _| paint_frame(bounds, frame, ink, window),
                        )
                        .w_full()
                        .h(px(72.)),
                    );
                }
                if self.show_history {
                    let mut history = div().flex().flex_wrap().gap_2().child(
                        button("current", "Current document").on_click(cx.listener(
                            |this, _, _, cx| {
                                if !this.composition_ready() {
                                    cx.notify();
                                    return;
                                }
                                if let Some(s) = &mut this.session {
                                    s.show_current();
                                }
                                cx.notify();
                            },
                        )),
                    );
                    if let Some(session) = &self.session {
                        for sequence in session.witnesses().into_iter().rev().take(30) {
                            history = history.child(
                                div()
                                    .id(("witness", sequence))
                                    .cursor_pointer()
                                    .p_2()
                                    .child(format!("Witness {sequence}"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.composition_ready() {
                                            cx.notify();
                                            return;
                                        }
                                        if let Some(s) = &mut this.session {
                                            let result = s.show_witness(sequence);
                                            this.report(result, "Read-only checkpoint.");
                                        }
                                        cx.notify();
                                    })),
                            );
                        }
                    }
                    body = body.child(history);
                }
                let entity = cx.entity();
                let drawing = canvas(
                    move |bounds, window, cx| {
                        entity.update(cx, |this, _| {
                            this.bounds = bounds;
                            let Some(session) = &mut this.session else {
                                return VectorFrame::default();
                            };
                            session.resize(
                                f32::from(bounds.size.width) - 48.,
                                f32::from(bounds.size.height) - 48.,
                            );
                            match session.frame(window.scale_factor()) {
                                Ok(frame) => frame,
                                Err(error) => {
                                    this.status = error.to_string();
                                    VectorFrame::default()
                                }
                            }
                        })
                    },
                    move |bounds, frame, window, _| paint_frame(bounds, frame, ink, window),
                )
                .size_full();
                body = body.child(
                    div()
                        .id("document")
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .bg(rgb(panel))
                        .w_full()
                        .max_w(px(design::DOCUMENT_WIDTH))
                        .mx_auto()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                                this.focus.focus(window);
                                this.dragging = true;
                                this.pointer(event.position, event.modifiers.shift);
                                cx.notify();
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, _| {
                                this.dragging = false;
                            }),
                        )
                        .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                            if this.dragging && event.dragging() {
                                this.pointer(event.position, true);
                                cx.notify();
                            }
                        }))
                        .on_scroll_wheel(cx.listener(
                            |this, event: &gpui::ScrollWheelEvent, _, cx| {
                                let delta = event.delta.pixel_delta(px(24.));
                                this.apply(Intent::ScrollBy(holonomy_native::fixed(-f32::from(
                                    delta.y,
                                ))));
                                cx.notify();
                            },
                        ))
                        .child(drawing),
                );
            }
        }
        if !self.preedit.is_empty() {
            let entity = cx.entity();
            body = body.child(
                canvas(
                    move |bounds, window, cx| {
                        entity.update(cx, |this, _| {
                            holonomy_native::vector_text(
                                &this.fonts,
                                if this.screen == Screen::Unlock && !this.editing_path {
                                    "••••"
                                } else {
                                    &this.preedit
                                },
                                f32::from(bounds.size.width) - 48.,
                                window.scale_factor(),
                            )
                            .unwrap_or_default()
                        })
                    },
                    move |bounds, frame, window, _| paint_frame(bounds, frame, ink, window),
                )
                .h(px(64.))
                .w_full(),
            );
        }
        if matches!(self.screen, Screen::Home | Screen::Unlock) {
            // A scroll viewport and a natural-height content column must be separate. Making
            // the constrained flex column itself scrollable lets wrapped text shrink/overlap.
            body = body
                .flex_none()
                .min_h(px((f32::from(viewport.height) - 82.).max(0.)));
            if f32::from(viewport.height) < 640. {
                body = body.justify_start();
            }
            body = div()
                .id("page-scroll")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_y_scroll()
                .child(body);
        }
        let input_entity = cx.entity();
        let focus = self.focus.clone();
        let input = canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                window.handle_input(
                    &focus,
                    gpui::ElementInputHandler::new(bounds, input_entity),
                    cx,
                );
            },
        )
        .absolute()
        .size_full();
        let mut shell = div()
            .size_full()
            .flex()
            .bg(rgb(bg))
            .text_color(rgb(ink))
            .text_size(px(14.))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .child(input);
        if sidebar_width > 0. {
            shell = shell.child(
                div()
                    .w(px(sidebar_width))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .bg(rgb(palette.rail))
                    .px_3()
                    .py_4()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_2()
                            .pb_6()
                            .child("Holonomy"),
                    )
                    .child(
                        button("rail-home", "Overview")
                            .justify_start()
                            .h(px(36.))
                            .px_3()
                            .bg(rgb(if self.screen == Screen::Home {
                                palette.selected
                            } else {
                                palette.rail
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.session.is_some() {
                                    this.home();
                                } else {
                                    this.cancel_unlock();
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        button("rail-new", "+  New document")
                            .justify_start()
                            .h(px(36.))
                            .px_3()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.begin_document(true);
                                cx.notify();
                            })),
                    )
                    .child(
                        button("rail-open", "Open document")
                            .justify_start()
                            .h(px(36.))
                            .px_3()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.begin_document(false);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .mt_6()
                            .px_3()
                            .text_size(px(11.))
                            .text_color(rgb(muted))
                            .child("WORKSPACE"),
                    )
                    .child(
                        div()
                            .px_3()
                            .text_size(px(12.))
                            .text_color(rgb(muted))
                            .overflow_hidden().whitespace_nowrap().text_ellipsis()
                            .child(self.session.as_ref().map(|s| format!("▤  {}", s.path().file_name().unwrap_or_default().to_string_lossy())).unwrap_or_else(|| "No document open".into())),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .border_t_1()
                            .border_color(rgb(palette.border))
                            .pt_3()
                            .px_3()
                            .text_size(px(11.))
                            .text_color(rgb(muted))
                            .child("Private by design"),
                    ),
            );
        }
        shell.child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(52.))
                        .flex_shrink_0()
                        .px_6()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(rgb(palette.border))
                        .child(button("workspace-home", "Holonomy  ›").on_click(cx.listener(
                            |this, _, _, cx| {
                                if this.session.is_some() {
                                    this.home();
                                } else {
                                    this.cancel_unlock();
                                }
                                cx.notify();
                            },
                        )))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis()
                            .child(self.session.as_ref().map(|s| s.path().file_name().unwrap_or_default().to_string_lossy().into_owned()).unwrap_or_else(|| "Workspace".into())))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(button("header-open", "Open").on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.begin_document(false);
                                        cx.notify();
                                    },
                                )))
                                .child(
                                    button("theme", if self.dark { "Light" } else { "Dark" })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.dark = !this.dark;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .child(body)
                .child(
                    div()
                        .h(px(30.))
                        .flex_shrink_0()
                        .px_6()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .text_size(px(11.))
                        .text_color(rgb(muted))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(self.status.clone()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .child(format!("{:.2}× · Local workspace", window.scale_factor())),
                        ),
                ),
        )
    }
}

// Keep IME preedit separate from the encrypted editor until the platform commits it. This
// avoids turning each composition update into an undoable/persisted edit. The context deliberately
// excludes surrounding document/passphrase text; reconversion outside the marked range is refused.
impl gpui::EntityInputHandler for NativeHome {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        adjusted: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.screen == Screen::Unlock && !self.editing_path {
            return None;
        }
        let bytes = holonomy_native::utf16_range_bytes(&self.preedit, range.clone())?;
        *adjusted = Some(range);
        Some(self.preedit[bytes].to_string())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<gpui::UTF16Selection> {
        if self.screen == Screen::Home {
            return None;
        }
        let end = self.preedit.encode_utf16().count();
        Some(gpui::UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }
    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        if self.preedit.is_empty() {
            None
        } else {
            Some(0..self.preedit.encode_utf16().count())
        }
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let text = std::mem::take(&mut self.preedit);
        if !text.is_empty() {
            self.commit_text(&text);
        }
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<std::ops::Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_unlock.is_some() {
            return;
        }
        let range = range.unwrap_or(0..self.preedit.encode_utf16().count());
        if !self.preedit.replace_utf16(range, text) {
            self.status =
                "Input range is unsupported or exceeds the 4096-byte composition limit.".into();
            cx.notify();
            return;
        }
        let committed = std::mem::take(&mut self.preedit);
        self.commit_text(&committed);
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<std::ops::Range<usize>>,
        text: &str,
        _: Option<std::ops::Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_unlock.is_some() {
            return;
        }
        let range = range.unwrap_or(0..self.preedit.encode_utf16().count());
        if !self.preedit.replace_utf16(range, text) {
            self.status = "Composition range is unsupported or exceeds the 4096-byte limit.".into();
        }
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if let Some(session) = &mut self.session {
            if let Some(caret) = session.frame(1.).ok()?.caret {
                return Some(Bounds::new(
                    point(
                        self.bounds.origin.x + px(24. + caret.x),
                        self.bounds.origin.y + px(24. + caret.y),
                    ),
                    size(px(1.), px(caret.height)),
                ));
            }
        }
        Some(bounds)
    }
    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let entity = cx.new(NativeHome::new);
                entity.read(cx).focus.focus(window);
                let weak = entity.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    weak.update(cx, |this, cx| {
                        let saved = this.save();
                        cx.notify();
                        saved
                    })
                    .unwrap_or(false)
                });
                entity
            },
        )
        .expect("could not open the native frontend");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}

// Every visible format control maps to an undoable engine patch, not Markdown insertion.
fn formatting_presets() -> Vec<(&'static str, &'static str, StylePatch)> {
    let mut result = Vec::new();
    for (id, label, heading) in [("body-text", "Text", Heading::None), ("heading-1", "H1", Heading::H1), ("heading-2", "H2", Heading::H2)] {
        let mut patch = StylePatch::new();
        patch.heading = Some(heading);
        result.push((id, label, patch));
    }
    for (id, label, alignment) in [("align-start", "Left", Alignment::Start), ("align-center", "Center", Alignment::Center), ("align-end", "Right", Alignment::End)] {
        let mut patch = StylePatch::new();
        patch.alignment = Some(alignment);
        result.push((id, label, patch));
    }
    let mut bullet = StylePatch::new();
    bullet.list = Some(ListKind::Bullet { depth: 1 });
    result.push(("bullet-list", "• List", bullet));
    let mut plain = StylePatch::new();
    plain.list = Some(ListKind::None);
    result.push(("no-list", "No list", plain));
    result.push(("clear-format", "Clear format", StylePatch::clear_all()));
    result
}

// Forms do not inherit the document editor's clipboard shortcuts. Route paste explicitly
// through their bounded input path, never as a document edit or a partial field insertion.
fn form_paste(screen: Screen, command: bool, key: &str) -> bool {
    command && key == "v" && matches!(screen, Screen::Unlock | Screen::Find | Screen::Bookmark)
}

fn append_path(path: &mut String, text: &str) -> bool {
    if path.len().saturating_add(text.len()) > 4096 {
        return false;
    }
    path.push_str(text);
    true
}

// Saving cannot silently drop text that still belongs to the platform's composition. Refuse
// until the IME commits/cancels rather than guessing at a commit and later inserting it twice.
fn composition_ready(preedit: &SecretInput, status: &mut String) -> bool {
    match ensure_no_composition(preedit) {
        Ok(()) => true,
        Err(error) => {
            *status = error.to_string();
            false
        }
    }
}

fn ensure_no_composition(preedit: &SecretInput) -> holonomy_native::Result<()> {
    if preedit.is_empty() {
        Ok(())
    } else {
        Err("Finish or cancel the current text composition before continuing.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_paste_is_routed_only_to_active_fields() {
        for screen in [Screen::Unlock, Screen::Find, Screen::Bookmark] {
            assert!(form_paste(screen, true, "v"));
            assert!(!form_paste(screen, false, "v"));
            assert!(!form_paste(screen, true, "c"));
        }
        assert!(!form_paste(Screen::Home, true, "v"));
        assert!(!form_paste(Screen::Editor, true, "v"));
    }

    #[test]
    fn path_paste_is_atomic_and_utf8_byte_bounded() {
        let mut path = "x".repeat(4094);
        assert!(append_path(&mut path, "é"));
        assert_eq!(path.len(), 4096);
        assert!(!append_path(&mut path, "x"));
        assert_eq!(path.len(), 4096);
        let mut original = "notes.wavefunction".to_string();
        assert!(!append_path(&mut original, &"x".repeat(4096)));
        assert_eq!(original, "notes.wavefunction");
    }

    #[test]
    fn unlock_worker_guard_releases_single_flight_gate_on_drop() {
        let active = Arc::new(AtomicBool::new(true));
        let guard = UnlockWorker(Arc::clone(&active));
        assert!(active.load(Ordering::Acquire));
        drop(guard);
        assert!(!active.load(Ordering::Acquire));
    }

    #[test]
    fn composition_transition_guard_preserves_pending_text_and_reports_refusal() {
        let mut input = SecretInput::default();
        input.push_str("किताब");
        let mut status = String::new();
        assert!(!composition_ready(&input, &mut status));
        assert!(!status.is_empty());
        assert_eq!(&*input, "किताब");
        input.zeroize();
        assert!(composition_ready(&input, &mut status));
    }

    #[test]
    fn pending_composition_refuses_save_without_consuming_text() {
        let mut input = SecretInput::default();
        input.push_str("किताब");
        assert!(ensure_no_composition(&input).is_err());
        assert_eq!(&*input, "किताब");
        input.zeroize();
        assert!(ensure_no_composition(&input).is_ok());
    }
}
