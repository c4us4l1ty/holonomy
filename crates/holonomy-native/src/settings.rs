//! Separate native settings window. Holds no document text and only a weak workspace handle.
use super::{button, design, NativeHome};
use gpui::{div, prelude::*, px, rgb, Context, FocusHandle, KeyDownEvent, WeakEntity, Window};

pub struct SettingsWindow {
    owner: WeakEntity<NativeHome>,
    focus: FocusHandle,
    category: usize,
}

impl SettingsWindow {
    pub fn new(owner: gpui::Entity<NativeHome>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&owner, |_, _, cx| cx.notify()).detach();
        let focus = cx.focus_handle();
        focus.focus(window);
        Self { owner: owner.downgrade(), focus, category: 0 }
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title("Holonomy Settings");
        let owner = self.owner.upgrade();
        let (dark, expanded) = owner.as_ref().map(|owner| {
            let owner = owner.read(cx);
            (owner.dark, owner.show_formatting)
        }).unwrap_or((false, false));
        let palette = design::Palette::new(dark);
        let mut rail = div().w(px(180.)).flex_shrink_0().h_full().p_4().bg(rgb(palette.rail))
            .flex().flex_col().gap_2().child(div().mb_4().child("Settings"));
        let categories = ["General", "Appearance", "Shortcuts", "Documents", "Privacy", "About"];
        for (index, label) in categories.iter().enumerate() {
            rail = rail.child(div().id(("settings-category", index)).cursor_pointer().p_2().rounded_md()
                .bg(rgb(if self.category == index { palette.selected } else { palette.rail }))
                .child(*label).on_click(cx.listener(move |this, _, _, cx| { this.category = index; cx.notify(); })));
        }
        let mut content = div().id("settings-content").flex_1().min_w_0().h_full().overflow_y_scroll().p_6()
            .child(div().w_full().max_w(px(660.)).mx_auto().flex().flex_col().gap_4()
                .child(div().text_size(px(24.)).child(categories[self.category])));
        let mut card = div().w_full().max_w(px(660.)).mx_auto().mt_4().p_4().rounded_xl()
            .border_1().border_color(rgb(palette.border)).bg(rgb(palette.rail)).flex().flex_col().gap_4();
        match self.category {
            0 => {
                card = card.child("Workspace preferences")
                    .child("Changes apply immediately to the editor and last for this session.")
                    .child("Launch: Home, or the encrypted document supplied on the command line.")
                    .child("Interface language: English.");
            }
            1 => {
                let owner = self.owner.clone();
                card = card.child("Appearance")
                    .child(button("settings-theme", if dark { "Theme: Dark — switch to Light" } else { "Theme: Light — switch to Dark" })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let _ = owner.update(cx, |owner, cx| { owner.dark = !owner.dark; cx.notify(); });
                        })));
                let owner = self.owner.clone();
                card = card.child(button("settings-toolbar", if expanded { "Extended toolbar: shown" } else { "Extended toolbar: hidden" })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let _ = owner.update(cx, |owner, cx| { owner.show_formatting = !owner.show_formatting; cx.notify(); });
                    })));
            }
            2 => {
                for shortcut in ["Ctrl/Cmd+, — Settings", "Ctrl/Cmd+S — Save", "Ctrl/Cmd+N / O — New / Open", "Ctrl/Cmd+Z / Shift+Z — Undo / Redo", "Ctrl/Cmd+B / I / U — Bold / Italic / Underline", "Ctrl/Cmd+F — Find", "Ctrl/Cmd+Shift+T — Theme", "Ctrl/Cmd+Shift+Escape — Emergency wipe", "Escape — Close this settings window"] {
                    card = card.child(shortcut);
                }
            }
            3 => {
                card = card.child("Encrypted documents")
                    .child("Use the workspace sidebar to browse folders and .wavefunction files. Selecting another document first saves and closes the current one.")
                    .child("Failed saves and unfinished text composition block document switching.")
                    .child("No plaintext thumbnails or recent-document index are written.");
            }
            4 => {
                card = card.child("Local by design")
                    .child("The editor does not fetch web images or send selected text to web search or an agent. No automatic update checks run inside the editor.")
                    .child("Clipboard copy is explicit plaintext egress. Emergency wipe clears the system clipboard, but cannot erase copies held by other applications.")
                    .child("Folder browsing reads file metadata, not decrypted document previews.");
            }
            _ => {
                card = card.child("Holonomy — experimental encrypted editor")
                    .child("Native GPUI interface with vector-rendered document text.")
                    .child("Design adapted from Quiet MD (MIT). See bundled THIRD-PARTY-NOTICES.txt.")
                    .child("This build is experimental. Keep backups and start with a test document.");
            }
        }
        content = content.child(card).child(div().h(px(24.)));
        div().size_full().flex().bg(rgb(palette.page)).text_color(rgb(palette.ink)).text_size(px(14.))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, window, cx| {
                let command = if cfg!(target_os = "macos") { event.keystroke.modifiers.platform } else { event.keystroke.modifiers.control };
                if event.keystroke.key == "escape" || (command && event.keystroke.key == "w") {
                    window.remove_window(); cx.stop_propagation();
                }
            }))
            .child(rail).child(content)
    }
}
