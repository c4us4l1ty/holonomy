//! In-app settings page. Grouped row/card layout adapted from Quiet MD (MIT),
//! 668f2011fa3d1d5626ed523bbcf68daae055a007; no storage/updater/network code copied.
use super::{button, chrome, design, NativeHome};
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Window};

const CATEGORIES: [&str; 6] = ["General", "Appearance", "Shortcuts", "Documents", "Privacy", "About"];

fn row(title: &str, description: &str, control: impl IntoElement, palette: design::Palette) -> gpui::Div {
    div().w_full().flex().flex_wrap().items_center().justify_between().gap_3().p_4()
        .border_b_1().border_color(rgb(palette.border))
        .child(div().flex_1().min_w(px(160.)).flex().flex_col().gap_1()
            .child(title.to_string())
            .child(div().text_size(px(12.)).text_color(rgb(palette.muted)).child(description.to_string())))
        .child(control)
}

fn group(title: &str, rows: Vec<gpui::Div>, palette: design::Palette) -> gpui::Div {
    div().w_full().flex().flex_col().gap_2().mb_6()
        .child(div().px_1().text_size(px(13.)).child(title.to_string()))
        .child(div().rounded_xl().overflow_hidden().bg(rgb(palette.rail))
            .border_1().border_color(rgb(palette.border)).children(rows))
}

pub fn render(owner: &mut NativeHome, window: &mut Window, cx: &mut Context<NativeHome>) -> AnyElement {
    let palette = design::Palette::new(owner.dark);
    let page = owner.settings_page.unwrap_or(0).min(CATEGORIES.len() - 1);
    let compact = f32::from(window.viewport_size().width) < 640.;
    let mut rail = div().flex().flex_col().gap_1().p_3().w(px(224.)).h_full()
        .flex_shrink_0().bg(rgb(palette.rail))
        .child(button("settings-back", "<  Back").justify_start().on_click(cx.listener(|this, _, _, cx| this.close_settings(cx))))
        .child(div().px_2().py_3().child("Settings"));
    for (index, label) in CATEGORIES.iter().enumerate() {
        rail = rail.child(div().id(("settings-category", index)).cursor_pointer().px_3().py_2().rounded_md()
            .bg(rgb(if page == index { palette.selected } else { palette.rail })).child(*label)
            .on_click(cx.listener(move |this, _, _, cx| { this.settings_page = Some(index); cx.notify(); })));
    }
    if compact { rail = rail.w_full().h_auto().flex_row().flex_wrap(); }
    let mut column = div().w_full().max_w(px(660.)).mx_auto().flex().flex_col()
        .child(div().text_size(px(22.)).mb_6().child(CATEGORIES[page]));
    if let Some(error) = &owner.save_failure {
        column = column.child(group("Save failed — document retained", vec![row("Changes are not confirmed durable", error, div().child("Return to document"), palette)], palette));
    }
    match page {
        0 => {
            column = column.child(group("Basics", vec![
                row("Interface language", "English is currently supported.", div().child("English"), palette),
                row("On launch", "Documents supplied on the command line always require unlock.", div().child("Home / locked document"), palette),
                row("Quick new", "Create a new encrypted document with a passphrase.", div().child("Ctrl/Cmd+N"), palette),
            ], palette)).child(group("Workspace", vec![
                row("Home layout", "Choose how encrypted document filenames are displayed.",
                    button("general-grid", if owner.home_grid { "Grid" } else { "List" }).on_click(cx.listener(|this, _, _, cx| { this.home_grid = !this.home_grid; cx.notify(); })), palette),
                row("Preferences", "Appearance and view choices last for this session. No document content is written to settings.", div().child("Session only"), palette),
            ], palette));
        }
        1 => {
            column = column.child(group("Appearance", vec![
                row("Appearance", "The editor and settings share the same palette.", div().flex().gap_2()
                    .child(button("appearance-light", "Light").bg(rgb(if !owner.dark { palette.selected } else { palette.rail })).on_click(cx.listener(|this, _, _, cx| { this.dark = false; cx.notify(); })))
                    .child(button("appearance-dark", "Dark").bg(rgb(if owner.dark { palette.selected } else { palette.rail })).on_click(cx.listener(|this, _, _, cx| { this.dark = true; cx.notify(); }))), palette),
                row("Window background", "Opaque surfaces keep the UI consistent across Linux compositors.", div().child("Opaque"), palette),
            ], palette)).child(group("Interface", vec![
                row("Extended formatting", "Headings, lists, alignment, history, bookmarks and verification.", button("settings-toolbar", if owner.show_formatting { "On" } else { "Off" }).on_click(cx.listener(|this, _, _, cx| { this.show_formatting = !this.show_formatting; cx.notify(); })), palette),
                row("Workspace sidebar", "Show the document browser beside the writing area.", button("settings-sidebar", if !owner.sidebar_hidden { "Shown" } else { "Hidden" }).on_click(cx.listener(|this, _, _, cx| { this.sidebar_hidden = !this.sidebar_hidden; cx.notify(); })), palette),
            ], palette)).child(group("Document", vec![
                row("Document font and text size", "Use the editor toolbar to change selected text; changes are undoable and saved encrypted.", div().child("Sans / Serif / Mono"), palette),
                row("Display scale", "GPUI uses your desktop's device scale. Text-size controls are not native zoom.", div().child(format!("{:.2}×", window.scale_factor())), palette),
            ], palette));
        }
        2 => {
            let command = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
            let rows = [("New document", "N"), ("Open document", "O"), ("Open / close Settings", ","), ("Close application (save guarded)", "Q"), ("Save document", "S"), ("Undo", "Z"), ("Redo", "Shift+Z"), ("Find in document", "F"), ("Bold / Italic / Underline", "B / I / U"), ("Toggle dark mode", "Shift+T"), ("Emergency wipe", "Shift+Escape")]
                .into_iter().map(|(label, key)| row(label, "", div().px_3().py_1().rounded_md().border_1().border_color(rgb(palette.border)).child(format!("{command}  {key}")), palette)).collect();
            column = column.child(group("App and documents", rows, palette))
                .child(group("Navigation", vec![row("Return from Settings", "Restores the document or form you left, without closing it.", div().child("Escape"), palette)], palette));
        }
        3 => {
            column = column.child(group("Folders", vec![
                row("Workspace folder", &owner.library.folder.to_string_lossy(), button("settings-refresh", "Refresh").on_click(cx.listener(|this, _, _, cx| { this.library.refresh(); cx.notify(); })), palette),
                row("Hidden folders", "Show or hide names beginning with a dot.", button("settings-hidden", if owner.hide_hidden { "Hidden" } else { "Shown" }).on_click(cx.listener(|this, _, _, cx| { this.hide_hidden = !this.hide_hidden; cx.notify(); })), palette),
                row("Document order", "Sort the metadata-only library by filename.", button("settings-order", if owner.sort_descending { "Title Z–A" } else { "Title A–Z" }).on_click(cx.listener(|this, _, _, cx| { this.sort_descending = !this.sort_descending; cx.notify(); })), palette),
            ], palette)).child(group("Encrypted storage", vec![
                row("History", "Authenticated witnesses are retained by the encrypted engine. Settings does not prune or rewrite them.", div().child("Append-only"), palette),
                row("Switching documents", "Failed saves and unfinished composition block switching. Locked documents have no content preview.", div().child("Save guarded"), palette),
            ], palette));
        }
        4 => {
            column = column.child(group("Network and privacy", vec![
                row("Web search and remote images", "Selected text and document content are not sent to web services.", div().child("Never fetched"), palette),
                row("Agent connector", "No document-to-agent integration is enabled in this editor.", div().child("Unavailable"), palette),
                row("Automatic update checks", "The editor makes no background update requests. The launcher downloads a source-matched bundle when needed.", div().child("Off"), palette),
                row("Clipboard", "Copy is explicit plaintext egress. Wipe cannot erase copies retained by other applications.", div().child("Explicit only"), palette),
            ], palette));
        }
        _ => {
            column = column.child(div().flex().flex_col().items_center().gap_3().py_6()
                .child(div().text_size(px(32.)).child("Holonomy"))
                .child(format!("Version {} · Experimental", env!("CARGO_PKG_VERSION")))
                .child(div().text_color(rgb(palette.muted)).child("Local encrypted documents. Native vector text.")))
                .child(group("Project", vec![row("Project page", "github.com/c4us4l1ty/holonomy", div().child("Holonomy"), palette), row("Design attribution", "Quiet MD · MIT. See bundled THIRD-PARTY-NOTICES.txt.", div().child("Open source"), palette)], palette));
        }
    }
    let content = div().flex_1().min_w_0().min_h_0().h_full().flex().flex_col()
        .child(div().h(px(40.)).flex_shrink_0().flex().items_center()
            .child(div().flex_1().h_full().on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.start_window_move()))
            .child(chrome::controls(palette.ink, cx)))
        .child(div().id("settings-scroll").flex_1().min_h_0().overflow_y_scroll().px_6().py_4().child(column));
    let mut root = div().size_full().flex().bg(rgb(palette.page)).text_color(rgb(palette.ink)).text_size(px(14.))
        .track_focus(&owner.focus).on_key_down(cx.listener(NativeHome::key));
    if compact { root = root.flex_col(); }
    root.child(rail).child(content).into_any_element()
}
