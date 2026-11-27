//! Opt-in GPUI editor: native vector glyphs, with the existing encrypted session underneath.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use gpui::{
    canvas, div, fill, point, prelude::*, px, rgb, size, App, Application, Bounds, Context,
    FocusHandle, KeyDownEvent, MouseButton, PathBuilder, Pixels, Window, WindowBounds,
    WindowOptions,
};
use holonomy_native::{BoxRect, SecretInput, Session, VectorFrame};
use holonomy_shell::{Intent, Move};
use holonomy_text::{StylePatch, TriState};
use std::path::Path;
use zeroize::Zeroize;

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Home,
    Unlock,
    Editor,
    Find,
    Bookmark,
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
    status: String,
    bounds: Bounds<Pixels>,
    dragging: bool,
    show_history: bool,
}

fn button(id: &'static str, label: impl Into<gpui::SharedString>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .cursor_pointer()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(0x888888))
        .child(label.into())
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
            status: String::new(),
            bounds: Bounds::default(),
            dragging: false,
            show_history: false,
        }
    }

    fn report(&mut self, result: holonomy_native::Result<()>, success: &str) {
        self.status = match result {
            Ok(()) => success.into(),
            Err(error) => error.to_string(),
        };
    }

    fn apply(&mut self, intent: Intent) {
        if let Some(session) = self.session.as_mut() {
            let navigation = matches!(intent, Intent::RevealCaret);
            let result = session.apply(intent);
            if !navigation || result.is_err() {
                self.report(result, "");
            }
        }
    }

    fn save(&mut self) -> bool {
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
        if self.path.trim().is_empty() {
            self.status = "Choose a document path.".into();
            return;
        }
        // Never convert a refused open into create. Create itself uses exclusive file creation.
        let result = if self.create {
            Session::create(Path::new(&self.path), self.passphrase.as_bytes())
        } else {
            Session::open(Path::new(&self.path), self.passphrase.as_bytes())
        };
        self.preedit.zeroize();
        self.passphrase.zeroize();
        match result {
            Ok(mut session) => {
                if let Some(ms) = std::env::var("HOLONOMY_DEADMAN_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|&ms| ms > 0)
                {
                    if let Err(error) = session.set_deadman(std::time::Duration::from_millis(ms)) {
                        self.status = error.to_string();
                        return;
                    }
                }
                self.session = Some(session);
                self.screen = Screen::Editor;
                self.status.clear();
            }
            Err(error) => {
                self.status = error.to_string();
                self.editing_path = false;
            }
        }
    }

    fn format(&mut self, kind: u8) {
        if let Some(session) = &self.session {
            let toolbar = session.toolbar();
            let mut patch = StylePatch::new();
            match kind {
                0 => patch.bold = Some(!matches!(toolbar.bold, TriState::Uniform(true))),
                1 => patch.italic = Some(!matches!(toolbar.italic, TriState::Uniform(true))),
                _ => patch.underline = Some(!matches!(toolbar.underline, TriState::Uniform(true))),
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
        let verified = self.session.as_mut().map_or(true, Session::wipe);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new()));
        self.session.take();
        self.preedit.zeroize();
        self.passphrase.zeroize();
        self.query.zeroize();
        self.screen = Screen::Home;
        self.status = if verified {
            "Resident text/keys wiped; clipboard cleared. GPU memory and clipboard history are not qualified."
        } else {
            "Key wipe could not be confirmed."
        }
        .into();
    }

    fn clipboard(&mut self, cut: bool, cx: &mut Context<Self>) {
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
        if command && mods.shift && key == "escape" {
            self.wipe(cx);
            cx.notify();
            return;
        }
        if self.session.as_ref().is_some_and(|session| session.activity().is_err()) {
            self.wipe(cx);
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
        if !self.preedit.is_empty() && !command {
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
        if let Some(session) = &self.session {
            if let Err(error) = session.activity() {
                self.status = error.to_string();
                return;
            }
        }
        match self.screen {
            Screen::Unlock if self.editing_path => {
                if self.path.len().saturating_add(text.len()) <= 4096 {
                    self.path.push_str(text);
                }
            }
            Screen::Unlock => {
                self.passphrase.push_str(text);
            }
            Screen::Find | Screen::Bookmark => {
                self.query.push_str(text);
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
            || "Holonomy Native".to_string(),
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
        let (bg, panel, ink, muted) = if self.dark {
            (0x191b1e, 0x222529, 0xeeeeec, 0xa8acae)
        } else {
            (0xfaf9f6, 0xffffff, 0x242628, 0x666b70)
        };
        let mut body = div().flex_1().min_h_0().flex().flex_col().gap_4().p_4();
        match self.screen {
            Screen::Home => {
                body = body.child(div().text_size(px(30.)).child("A quiet place to write"))
                    .child("Native vector rendering · encrypted .wavefunction documents")
                    .child(div().flex().gap_4()
                        .child(button("new", "New encrypted document").on_click(cx.listener(|this,_,_,cx| { this.create=true; this.path.clear(); this.passphrase.zeroize(); this.editing_path=true; this.screen=Screen::Unlock; this.status.clear(); cx.notify(); })))
                        .child(button("open", "Open encrypted document").on_click(cx.listener(|this,_,_,cx| { this.create=false; this.passphrase.zeroize(); this.editing_path=true; this.screen=Screen::Unlock; this.status.clear(); cx.notify(); }))))
                    .child("Development frontend: IME, accessibility, suspend/USB integration and GPU scene cleanup are not qualified. Use test documents only.")
                    .child(div().text_size(px(12.)).child("12 px · Hamburgefontsiv 0123456789 Il1 O0"))
                    .child(div().text_size(px(24.)).child("24 px · Native-resolution text"));
            }
            Screen::Unlock => {
                body = body
                    .child(if self.create {
                        "Create a new encrypted document"
                    } else {
                        "Unlock an existing document"
                    })
                    .child("Path (Tab switches fields; Enter submits)")
                    .child(
                        button(
                            "path",
                            if self.path.is_empty() {
                                "Type an absolute .wavefunction path".to_string()
                            } else {
                                self.path.clone()
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.editing_path = true;
                            cx.notify();
                        })),
                    )
                    .child(if self.editing_path {
                        "Editing: path"
                    } else {
                        "Editing: passphrase"
                    })
                    .child(
                        button(
                            "passphrase",
                            format!(
                                "Passphrase: {}",
                                "•".repeat(self.passphrase.chars().count().min(64))
                            ),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.editing_path = false;
                            cx.notify();
                        })),
                    )
                    .child(
                        button("unlock", if self.create { "Create" } else { "Unlock" }).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.unlock();
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        button("cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| {
                            this.passphrase.zeroize();
                            this.screen = Screen::Home;
                            cx.notify();
                        })),
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
                body = body.child(title).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            button("home", "Home").on_click(cx.listener(|this, _, _, cx| {
                                this.home();
                                cx.notify();
                            })),
                        )
                        .child(
                            button("save", "Save").on_click(cx.listener(|this, _, _, cx| {
                                this.save();
                                cx.notify();
                            })),
                        )
                        .child(
                            button("undo", "Undo").on_click(cx.listener(|this, _, _, cx| {
                                this.apply(Intent::Undo);
                                cx.notify();
                            })),
                        )
                        .child(
                            button("redo", "Redo").on_click(cx.listener(|this, _, _, cx| {
                                this.apply(Intent::Redo);
                                cx.notify();
                            })),
                        )
                        .child(
                            button("bold", "Bold").on_click(cx.listener(|this, _, _, cx| {
                                this.format(0);
                                cx.notify();
                            })),
                        )
                        .child(button("italic", "Italic").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.format(1);
                                cx.notify();
                            },
                        )))
                        .child(button("underline", "Underline").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.format(2);
                                cx.notify();
                            },
                        )))
                        .child(
                            button("find", "Find").on_click(cx.listener(|this, _, _, cx| {
                                this.screen = Screen::Find;
                                this.query.zeroize();
                                this.search_index = 0;
                                cx.notify();
                            })),
                        )
                        .child(button("history", "History").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.show_history = !this.show_history;
                                cx.notify();
                            },
                        )))
                        .child(button("bookmark", "Bookmark").on_click(cx.listener(
                            |this, _, _, cx| {
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
        div()
            .size_full()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .bg(rgb(bg))
            .text_color(rgb(ink))
            .text_size(px(15.))
            .on_key_down(cx.listener(Self::key))
            .child(input)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .p_4()
                    .border_b_1()
                    .border_color(rgb(muted))
                    .child("Holonomy · native development frontend")
                    .child(button("theme", "Toggle theme").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.dark = !this.dark;
                            cx.notify();
                        },
                    ))),
            )
            .child(body)
            .child(
                div()
                    .p_3()
                    .text_size(px(12.))
                    .text_color(rgb(muted))
                    .child(format!(
                        "Device scale {:.2}× · {}",
                        window.scale_factor(),
                        self.status
                    )),
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

// Saving cannot silently drop text that still belongs to the platform's composition. Refuse
// until the IME commits/cancels rather than guessing at a commit and later inserting it twice.
fn ensure_no_composition(preedit: &SecretInput) -> holonomy_native::Result<()> {
    if preedit.is_empty() { Ok(()) } else {
        Err("Finish or cancel the current text composition before saving or closing.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
