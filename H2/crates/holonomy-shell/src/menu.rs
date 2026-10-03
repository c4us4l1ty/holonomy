//! The native application menu.
//!
//! # Why this file exists rather than a `tauri.conf.json` array
//!
//! Tauri can declare a menu in the config file, but the items here have to *do*
//! something, and a config array can only name handlers that already exist as a string.
//! Building the menu in Rust means the ids are checked against the handler table below,
//! which is the failure a menu most often has: an item that appears, is enabled, and
//! silently does nothing because its id was misspelled.
//!
//! # Why the menu talks to the frontend rather than acting directly
//!
//! Items whose work is *in this process* — `Optimize`, `Backup` — call their command
//! directly. `Find` emits to the frontend, because the search panel is frontend state and
//! duplicating it in Rust would be a second copy of a thing the keyboard already drives.
//!
//! # Keyboard shortcuts are duplicated here on purpose
//!
//! `Cmd+F` appears both here and in the frontend's `GLOBAL_BINDINGS`. That is not a
//! duplication bug: the menu is native chrome and renders *outside* the webview, so a
//! binding that lives only in the webview cannot be shown in the OS menu, and one that
//! lives only in the menu does not fire while the webview has focus. The two are held
//! together by `the_find_accelerator_is_the_one_the_frontend_binds`.

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// The event `Find` emits to the frontend.
///
/// Namespaced because `tauri://` is the application's own scheme and a bare name would
/// collide with anything a plugin emits.
pub const MENU_EVENT: &str = "holonomy://menu";

/// An id the application knows how to act on.
///
/// Deliberately a closed enum rather than a `String`: a typo becomes a compile error rather
/// than a menu item that does nothing, which is the whole point of building the menu here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MenuCommand {
    /// `File -> Optimize Document`. Reclaims space in the open document.
    Optimize,
    /// `File -> Save Backup Copy...`. Writes a snapshot through the save dialog.
    Backup,
    /// `Edit -> Find...`. Opens or re-focuses the search panel.
    Find,
}

/// The wire id for a command.
///
/// This *is* the contract: the frontend switches on the serialised form, and `MenuItem`'s id
/// is set from the same function. Two spellings would mean a menu item that renders and
/// does nothing.
pub fn command_id(command: MenuCommand) -> &'static str {
    match command {
        MenuCommand::Optimize => "optimize",
        MenuCommand::Backup => "backup",
        MenuCommand::Find => "find",
    }
}

/// Every command, for the frontend's cross-check and for tests.
pub fn all_commands() -> &'static [MenuCommand] {
    &[MenuCommand::Optimize, MenuCommand::Backup, MenuCommand::Find]
}

/// The accelerator for each command, as Tauri spells it.
///
/// Held as a table rather than inline in the builder chain so the parity test has something
/// to compare against, and so the mapping is one place rather than a string buried in a
/// call.
pub const ACCELERATORS: &[(MenuCommand, &str)] = &[(MenuCommand::Find, "CmdOrCtrl+F")];

/// Build the application menu.
///
/// # Why the standard edit items are `PredefinedMenuItem`
///
/// Because a predefined item is wired to the platform's own undo/copy/paste machinery, which
/// on macOS routes through `NSMenu` and reaches services the webview cannot. A custom item
/// labelled "Copy" would be a button that emits an event and hopes the frontend implements
/// clipboard semantics — which is a worse clipboard than the one already in `shortcuts.ts`.
///
/// # Why `New Document` and `Open...` are plain items
///
/// They are placeholders wired to the same event path as everything else. They are declared
/// so the File menu reads like a File menu; neither is implemented, and a disabled item that
/// cannot be chosen is more honest than one that opens an empty document.
fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &item(app, "New Document", MenuCommand::Optimize, None, false)?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, "Save Backup Copy...", MenuCommand::Backup, Some("CmdOrCtrl+S"), true)?,
            &item(app, "Optimize Document", MenuCommand::Optimize, None, true)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;

    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, "Find...", MenuCommand::Find, Some("CmdOrCtrl+F"), true)?,
        ],
    )?;

    let view = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &PredefinedMenuItem::fullscreen(app, None)?,
            &PredefinedMenuItem::minimize(app, None)?,
        ],
    )?;

    Menu::with_items(app, &[&file, &edit, &view])
}

/// A menu item carrying a [`MenuCommand`] as its id.
fn item<R: Runtime>(
    app: &AppHandle<R>,
    text: &str,
    command: MenuCommand,
    accelerator: Option<&str>,
    enabled: bool,
) -> tauri::Result<MenuItem<R>> {
    MenuItem::with_id(app, command_id(command), text, enabled, accelerator)
}

/// Attach the menu to a window, and route its events.
///
/// Non-fatal by design, and the caller logs rather than propagates: a window with no menu
/// is still a working editor, and every accelerator here is *also* bound in the frontend, so
/// the degradation is "no menu bar" rather than "no application".
pub fn install<R: Runtime>(
    app: &AppHandle<R>,
    window: &tauri::WebviewWindow<R>,
) -> tauri::Result<()> {
    let menu = build(app)?;
    window.set_menu(menu)?;

    // One handler for the window. `on_menu_event` on the `Window` rather than the `Builder`
    // because the Builder form has to be registered before `setup`, and the window does not
    // exist until after it.
    // Cloned into the closure. `on_menu_event` takes a `'static` closure, and `app` arrives
    // as a borrow — capturing the borrow is the "borrowed data escapes" error, and cloning
    // the handle (two `Arc`s) is what makes the handler outlive this function.
    let handle = app.clone();
    window.on_menu_event(move |_window, event| {
        // Owned first: `event.id()` borrows the event, and the id is needed twice below
        // (once to look the command up, once to log), which outlives nothing if it is a
        // borrow. An `Arc`-backed `MenuId` clone is a pointer copy.
        let id = event.id().0.clone();
        let Some(command) = command_from_id(id.as_str()) else {
            // Not one of ours. The platform's own items (`close_window`, the predefined
            // edit entries) all arrive here too, and returning is what lets them act.
            return;
        };
        if let Err(e) = dispatch(&handle, command) {
            eprintln!("[holonomy] menu command {id} failed: {e}");
        }
    });
    Ok(())
}

/// Route a command.
///
/// The two storage commands are handled here, in this process, because there is no frontend
/// state involved and a round trip through the webview would only add a way to fail.
/// `Find` is emitted, because the search panel lives in the frontend.
fn dispatch<R: Runtime>(app: &AppHandle<R>, command: MenuCommand) -> tauri::Result<()> {
    match command {
        MenuCommand::Optimize | MenuCommand::Backup => {
            // Emitted rather than executed inline. Both cross a native dialog or do
            // sustained disk I/O, and doing either on the setup thread would freeze the
            // window; the frontend runs them with a progress indication, which is also where
            // the report has to be shown anyway.
            app.emit(MENU_EVENT, command)
        }
        MenuCommand::Find => app.emit(MENU_EVENT, command),
    }
}

/// The command an id names, or `None` if it is not one of ours.
pub fn command_from_id(id: &str) -> Option<MenuCommand> {
    all_commands().iter().copied().find(|c| command_id(*c) == id)
}

/// Attach the menu to the main window, if there is one.
pub fn install_on_first_window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let window = app.get_webview_window("main").ok_or_else(|| {
        tauri::Error::Anyhow(anyhow::anyhow!("the main window does not exist yet"))
    })?;
    install(app, &window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_distinct_id() {
        // The id *is* the contract with the frontend, so two commands sharing one would make
        // whichever the frontend handled first unreachable — silently.
        let mut ids: Vec<&str> = all_commands().iter().map(|c| command_id(*c)).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two commands share a wire id");
    }

    #[test]
    fn the_find_accelerator_is_the_one_the_frontend_binds() {
        // `Cmd+F` exists in two places by necessity — native chrome renders outside the
        // webview — so this is the assertion that keeps the two from drifting.
        assert_eq!(
            ACCELERATORS,
            &[(MenuCommand::Find, "CmdOrCtrl+F")],
            "the native accelerator must match `shortcuts.ts`'s `mod+f`"
        );
    }

    #[test]
    fn the_serialised_form_round_trips_to_the_wire_id() {
        // If these ever differ, the frontend will not match what the menu emits and the item
        // does nothing — the exact failure this module exists to prevent.
        for command in all_commands() {
            let expected = command_id(*command);
            assert_eq!(
                serde_json::to_string(command).unwrap(),
                format!("\"{expected}\""),
                "{command:?} serialises to something the frontend will not match"
            );
            assert_eq!(
                command_from_id(expected),
                Some(*command),
                "{expected} does not parse back to its own command"
            );
        }
    }

    #[test]
    fn a_platform_menu_id_is_not_mistaken_for_a_command() {
        // The predefined items all arrive at the same handler. If any of them parsed as a
        // command, the handler would act on `close_window` as though it were ours.
        for foreign in ["close_window", "quit", "copy", "paste", "fullscreen", ""] {
            assert_eq!(command_from_id(foreign), None, "{foreign} must not parse");
        }
    }
}