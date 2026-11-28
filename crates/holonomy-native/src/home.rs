//! Metadata-only workspace cards and menus, visually informed by Quiet MD (MIT),
//! 668f2011fa3d1d5626ed523bbcf68daae055a007. No decrypted previews or disk index.
use super::{button, design, NativeHome};
use gpui::{div, prelude::*, px, rgb, rgba, AnyElement, Context};

#[derive(Clone, Copy, PartialEq)]
pub enum Menu { New, View, More }

pub fn cards(owner: &NativeHome, cx: &mut Context<NativeHome>) -> gpui::Div {
    let palette = design::Palette::new(owner.dark);
    let entries = owner.library.visible_entries("", owner.hide_hidden, owner.sort_descending);
    let mut cards = div().w_full().flex().flex_wrap().gap_4();
    if !owner.home_grid { cards = cards.flex_col(); }
    for (index, entry) in entries.into_iter().enumerate() {
        let name = entry.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let mut card = div().id(("home-card", index)).cursor_pointer().p_4().rounded_xl()
            .border_1().border_color(rgb(palette.border)).flex().flex_col().gap_3()
            .hover(|style| style.bg(rgba(0x88888810)))
            .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(name))
            .child(div().text_size(px(11.)).text_color(rgb(palette.muted))
                .child(if entry.directory { "Folder" } else { "Encrypted document · Locked" }));
        if owner.home_grid { card = card.w(px(280.)).max_w_full().h(px(if entry.directory { 92. } else { 320. })); }
        else { card = card.w_full().flex_row().items_center().justify_between(); }
        cards = cards.child(card.on_click(cx.listener(move |this, _, _, cx| {
            this.open_library_entry(&entry); cx.notify();
        })));
    }
    if owner.library.visible_entries("", owner.hide_hidden, owner.sort_descending).is_empty() {
        cards = cards.child(div().p_4().text_size(px(13.)).text_color(rgb(palette.muted))
            .child("No encrypted documents in this folder. Use + to create one, or Open to choose a path."));
    }
    cards
}

pub fn overlay(owner: &NativeHome, cx: &mut Context<NativeHome>) -> Option<AnyElement> {
    let palette = design::Palette::new(owner.dark);
    if owner.search_open || owner.folder_create {
        let title = if owner.folder_create { "New folder" } else { "Search documents" };
        let mut content = div().w_full().max_w(px(560.)).max_h_full().rounded_xl().bg(rgb(palette.rail))
            .border_1().border_color(rgb(palette.border)).shadow_lg().overflow_hidden().flex().flex_col()
            .child(div().p_4().flex().items_center().justify_between().child(title)
                .child(button("close-palette", "Esc").on_click(cx.listener(|this, _, _, cx| {
                    this.search_open = false; this.folder_create = false; this.library_filter.clear(); cx.notify();
                }))))
            .child(div().px_4().py_3().border_t_1().border_b_1().border_color(rgb(palette.border))
                .child(if owner.library_filter.is_empty() { "Type to search / enter a folder name".to_string() } else { owner.library_filter.clone() }));
        if !owner.folder_create {
            let entries = owner.library.visible_entries(&owner.library_filter, owner.hide_hidden, owner.sort_descending);
            let selected = owner.library_selection.min(entries.len().saturating_sub(1));
            let mut results = div().id("search-results").min_h_0().max_h(px(420.)).overflow_y_scroll();
            for (index, entry) in entries.into_iter().take(20).enumerate() {
                let label = entry.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                results = results.child(div().id(("search-result", index)).cursor_pointer().p_3().mx_2().my_1().rounded_md()
                    .bg(rgb(if index == selected { palette.selected } else { palette.rail }))
                    .child(label).on_click(cx.listener(move |this, _, _, cx| {
                        this.search_open = false; this.library_filter.clear(); this.open_library_entry(&entry); cx.notify();
                    })));
            }
            content = content.child(results);
        }
        content = content.child(div().p_3().text_size(px(11.)).text_color(rgb(palette.muted)).child("Up / Down to select · Enter confirms · Escape cancels"));
        return Some(div().absolute().inset_0().p_6().bg(rgba(0x00000066)).flex().items_center().justify_center().child(content).into_any_element());
    }
    let menu = owner.home_menu?;
    let mut panel = div().absolute().top(px(48.)).right(px(120.)).w(px(240.)).p_2().rounded_xl()
        .bg(rgb(palette.rail)).border_1().border_color(rgb(palette.border)).shadow_lg().flex().flex_col().gap_1();
    match menu {
        Menu::New => {
            panel = panel.child(button("menu-new-document", "Document").justify_start().on_click(cx.listener(|this, _, _, cx| {
                this.home_menu = None; this.begin_document(true); cx.notify();
            }))).child(button("menu-new-folder", "Folder").justify_start().on_click(cx.listener(|this, _, _, cx| {
                if this.composition_ready() { this.home_menu = None; this.folder_create = true; this.search_open = false; this.library_filter.clear(); }
                cx.notify();
            }))).child(button("menu-open", "Open encrypted document...").justify_start().on_click(cx.listener(|this, _, _, cx| {
                this.home_menu = None; this.begin_document(false); cx.notify();
            })));
        }
        Menu::View => {
            panel = panel.child(div().px_2().py_1().text_size(px(11.)).text_color(rgb(palette.muted)).child("View"));
            for (id, label, grid) in [("menu-grid", "Grid", true), ("menu-list", "List", false)] {
                panel = panel.child(button(id, label).justify_start().bg(rgb(if owner.home_grid == grid { palette.selected } else { palette.rail }))
                    .on_click(cx.listener(move |this, _, _, cx| { this.home_grid = grid; this.home_menu = None; cx.notify(); })));
            }
            panel = panel.child(div().px_2().py_1().text_size(px(11.)).text_color(rgb(palette.muted)).child("Sort by · Folders first"))
                .child(button("menu-sort", if owner.sort_descending { "Title Z–A" } else { "Title A–Z" }).justify_start().on_click(cx.listener(|this, _, _, cx| {
                    this.sort_descending = !this.sort_descending; this.home_menu = None; cx.notify();
                })));
        }
        Menu::More => {
            panel = panel.child(button("menu-manage", "Manage workspace").justify_start().on_click(cx.listener(|this, _, _, cx| {
                this.home_menu = None; this.open_settings(cx); if this.settings_page.is_some() { this.settings_page = Some(3); } cx.notify();
            }))).child(button("menu-refresh", "Refresh folder").justify_start().on_click(cx.listener(|this, _, _, cx| {
                this.library.refresh(); this.home_menu = None; cx.notify();
            })));
        }
    }
    Some(panel.child(button("menu-dismiss", "Close menu").justify_start().on_click(cx.listener(|this, _, _, cx| { this.home_menu = None; cx.notify(); }))).into_any_element())
}
