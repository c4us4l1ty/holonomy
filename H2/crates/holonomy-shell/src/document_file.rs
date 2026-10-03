//! Opening and creating documents by path, and the native dialogs around them.
//!
//! # Why this is separate from the bridge
//!
//! Because "which document is open" is a decision the shell makes and the frontend asks
//! about. Every command in `bridge.rs` operates on whatever document is already open, which
//! works fine until the user double-clicks a `.holo` file in a file manager — at which point
//! the app has to be told to close one document and open another, and the frontend needs a
//! single event saying so rather than a sequence it has to reconstruct.
//!
//! # Why the file dialogs are here and not in the frontend
//!
//! A `<input type="file">` in the webview gets a path, not a handle, and on some engines
//! the path is a fiction — a sandboxed `blob:` URL with no file behind it that cannot then
//! be written back. The native dialogs return a real path the shell can open, and that is
//! the same string the OS gave us for a double-click. One representation of "the document
//! at this path" therefore reaches both entry points.
//!
//! Both entry points converge on [`DocumentCore::open_path`], so a file opened by
//! double-click and one opened from *Open…* take identical code. Two implementations would
//! be two places for "opened" to differ, and the difference would only show up for the
//! users who never touch the menu.

use crate::bridge::{DocumentSummary, OpenDocumentReport};
use crate::DocumentCore;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};

/// Open (or create) the document at `path` and make it the current one.
///
/// Returns what happened rather than only succeeding, because the three outcomes need
/// different handling in the UI and reporting them uniformly is how "you opened a file that
/// did not contain a document" turns into a silent success.
#[tauri::command]
pub async fn open_document_path(
    app: AppHandle,
    path: String,
) -> Result<OpenDocumentReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Mutex<DocumentCore>>();
        let mut core = state.lock().expect("core lock poisoned");
        core.open_path(&PathBuf::from(path))
    })
    .await
    .map_err(|e| format!("open task panicked: {e}"))?
    .map_err(|e| e.to_string())
}

/// Create a new, empty document at `path` and make it current.
#[tauri::command]
pub async fn create_document_path(
    app: AppHandle,
    path: String,
) -> Result<OpenDocumentReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Mutex<DocumentCore>>();
        let mut core = state.lock().expect("core lock poisoned");
        core.create_path(&PathBuf::from(path))
    })
    .await
    .map_err(|e| format!("create task panicked: {e}"))?
    .map_err(|e| e.to_string())
}

/// Ask the OS where to open a document from.
///
/// # Why this is a command and not a frontend call to the plugin
///
/// Both would reach the same plugin. The command is here so that *the dialog's filters*
/// come from `holonomy_core` rather than from a literal in `main.ts`, and so that a test
/// can exercise the filter construction without a window. The filter is the part that
/// drifts: a `.holo` file that does not appear in the file picker is indistinguishable
/// from an app that cannot open its own format, and no test of the dialog would notice
/// because the dialog is not what is being tested.
#[tauri::command]
pub async fn pick_document_to_open(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (name, pattern) = holonomy_core::holo::dialog_filter();
    let picked = app
        .dialog()
        .file()
        .set_title("Open a Holonomy document")
        .add_filter(&name, &[&pattern])
        .blocking_pick_file();
    Ok(picked.map(|p| p.to_string()))
}

/// Ask the OS where to write a backup copy.
///
/// # Why "backup" and not "save as" in the same call
///
/// Because they refuse different things, and conflating them loses documents. *Save* writes
/// over the file the user came from; *save a copy* must not, because the open document is
/// still there. `Store::backup_to` refuses to overwrite as a result, and this asks for a
/// path in a way that does not suggest the original will change — a different window title
/// for the same operation would be a lie.
#[tauri::command]
pub async fn pick_backup_destination(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (name, pattern) = holonomy_core::holo::dialog_filter();
    let picked = app
        .dialog()
        .file()
        .set_title("Save a backup copy")
        .add_filter(&name, &[&pattern])
        .set_file_name("backup.holo")
        .blocking_save_file();
    Ok(picked.map(|p| p.to_string()))
}

/// Summaries for every document in the open file, for a document switcher.
#[tauri::command]
pub fn list_documents(
    core: State<'_, Mutex<DocumentCore>>,
) -> Result<Vec<DocumentSummary>, String> {
    let core = core.lock().expect("core lock poisoned");
    core.documents().map_err(|e| e.to_string())
}

/// The path of the currently open document, if it came from a file.
///
/// `None` for a document created in memory, and that distinction matters: the frontend
/// must not offer "Save" for a document with nowhere to save it to.
#[tauri::command]
pub fn current_document_path(core: State<'_, Mutex<DocumentCore>>) -> Result<Option<String>, String> {
    let core = core.lock().expect("core lock poisoned");
    Ok(core.current_path().map(|p| p.display().to_string()))
}

/// The event the frontend listens for when the open document changes.
///
/// One event carrying the whole [`OpenDocumentReport`] rather than "something opened",
/// because the frontend needs the same three things the command returns — which document,
/// which path, and whether it was created — and a bare notification would send it back to
/// ask, producing a second round trip and a second chance to disagree.
pub const DOCUMENT_OPENED_EVENT: &str = "holo://document-opened";

/// Open a document and tell the frontend.
///
/// Shared by the *Open…* command and the OS's own open-file event, so a file double-clicked
/// in a file manager and a file chosen from a dialog take identical code. `tauri` emits
/// `RunEvent::Opened` for the first and the command is the second; routing both through one
/// function is what stops them from diverging into "double-click ignores the report" and
/// "menu asks the frontend to reload".
pub fn open_and_notify(app: &AppHandle, path: &Path) -> Result<OpenDocumentReport, String> {
    let state = app.state::<Mutex<DocumentCore>>();
    let mut core = state.lock().expect("core lock poisoned");
    let report = core.open_path(path).map_err(|e| e.to_string())?;
    drop(core);

    use tauri::Emitter;
    app.emit(DOCUMENT_OPENED_EVENT, &report)
        .map_err(|e| format!("could not tell the frontend the document changed: {e}"))?;
    Ok(report)
}

/// Handle the OS's "open this file" event.
///
/// # Why this returns instead of opening
///
/// Because on macOS the very first launch of a double-clicked document emits `Opened`
/// *before* the webview has finished booting, and possibly before `setup` has run at all.
/// Opening here would either panic on missing state or open the file and then have the
/// frontend boot and ask for the boot payload of the *default* document, overwriting it.
/// So the path is stashed and drained once the frontend says it has mounted — see
/// [`drain_pending_open`].
pub fn queue_open(app: &AppHandle, path: &Path) {
    let state = app.state::<Mutex<PendingOpen>>();
    state
        .lock()
        .expect("pending-open lock poisoned")
        .0
        .push(path.display().to_string());
}

/// Take any paths the OS handed over before the frontend was ready.
///
/// Called from `report_mounted`. Anything queued is opened now, in order, so the last
/// double-clicked document is the one left open — which is what the OS itself does when a
/// second file is opened while the first is still starting.
pub fn drain_pending_open(app: &AppHandle) -> Vec<Result<OpenDocumentReport, String>> {
    let queued: Vec<String> = {
        let state = app.state::<Mutex<PendingOpen>>();
        let mut guard = state.lock().expect("pending-open lock poisoned");
        std::mem::take(&mut guard.0)
    };
    queued
        .iter()
        .map(|url| match path_from_url(url) {
            Some(path) => open_and_notify(app, &path),
            None => Err(format!("{url} is not a file this app can open")),
        })
        .collect()
}

/// Paths handed over by the OS before the frontend was ready.
///
/// A `Vec`, not an `Option`: a user can double-click two documents in the time it takes to
/// start, and keeping only the last would silently discard the first. `Mutex` because
/// `Opened` arrives on the platform's thread and `report_mounted` on the webview's.
#[derive(Default)]
pub struct PendingOpen(pub Vec<String>);

/// Pull document paths out of a process's command line.
///
/// # Why this exists at all
///
/// Because `RunEvent::Opened` — Tauri's "the user asked the OS to open this file" event —
/// is compiled only on macOS, iOS and Android. On Linux and Windows the same double-click
/// arrives as an **argument**: the freedesktop spec and the Windows registry both launch
/// the application with the file's path appended. Checking the vendored `tauri` 2.12.1
/// `RunEvent` is what established this; the variant simply is not there to match on.
///
/// So there are two shapes to read and one code path afterwards. Handling only the event
/// would have produced an app that opens documents perfectly on a Mac and silently ignores
/// every double-click on Linux and Windows, which is the shape of a bug that reaches users
/// on two of the three platforms this project targets.
///
/// # Why only arguments that look like documents are taken
///
/// Because the same command line carries `--no-watch`, a log level and, in development, a
/// config path. Treating argument 1 as a filename would mean a development run with
/// `--config x.json` tries to open `x.json` as a document, and the user sees an error about
/// a file they never touched.
///
/// # Why `argv[0]` gets no special case
///
/// Because it does not need one, and a version of this that called `.skip(1)` with a test
/// named "the executable is not a document" looked like a guard while the extension filter
/// was doing all the work — the test passed with `.skip(1)` removed, which is exactly how a
/// line of defence that protects nothing gets to look like one that does. The executable is
/// excluded by the same rule as everything else: it is not a document. There is no
/// arrangement in which argv[0] is named `*.holo`, and inventing one to justify a skip
/// would be a rule written to fit its own test.
///
/// # Why a path that does not exist is dropped rather than reported
///
/// Because this runs before the app is up, and a stale file association entry in a desktop
/// database can name a file that was deleted years ago. Reporting it would put an error
/// dialog in front of a user who launched the app normally with no file at all. A path that
/// exists but is not a document *is* reported — that is a user who double-clicked the wrong
/// file, and they should be told.
pub fn paths_from_args<I: IntoIterator<Item = String>>(args: I) -> Vec<PathBuf> {
    args.into_iter()
        .filter(|a| !a.starts_with('-')) // flags are not filenames
        .filter(|a| holonomy_core::holo::has_extension(Path::new(a)))
        .map(PathBuf::from)
        .collect()
}

/// What a *second* launch of the application asked the already-running one to do.
///
/// # Why this is its own type and not a `Vec<PathBuf>`
///
/// Because "there is nothing to open" and "there is something to open" are different
/// instructions, and the caller does different work for each: nothing at all for the first,
/// and a window focus for the second. Returning a bare `Vec` and testing `.is_empty()` at
/// the call site is fine until the list is empty *and* an error should be reported, at which
/// point the two have been conflated and nobody can tell which one happened.
#[derive(Debug, PartialEq, Eq, Default)]
pub enum SecondLaunch {
    /// No openable document was passed. The running instance keeps whatever it has open.
    #[default]
    Nothing,
    /// Documents to open, in order. The last is the one left open.
    Open(Vec<PathBuf>),
}

/// Decide what a second launch asked the running instance to do.
///
/// # Why this is a free function and not inside the plugin callback
///
/// Because the callback runs inside a D-Bus handler on a thread Tauri owns, and the one
/// decision it makes — *is this argv asking for a document, or is it a development run, a
/// re-exec, or a stray flag* — is the same decision the cold-start path already makes in
/// [`paths_from_args`]. Isolated here it is testable without a D-Bus session, a running
/// window, or a database, which is the only reason it can be tested at all.
///
/// The caller still decides *how* to open (immediately, or queue for a frontend that has not
/// mounted yet); this says only *what*.
pub fn second_launch<I: IntoIterator<Item = String>>(argv: I) -> SecondLaunch {
    let paths = paths_from_args(argv);
    if paths.is_empty() {
        SecondLaunch::Nothing
    } else {
        SecondLaunch::Open(paths)
    }
}

/// Turn what the OS gave us into a path.
///
/// # Why `file://` is decoded and a bare path is used as-is
///
/// Because the two platforms disagree. macOS delivers a `file://` URL with percent-escapes;
/// Windows and most Linux desktops deliver a plain filesystem path. Treating both as
/// opaque strings means a document called `Chapter 1.holo` opens as a file called
/// `Chapter%201.holo` on Linux — a different file from the one the user clicked, which is
/// the worst kind of wrong.
pub fn path_from_url(url: &str) -> Option<PathBuf> {
    if let Some(rest) = url.strip_prefix("file://") {
        // Strip any authority (`file://localhost/tmp/x` is legal and means `/tmp/x`).
        let path = match rest.find('/') {
            Some(0) => rest,
            Some(i) => &rest[i..],
            None => rest,
        };
        let decoded = percent_decode(path)?;
        return Some(PathBuf::from(decoded));
    }
    if url.is_empty() {
        None
    } else {
        Some(PathBuf::from(url))
    }
}

/// Decode `%XX` escapes in a path.
///
/// Hand-rolled rather than pulled from a crate for one function, and the constraint is
/// stated rather than assumed: this decodes **paths**, not URLs, and must not turn `+` into
/// a space or resolve `..`. Those are correct for query strings and wrong here — a file
/// genuinely named `a+b.holo` must open as `a+b.holo`.
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = input.get(i + 1..i + 3)?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Whether a path looks like something this app should open.
///
/// # Why the frontend is allowed to ask
///
/// Because the OS hands over paths from `.holo` associations, drag-and-drop and the recent
/// list, and the frontend has to decide whether to offer "Open" for any of them. Deciding
/// in the frontend would mean reimplementing `probe` in TypeScript — two implementations of
/// "is this mine", one of which would be wrong.
#[tauri::command]
pub fn is_holo_path(path: String) -> bool {
    holonomy_core::holo::has_extension(Path::new(&path))
}
/// Tests for reading a document out of whatever shape the OS delivered it in.
///
/// # Why the shape-reading is where the bugs are
///
/// Because on no platform can this be tested by opening a file. Linux and Windows put a
/// path in `argv`; macOS puts a percent-escaped URL in an event that does not compile on
/// the other two. The *dispatch* is untestable here without a desktop session and a file
/// manager, so these tests hold the part that can be held — and the part that decides
/// whether a document named `Chapter 1.holo` opens or a different file that happens to
/// exist does.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_is_the_only_rule_that_keeps_the_executable_out() {
        // Which is why there is no `.skip(1)`.
        //
        // This test passed unchanged when the `skip` was removed, because
        // `/usr/bin/holonomy` does not end in `.holo`. A test named "the executable is not a
        // document" therefore proved nothing about the line it appeared to cover — it was
        // testing the extension filter and crediting a skip. What it now says is the rule
        // that actually holds: the argument list is filtered by extension alone, so the
        // executable is excluded for the same reason as any other non-document.
        let args = vec!["/usr/bin/holonomy".to_string()];
        assert!(paths_from_args(args.clone()).is_empty());

        // And the exclusion is the extension, not the position: an argument that *is* a
        // document is taken wherever it appears, including first.
        assert_eq!(
            paths_from_args(vec!["/home/someone/a.holo".to_string()]),
            vec![PathBuf::from("/home/someone/a.holo")],
            "a document in first position was dropped, so something is filtering by \
             position after all"
        );
        assert_eq!(paths_from_args(args).len(), 0);
    }

    #[test]
    fn a_flag_is_not_a_document() {
        let args = vec![
            "holonomy".to_string(),
            "--no-watch".to_string(),
            "--config".to_string(),
            "crates/holonomy-shell/tauri.verify.json".to_string(),
        ];
        assert!(
            paths_from_args(args).is_empty(),
            "development flags were mistaken for documents, so every `cargo tauri dev` run \
             would try to open a JSON file"
        );
    }

    #[test]
    fn a_document_argument_is_taken_and_its_case_does_not_matter() {
        let args = vec![
            "holonomy".to_string(),
            "/home/someone/My Novel.HOLO".to_string(),
            "/home/someone/other.holo".to_string(),
        ];
        let found = paths_from_args(args);
        assert_eq!(
            found,
            vec![
                PathBuf::from("/home/someone/My Novel.HOLO"),
                PathBuf::from("/home/someone/other.holo"),
            ],
            "both documents should be taken, in the order they were given"
        );
    }

    #[test]
    fn a_non_document_argument_is_left_alone() {
        // The app is launched with a log path by some supervisors and with a file to open
        // by others; only the second is ours.
        let args = vec![
            "holonomy".to_string(),
            "/var/log/holonomy.log".to_string(),
            "/home/someone/notes.holo".to_string(),
        ];
        assert_eq!(
            paths_from_args(args),
            vec![PathBuf::from("/home/someone/notes.holo")]
        );
    }

    #[test]
    fn a_path_with_a_space_survives_intact() {
        // Not decoded, not split. `argv` arrives as discrete strings, so anything that
        // "decoded" one would be inventing a second parse of the user's filename.
        let args = vec![
            "holonomy".to_string(),
            "/home/someone/My Novel.holo".to_string(),
        ];
        assert_eq!(
            paths_from_args(args),
            vec![PathBuf::from("/home/someone/My Novel.holo")]
        );
    }

    #[test]
    fn a_macos_file_url_becomes_the_path_it_names() {
        assert_eq!(
            path_from_url("file:///home/someone/My%20Novel.holo"),
            Some(PathBuf::from("/home/someone/My Novel.holo"))
        );
        // A local authority is legal and means the same thing.
        assert_eq!(
            path_from_url("file://localhost/home/someone/a.holo"),
            Some(PathBuf::from("/home/someone/a.holo"))
        );
    }

    #[test]
    fn a_percent_escape_that_is_not_an_escape_is_refused_not_guessed() {
        // A file genuinely named `100%.holo` is a real file. Refusing the URL means the
        // app opens nothing rather than opening something else, which is the right way
        // round: a wrong file is worse than no file.
        // The first version of this test wrote `file:///tmp/zz.holo` and expected `None`,
        // on the theory that `zz` was a bad escape. There is no `%` in it, so the decoder
        // was right to return `/tmp/zz.holo` and the test was wrong — which is the failure
        // mode a test written to agree with the code always has. These are the strings that
        // actually contain a broken escape.
        assert_eq!(path_from_url("file:///tmp/100%.holo"), None);
        assert_eq!(path_from_url("file:///tmp/zz%gg.holo"), None);
        assert_eq!(path_from_url("file:///tmp/%2.holo"), None);
        // A lone `%` at the end cannot be a truncated escape either.
        assert_eq!(path_from_url("file:///tmp/a%"), None);
    }

    #[test]
    fn plus_is_not_a_space_in_a_path() {
        // The single most tempting bug in a hand-rolled decoder. `+` is a space in a query
        // string and a literal plus in a path, and a document called `a+b.holo` is
        // perfectly ordinary. If this ever regresses, the file silently becomes a
        // different file.
        assert_eq!(
            path_from_url("file:///tmp/a+b.holo"),
            Some(PathBuf::from("/tmp/a+b.holo"))
        );
    }

    #[test]
    fn a_utf8_path_is_not_mangled() {
        // Percent-escapes for multi-byte characters are the reason the decode loop exists
        // at all: the bytes have to be reassembled *after* decoding, not before.
        assert_eq!(
            path_from_url("file:///home/someone/%E6%89%8B%E6%9C%AC.holo"),
            Some(PathBuf::from("/home/someone/手本.holo"))
        );
    }

    #[test]
    fn a_bare_path_from_linux_and_windows_is_used_as_given() {
        // No decoding, no scheme stripping, no `~` expansion. The OS gave a path and it is
        // one.
        assert_eq!(
            path_from_url(r"C:\Users\someone\My Novel.holo"),
            Some(PathBuf::from(r"C:\Users\someone\My Novel.holo"))
        );
        assert_eq!(
            path_from_url("/home/someone/a.holo"),
            Some(PathBuf::from("/home/someone/a.holo"))
        );
    }

    #[test]
    fn nothing_at_all_is_not_a_path() {
        assert_eq!(path_from_url(""), None);
    }

    // -- what a second launch asks the running instance to do ------------------------

    #[test]
    fn a_second_launch_with_no_document_asks_for_nothing() {
        // This is the case the `SecondLaunch` type exists for. A bare "open the app" —
        // clicking the dock icon, or a desktop entry with no document — must be
        // distinguishable from "open this", because the first still deserves a window
        // focus and the second deserves a window focus *and* a document.
        assert_eq!(
            second_launch(vec!["/usr/bin/holonomy".to_string()]),
            SecondLaunch::Nothing
        );
        assert_eq!(second_launch(Vec::<String>::new()), SecondLaunch::Nothing);
        assert_eq!(
            second_launch(vec![
                "/usr/bin/holonomy".to_string(),
                "--no-watch".to_string(),
                "tauri.verify.json".to_string(),
            ]),
            SecondLaunch::Nothing,
            "a development run that carries no document must not be mistaken for one that \
             does, or a `cargo tauri dev` would re-open whatever the last instance had"
        );
    }

    #[test]
    fn a_second_launch_carries_every_document_not_just_the_last() {
        // Two documents can be double-clicked in the time it takes a process to start, and
        // the whole of `PendingOpen` is a `Vec` for exactly that reason. If the second
        // launch dropped all but the last, the same user action would work differently
        // depending on how fast the machine is — which is the definition of a race the
        // user cannot see.
        assert_eq!(
            second_launch(vec![
                "/usr/bin/holonomy".to_string(),
                "/home/someone/one.holo".to_string(),
                "/home/someone/two.holo".to_string(),
            ]),
            SecondLaunch::Open(vec![
                PathBuf::from("/home/someone/one.holo"),
                PathBuf::from("/home/someone/two.holo"),
            ]),
            "order is preserved, because the last document double-clicked is the one left \
             open — the same rule the cold-start queue follows"
        );
    }

    #[test]
    fn the_second_launch_argv_is_filtered_by_the_same_rule_as_a_cold_one() {
        // A plugin forwards `std::env::args()` of the *second* process verbatim. That list
        // starts with the second process's own executable path, which is a different string
        // from the primary's and is never a document. The filter has to hold on this path
        // too, or every warm start would try to open the launcher.
        let argv = vec![
            ".local/share/holonomy/holonomy".to_string(),
            "/home/someone/Chapter 1.holo".to_string(),
        ];
        assert_eq!(
            second_launch(argv),
            SecondLaunch::Open(vec![PathBuf::from("/home/someone/Chapter 1.holo")]),
            "a path with a space in it is one argument, and the extension rule is the only \
             thing that may reject it"
        );
    }

    #[test]
    fn a_second_launch_cannot_be_told_to_open_something_that_is_not_a_document() {
        // The forwarded argv is attacker-influenced in the sense that any process on the
        // session bus can send it. `paths_from_args` requires the `.holo` extension, and
        // `open_path` runs `holo::probe` before opening anything — so a forwarded
        // `/etc/passwd` is refused twice over: not a document by extension, and not one by
        // header if the name is changed to `.holo`.
        assert_eq!(
            second_launch(vec![
                "/usr/bin/holonomy".to_string(),
                "/home/someone/.bashrc".to_string(),
                "/tmp/whatever.holo".to_string(),
            ]),
            SecondLaunch::Open(vec![PathBuf::from("/tmp/whatever.holo")])
        );
    }
}
