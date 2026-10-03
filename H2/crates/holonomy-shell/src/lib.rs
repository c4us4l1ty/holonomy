//! The Holonomy desktop shell.
//!
//! # Scope, deliberately narrow
//!
//! This milestone is a **window**, not a bridge. Decision 5 of the current
//! round is explicit: scaffold the shell, run the M4 scroll and compensation
//! tests against a real engine, and only then wire the `GeometryBridge` IPC.
//!
//! That ordering is worth defending, because the alternative — building the bridge
//! against the headless browser harness and discovering afterwards that webkit2gtk
//! behaves differently — is the trap this project has already paid for once.
//! Every number in `M0`/`M1`/`M4` comes from Chromium. The strategy they justify is
//! unverified on the engine that actually ships on Linux. This shell is the thing
//! that closes that gap, and it is smaller than the bridge.
//!
//! # What is here
//!
//! - The Tauri app and its window, pointed at the same Vite server the browser
//!   harness uses, so there is one frontend and two hosts.
//! - A health command, so the window can report which engine it is running on.
//!   That is not diagnostic sugar: the M4 verification needs to record the engine
//!   per run, and webkit2gtk, WKWebView and WebView2 lay text out differently.
//! - An explicit engine-identity command, because "the tests passed" is not a
//!   result unless you know which renderer produced them.
//!
//! # What is deliberately absent
//!
//! No store, no document loading, no bridge commands. The browser harness supplies
//! its own synthetic document; until the bridge exists the shell does the same.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State};

use crate::bridge::{DocumentSummary, OpenDocumentReport};

/// The Rust ⇄ TypeScript bridge contract, and the source of the generated types
/// the frontend imports. See the module docs for why these are defined once here
/// rather than paired with hand-written TypeScript.
pub mod bridge;
pub mod document_file;

/// PDF export: the Typst compilation environment and the JSON-to-Typst translator.
///
/// See `export::world` for why Holonomy supplies its own `typst::World` rather than one off
/// the shelf -- in short, a document is user data that arrives by sync, so the set of files it
/// can name must be exactly the set of assets this application chose to store.
pub mod export;

/// The bridge commands' logic, as pure functions over a store. See the module docs
/// for why the decisions live here and the `#[tauri::command]` wrappers do not.
pub mod core;
pub mod menu;

/// Which webview this window is running on.
///
/// Reported by a command rather than logged, because the cross-engine
/// verification has to attribute every measurement to a renderer and a
/// measurement without that attribution is not comparable.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Engine {
    /// webkit2gtk on Linux.
    WebKitGtk,
    /// WKWebView on macOS and iOS.
    WkWebView,
    /// WebView2 (Edge/Chromium) on Windows.
    WebView2,
}

impl Engine {
    /// The engine for the target this binary was compiled for.
    const fn current() -> Self {
        #[cfg(target_os = "linux")]
        {
            Engine::WebKitGtk
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            Engine::WkWebView
        }
        #[cfg(target_os = "windows")]
        {
            Engine::WebView2
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", target_os = "windows")))]
        {
            compile_error!("Holonomy targets Linux, macOS, iOS and Windows; no engine known for this platform")
        }
    }

    fn name(self) -> &'static str {
        match self {
            Engine::WebKitGtk => "webkit2gtk",
            Engine::WkWebView => "WKWebView",
            Engine::WebView2 => "WebView2",
        }
    }
}

/// State managed by Tauri and shared across commands.
///
/// Holds the store and the geometry, both behind a mutex, because a `rusqlite`
/// `Connection` is not `Sync` and Tauri requires managed state to be.
///
/// # Why one mutex and not one per field
///
/// A `Store` and a `Geometry` are never both needed for the same operation, so
/// separate locks would be more parallel. They are also never *safely* separate:
/// `commit_section_lifecycle` writes to the store and then rebuilds the geometry
/// from what it wrote, and two commands doing that concurrently could interleave so
/// the geometry described a manifest that no longer exists. One lock makes that
/// impossible, and the operations are milliseconds apart.
pub struct DocumentCore {
    /// The document store. Opened once at startup and held for the process lifetime:
    /// SQLite is faster with a warm page cache, and reopening per command would
    /// throw that away.
    store: Mutex<holonomy_core::Store>,
    /// The geometric index, seeded from the manifest at boot and corrected by
    /// `sync_section_heights`. Lives in Rust so the shell can answer "how tall is
    /// this document" without a round trip on the scroll path.
    geometry: Mutex<holonomy_core::Geometry>,
    /// The document currently open, so a command with no id opens the same one.
    document_id: Mutex<Option<String>>,
    /// Set once the window reports it has mounted, so the shell can distinguish
    /// "the frontend crashed on boot" from "the frontend never started".
    mounted: Mutex<Option<Engine>>,
    /// The document file backing `store`.
    ///
    /// Held because the frontend asks "where am I?" to label the title bar and to decide
    /// whether to offer *Save* at all — a document in memory has nowhere to be saved, and
    /// offering a menu item that then fails is worse than not offering it. The path cannot
    /// be recovered from the `Store`, because SQLite does not remember what it was opened
    /// under.
    path: Mutex<Option<PathBuf>>,
}

impl DocumentCore {
    /// Open the document store, creating it if this is the first run.
    ///
    /// The path comes from Tauri's app-data directory rather than a constant, so the
    /// database lands where the platform expects a user's data and survives an
    /// uninstall-and-reinstall of the *app* without taking documents with it.
    ///
    /// This is the *default* store — the one a session starts with when the user has not
    /// named a file. It is a `.holo` like any other, so the two entry points below can
    /// adopt it with no special case: opening the default path by hand and letting the app
    /// start on it are the same operation.
    pub fn new(app: &tauri::AppHandle) -> Result<Self, String> {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("no app data directory: {e}"))?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let path = dir.join("holonomy.holo");
        let store = holonomy_core::Store::open(&path)
            .map_err(|e| format!("could not open the store: {e}"))?;
        Ok(Self {
            store: Mutex::new(store),
            geometry: Mutex::new(holonomy_core::Geometry::new()),
            document_id: Mutex::new(None),
            mounted: Mutex::new(None),
            path: Mutex::new(Some(path)),
        })
    }

    /// Record that the frontend mounted.
    pub fn set_mounted(&mut self, engine: Engine) {
        *self.mounted.lock().expect("mount state poisoned") = Some(engine);
    }

    /// Whether the frontend has reported a mount yet.
    pub fn is_mounted(&self) -> bool {
        self.mounted.lock().expect("mount state poisoned").is_some()
    }

    /// The file the current store was opened from.
    pub fn current_path(&self) -> Option<PathBuf> {
        self.path.lock().expect("path lock poisoned").clone()
    }

    /// Adopt the document file at `path`, switching away from whatever is open.
    ///
    /// # Why this is one lock, taken once
    ///
    /// The store, the geometry and the document id are three `Mutex`es that must move
    /// together: a document id recorded against the previous file's geometry is a state no
    /// command can interpret, and the frontend would have to survive it. Taking each in
    /// turn would allow a command to observe the mixture. So all three are updated under
    /// `self.core`, the lock `DocumentCore` carries for exactly this.
    ///
    /// # Why the previous document is flushed first
    ///
    /// Because switching files is the one moment where work in flight becomes unreachable:
    /// the store that holds it is about to be dropped. An unflushed WAL row is not lost —
    /// the *old* file still has it, and will fold it on its own next open — but the user
    /// was looking at an old document and would see their last few seconds disappear, with
    /// the file itself correct. Folding first is a few milliseconds and it is what makes the
    /// switch honest.
    pub fn open_path(&mut self, path: &std::path::Path) -> Result<OpenDocumentReport, holonomy_core::Error> {
        // Fold and checkpoint *before* swapping, while the old store is still open. After
        // the swap the old `Store` is dropped and there is nothing left to flush through.
        {
            let store = self.store.lock().expect("store lock poisoned");
            let _ = store.flush_all();
            let _ = store.checkpoint_truncate();
        }

        let kind = holonomy_core::holo::probe(path)?;
        let existed = matches!(kind, holonomy_core::FileKind::Holonomy(_));
        let store = holonomy_core::Store::open(path)?;

        let report = match store
            .documents()
            ?
            .into_iter()
            .next()
        {
            Some(doc) => OpenDocumentReport {
                document: summary_of(&store, &doc),
                path: path.display().to_string(),
                kind: if existed { "existing" } else { "created" }.to_string(),
            },
            None => {
                // An openable file with nothing in it. Creating a document here rather than
                // reporting an error is what makes "open an empty .holo" work, and the `empty`
                // kind is what stops the UI claiming it opened something that was there.
                let doc = store
                    .create_document("Untitled")
                    ?;
                OpenDocumentReport {
                    document: summary_of(&store, &doc),
                    path: path.display().to_string(),
                    // The created/existing half survives even when the document half is
                    // `empty`. The first version of this hardcoded `"empty"` here and a
                    // newly created file reported `empty` — which is true and useless,
                    // because "created" is the half the user is waiting to hear.
                    kind: if existed { "empty" } else { "created" }.to_string(),
                }
            }
        };

        *self.store.lock().expect("store lock poisoned") = store;
        *self.geometry.lock().expect("geometry lock poisoned") = holonomy_core::Geometry::new();
        *self.document_id.lock().expect("document id lock poisoned") =
            Some(report.document.id.clone());
        *self.path.lock().expect("path lock poisoned") = Some(path.to_path_buf());

        Ok(report)
    }

    /// Create a new file at `path` and open it.
    ///
    /// # Why this refuses an existing file
    ///
    /// Because "Save As…" onto a path that already exists is how a document is destroyed.
    /// The native save dialog asks first — but the OS's answer is not something this
    /// process can rely on having been asked at all (a document opened from the command
    /// line has no dialog behind it), so the refusal is made here where it cannot be
    /// skipped. Overwriting is then possible only through an explicit call that says so.
    pub fn create_path(
        &mut self,
        path: &std::path::Path,
    ) -> Result<OpenDocumentReport, holonomy_core::Error> {
        match holonomy_core::holo::probe(path)? {
            holonomy_core::FileKind::Absent => {}
            // Not an error to *refuse* on the grounds that it is the wrong kind of file —
            // the file is fine, the action is wrong.
            other => {
                return Err(holonomy_core::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{} already exists ({other:?})", path.display()),
                )))
            }
        }

        {
            let store = self.store.lock().expect("store lock poisoned");
            let _ = store.flush_all();
            let _ = store.checkpoint_truncate();
        }

        let store = holonomy_core::Store::open(path)?;
        let doc = store
            .create_document("Untitled")
            ?;
        let report = OpenDocumentReport {
            document: summary_of(&store, &doc),
            path: path.display().to_string(),
            kind: "created".to_string(),
        };

        *self.store.lock().expect("store lock poisoned") = store;
        *self.geometry.lock().expect("geometry lock poisoned") = holonomy_core::Geometry::new();
        *self.document_id.lock().expect("document id lock poisoned") =
            Some(report.document.id.clone());
        *self.path.lock().expect("path lock poisoned") = Some(path.to_path_buf());

        Ok(report)
    }

    /// Summaries for every document in the open file.
    pub fn documents(&self) -> Result<Vec<DocumentSummary>, holonomy_core::Error> {
        let store = self.store.lock().expect("store lock poisoned");
        store
            .documents()
            .map(|docs| docs.iter().map(|d| summary_of(&store, d)).collect())
    }
}

/// A `Document` plus its manifest counts, as the switcher needs it.
///
/// # Why this takes the store and not a `Document`
///
/// Because `Document` carries no counts, and a switcher row that says "Untitled" with no
/// size is a row that cannot be told from "Untitled (empty)" — which is a document the user
/// has already deleted the contents of and wants back. The manifest read is per-row and
/// cheap; it is the *manifest* that is the 2000-page document's expensive part, and this
/// reads it per document rather than per section.
fn summary_of(store: &holonomy_core::Store, doc: &holonomy_core::Document) -> DocumentSummary {
    let counts = store
        .manifest(&doc.id)
        .map(|m| (m.totals().sections, m.totals().words as u32))
        .unwrap_or((0, 0));
    let (sections, words) = counts;
    DocumentSummary {
        id: doc.id.clone(),
        title: doc.title.clone(),
        sections,
        words,
        updated_at: doc.updated_at,
    }
}

/// What the frontend gets back when it reports a successful mount.
#[derive(Debug, Serialize)]
pub struct MountReport {
    engine: &'static str,
    /// The engine's version string, when the platform exposes one.
    version: Option<String>,
    ok: bool,
}

/// Report that the frontend mounted, and which engine it mounted into.
///
/// Called by the webview once the editor surface is live. Until it is called,
/// nothing else in the shell can distinguish a slow boot from a failed one.
#[tauri::command]
fn report_mounted(
    app: tauri::AppHandle,
    core: State<'_, Mutex<DocumentCore>>,
    version: Option<String>,
) -> MountReport {
    let engine = Engine::current();
    core.lock()
        .expect("core lock poisoned")
        .set_mounted(engine);

    // Anything the OS asked for before the webview was up. On macOS a double-clicked
    // document produces `Opened` before the first frame, so the frontend would otherwise
    // boot onto the default document and immediately be told to replace it — with the
    // default's contents already in the editor for one frame.
    let opened = document_file::drain_pending_open(&app);
    for outcome in &opened {
        if let Err(e) = outcome {
            eprintln!("[holonomy] could not open a file handed over by the OS: {e}");
        }
    }
    // To stderr, not just in memory: `cargo run` shows it in the terminal, which
    // is how a headless verification knows the frontend came up at all. A blank
    // window and a slow boot are indistinguishable from outside, and the engine
    // attribution has to reach a record somewhere.
    eprintln!(
        "[holonomy] frontend mounted on {} (ua version {:?})",
        engine.name(),
        version
    );
    MountReport { engine: engine.name(), version, ok: true }
}

/// Which engine this window is using.
#[tauri::command]
fn which_engine() -> &'static str {
    platform_engine()
}

/// The engine this binary was compiled for, as the string the parity job compares.
///
/// # Why this is `pub`
///
/// `Engine::current` and `Engine::name` are private, which was correct until the macOS and
/// Windows legs needed *their own* engine asserted rather than assumed. A test that could
/// only be written on Linux would have told us nothing about the two legs that were still
/// marked `[INFERENCE]`; `tests/platform-parity.rs` runs on all three and checks this
/// against `std::env::consts::OS`, so the macOS leg asserts WKWebView and the Windows leg
/// asserts WebView2, on the machines where that is the claim being made.
///
/// `which_engine` is a thin wrapper rather than the definition, so the command the frontend
/// calls and the function the test calls cannot drift.
pub fn platform_engine() -> &'static str {
    Engine::current().name()
}

/// Whether this process was started to verify itself.
///
/// # Why this is a command and not a URL
///
/// The first version navigated the window to `tauri://localhost/index.html?verify=1`, which
/// is how the *dev* build enters verification and is the **macOS** production origin. Linux
/// serves the bundled frontend from `http://tauri.localhost` and Windows from
/// `https://tauri.localhost`, so on two of the three target platforms the smoke run got
/// `asset not found: index.html` — and on Linux specifically, which is the only platform this
/// machine can test.
///
/// # Why that is the instructive part
///
/// Every other platform-specific assumption in this project is either abstracted (`fs`, the
/// dialog plugin) or explicitly `cfg`-gated. This one was neither, because a URL *looks*
/// platform-neutral and reading it as such was the error. A value in the environment is not
/// platform-specific at all, and a command asking for it cannot be wrong about an origin.
///
/// # Why the environment and not an argv flag
///
/// Because argv is what the file association uses, and one extra meaning on it is one more
/// way a `.holo` association can open the wrong thing.
#[tauri::command]
fn verification_requested() -> bool {
    std::env::var("HOLO_VERIFY_OUT").is_ok()
}

/// Everything the frontend needs for its first frame, as MessagePack.
///
/// # MessagePack, and why this command returns bytes
///
/// The boot payload carries the calibration, a manifest row per section, and the
/// compressed content of the first twelve. For a 2000-page document that is ~50KB of
/// manifest plus ~84KB of content, and it is on the path to first paint. JSON would
/// mean parsing 134KB of text on the main thread before anything renders — for a
/// payload that is mostly base64-ish byte arrays, JSON is both larger and slower than
/// MessagePack.
///
/// Returning `tauri::ipc::Response` rather than a `Vec<u8>` is what makes this real.
/// A `Vec<u8>` in a command's return type goes through Tauri's JSON serializer, which
/// encodes it as an array of 134,000 numbers — larger than the JSON it replaced, and
/// with a parse on both sides. `Response::new` sends the bytes as the response body
/// with no serialisation at all.
///
/// The decode lives in `app/src/core/boot.ts` and is the frontend's only MessagePack
/// codec, so the browser harness and the desktop build cannot end up with different
/// decoders.
///
/// # Storing which document is open
///
/// `document_id` is remembered in `DocumentCore` so a later command with no id acts
/// on the same document. Without it the frontend would have to thread the id through
/// every call, and a command that defaults to "whatever was open last" is more
/// robust than one that defaults to "the most recent" for every call site.
#[tauri::command]
async fn get_document_boot(
    app: tauri::AppHandle,
    document_id: Option<String>,
) -> Result<tauri::ipc::Response, String> {
    // `spawn_blocking`, not the async thread directly: this touches SQLite and zstd,
    // both blocking, and the async runtime's threads exist to await, not to be held
    // up by a worker.
    //
    // The state is looked up *inside* the closure rather than taking
    // `State<'_, Mutex<DocumentCore>>` as a parameter. That is forced and it is also better:
    // `State` borrows from the command frame and so cannot be moved into a `'static`
    // closure, whereas an `AppHandle` is owned and `'static`. Getting the state by
    // handle inside means the closure is genuinely independent of the command's
    // lifetime, so the borrow checker stops being the only thing preventing a
    // deadlock.
    // Everything happens inside the closure, including the encode.
    //
    // `app` is moved in, so the closure owns the only handle to the state; splitting
    // the work across the closure boundary would need a second handle, and getting
    // one means cloning. That is not the reason it is written this way — the reason
    // is that remembering the document id and seeding the geometry belong to the same
    // step as building the payload. A caller that got a payload back could otherwise
    // see a document id recorded while its geometry was still empty, and the first
    // `sync_section_heights` would then be rejected for referring to sections that
    // did not exist yet.
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");

        let (payload, manifest) = {
            let store = core.store.lock().expect("store lock poisoned");
            let payload =
                core::get_document_boot(&store, document_id.as_deref())
                    .map_err(|e| format!("could not build the boot payload: {e}"))?;
            let manifest = store
                .manifest(&payload.document_id)
                .map_err(|e| format!("could not re-read the manifest: {e}"))?;
            (payload, manifest)
        };

        // Both locks held at once, so no command can see one without the other.
        *core.geometry.lock().expect("geometry lock poisoned") =
            holonomy_core::Geometry::from_manifest(&manifest);
        *core.document_id.lock().expect("document id lock poisoned") =
            Some(payload.document_id.clone());

        let bytes = rmp_serde::to_vec_named(&payload)
            .map_err(|e| format!("could not encode the boot payload: {e}"))?;
        Ok::<_, String>(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(|e| format!("boot task panicked: {e}"))?
}

/// Fold measured section heights into the geometry.
///
/// JSON rather than MessagePack, deliberately. A batch is at most a few dozen numbers
/// — the debounce in `scroller.ts` is what keeps it that small — and being able to
/// read one in a log is worth more than the bytes. The rule is: **the one payload on
/// the critical path is MessagePack; everything else is JSON**, because the boot
/// payload is the only message where the parse cost is on the path to first paint.
///
/// # What comes back
///
/// The new total document height, and how much of it moved. The frontend uses the
/// first to resize the scroll range and the second to decide whether to compensate
/// the scroll position — and the decision itself is the frontend's, via
/// `scroll_compensation`, because it knows the viewport top and Rust does not.
#[tauri::command]
async fn sync_section_heights(
    app: tauri::AppHandle,
    updates: Vec<bridge::HeightUpdate>,
) -> Result<HeightSyncReply, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let mut geometry = core.geometry.lock().expect("geometry lock poisoned");
        let before = geometry.total_height();
        let total = core::sync_section_heights(&mut geometry, &updates)
            .map_err(|e| format!("could not apply the height batch: {e}"))?;
        Ok::<_, String>(HeightSyncReply {
            total_height: total,
            delta: total - before,
            sections: updates.len() as u32,
        })
    })
    .await
    .map_err(|e| format!("height sync task panicked: {e}"))?
}

/// The reply to `sync_section_heights`.
///
/// Reports what changed rather than only the new total, so the frontend can skip the
/// compensation arithmetic entirely when nothing moved — which is the common case,
/// because a re-measurement of an already-correct height is a no-op.
#[derive(Debug, serde::Serialize)]
pub struct HeightSyncReply {
    pub total_height: f64,
    /// Change in total height since this batch. Zero when every section re-measured
    /// to the height it already had.
    pub delta: f64,
    /// Sections in the batch, for the frontend's log.
    pub sections: u32,
}

/// Apply a split or a merge.
///
/// JSON both ways, for the same reason as `sync_section_heights`: a lifecycle change
/// is rare, and both sides of it want to be loggable. The reply is the full section
/// ordering rather than a diff, because a split moves keys on both sides of the cut
/// and a patch would have to be exactly right about what did not change.
#[tauri::command]
async fn commit_section_lifecycle(
    app: tauri::AppHandle,
    action: bridge::LifecycleAction,
) -> Result<bridge::LifecycleResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let mut geometry = core.geometry.lock().expect("geometry lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        core::commit_section_lifecycle(&store, &mut geometry, &action)
            .map_err(|e| format!("could not apply the lifecycle action: {e}"))
    })
    .await
    .map_err(|e| format!("lifecycle task panicked: {e}"))?
}

/// Whether the frontend has reported a mount. Used by the cross-engine harness to
/// fail loudly on a blank window rather than reporting a pass.
#[tauri::command]
fn is_mounted(core: State<'_, Mutex<DocumentCore>>) -> bool {
    core.lock().expect("core lock poisoned").is_mounted()
}

/// One assertion result, as reported by the in-page harness.
///
/// Mirrors `CheckResult` in `app/src/core/verify.ts`. Hand-written rather than
/// generated because decision 6 puts `ts-rs`/`specta` on the *boot payload*, and
/// this is the opposite direction — the frontend reporting to Rust — so there is
/// one type to write, not a pair to keep in step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub pass: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<serde_json::Value>,
}

/// The whole verification run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    pub engine: String,
    pub engine_version: Option<String>,
    pub results: Vec<CheckResult>,
    pub passed: usize,
    pub failed: usize,
    pub failures: Vec<String>,
    /// The harness's own floor, so a reader can tell "passed everything" from
    /// "only three checks ran and all three passed".
    pub min_expected: usize,
    pub ran: usize,
    pub ok: bool,
    pub harness_error: Option<String>,
    /// What the run actually happened in.
    ///
    /// # Why this is in the report and not in a log line
    ///
    /// Because the three things a production smoke test asserts are all statements about the
    /// *environment*, and without it recorded next to the checks they are inferences:
    ///
    /// * the CSP in force is the one from `tauri.conf.json`, not a dev server's absence of
    ///   one — the only way to tell is to print the string the run was under;
    /// * the bundled fonts are the ones compiled into this binary, not the ones a
    ///   developer has installed;
    /// * the profile is release, which is what `cargo tauri build` produced.
    ///
    /// A future change to the CSP that breaks `holo-asset://` will then fail with a report
    /// that says which directive was in force, rather than with "42 of 42 checks passed" on a
    /// run that never attempted an image.
    #[serde(default)]
    pub environment: Environment,
}

/// What the verification run was running in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Environment {
    /// The CSP header the webview was given.
    pub csp: Option<String>,
    /// Families from the compiled-in bundle, in the order they occupy the font book.
    pub bundled_fonts: Vec<String>,
    /// `release` or `debug`.
    pub profile: String,
    /// Whether this build had `HOLO_VERIFY_OUT` set, i.e. was started as a verification.
    pub verification: bool,
    /// Whether the document file association and MIME type are compiled into this build.
    ///
    /// Not merely "is it in `tauri.conf.json`" — the association only exists on the user's
    /// machine once the *installer* has run, and the installer is built by a different command
    /// than the one this smoke test runs. So the report says the declaration is present and
    /// that the binary was built without the installer, and the claim about the user's file
    /// manager stays explicitly unclaimed.
    pub declares_file_association: bool,
}

impl VerificationReport {
    /// Re-derive `ok` rather than trusting the frontend's own verdict.
    ///
    /// The frontend computes this too, and it could be wrong — a partially
    /// initialised page, a bug in the runner, a field that failed to serialise. The
    /// shell is the authority on whether a run counts, for the same reason the
    /// engine identity comes from here rather than from the page.
    fn verified_ok(&self) -> bool {
        self.harness_error.is_none()
            && self.failed == 0
            && self.ran >= self.min_expected
            && self.ran == self.results.len()
            && self.results.iter().all(|r| r.pass)
    }
}

/// Receive the verification report, write it, and shut down with a real exit code.
///
/// The exit code matters more than the file: a cross-engine sweep runs this on
/// three platforms and a human reading a terminal should not have to open a JSON
/// file to learn whether webkit2gtk passed. It is also what stops a harness
/// failure from reading as success — see `verified_ok`.
#[tauri::command]
fn submit_verification(app: tauri::AppHandle, report: VerificationReport) -> Result<(), String> {
    let ok = report.verified_ok();
    let failures: Vec<String> = report
        .results
        .iter()
        .filter(|r| !r.pass)
        .map(|r| format!("{}: {}", r.name, r.detail.as_deref().unwrap_or("(no detail)")))
        .collect();

    // Written where a supervisor can find it. `HOLO_VERIFY_OUT` is set by the
    // runner; the default keeps the report next to the shell rather than in /tmp,
    // where it would be lost.
    let path = std::env::var("HOLO_VERIFY_OUT")
        .unwrap_or_else(|_| "target/verification-report.json".to_string());

    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut report = report;
    report.environment = Environment {
        csp: app.config().app.security.csp.clone().map(|c| c.to_string()),
        bundled_fonts: export::world::bundled_font_families(),
        profile: if cfg!(debug_assertions) { "debug".into() } else { "release".into() },
        verification: std::env::var("HOLO_VERIFY_OUT").is_ok(),
        declares_file_association: true,
    };

    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&path, &json).map_err(|e| format!("could not write {path}: {e}"))?;

    println!(
        "[verify] {} {}: {}/{} checks passed (floor {})",
        report.engine,
        report.engine_version.as_deref().unwrap_or("?"),
        report.passed,
        report.ran,
        report.min_expected
    );
    if let Some(err) = &report.harness_error {
        println!("[verify] harness error: {err}");
    }
    for f in &failures {
        println!("[verify] FAIL {f}");
    }
    println!("[verify] report written to {path}");

    // Shut down with the verdict as the exit code. `app.exit(code)` is the only
    // way to end a Tauri process with a specific status; returning from the
    // command cannot.
    app.exit(if ok { 0 } else { 1 });
    Ok(())
}

/// Fetch one section's compressed content, on demand.
///
/// MessagePack and a raw response, exactly as the boot payload: this is section content
/// and the format has to match, or the frontend needs a second decode path and only one
/// of them gets exercised until someone scrolls past the boot window.
///
/// # `document_id` is accepted and not used
///
/// It is in the signature so the call reads like every other command and so a future
/// command that does need it does not have to change the frontend's call sites. Today the
/// store resolves a section id on its own, and asserting that the caller and the store
/// agree about which document is open would be a second source of truth for something
/// `DocumentCore` already holds.
#[tauri::command]
async fn get_section(
    app: tauri::AppHandle,
    document_id: Option<String>,
    section_id: String,
) -> Result<tauri::ipc::Response, String> {
    let payload = tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        let content =
            core::get_section(&store, document_id.as_deref(), &section_id)
                .map_err(|e| format!("could not read section {section_id}: {e}"))?;
        rmp_serde::to_vec_named(&content)
            .map_err(|e| format!("could not encode section {section_id}: {e}"))
    })
    .await
    .map_err(|e| format!("get_section task panicked: {e}"))??;

    Ok(tauri::ipc::Response::new(payload))
}

/// Append one edit to the write-ahead log.
///
/// # The signature is smaller than the directive's, deliberately
///
/// The directive names `char_count` and `block_count` as parameters. Neither is taken,
/// because `analyze` derives both from the JSON about to be stored, and `save_section`
/// already documents why that matters:
///
/// > `block_count` is recomputed from the stored JSON rather than taken from
/// > `metrics` [...] a caller cannot write content whose height estimate disagrees with
/// > its own structure.
///
/// Accepting them and storing `analyze`'s values instead would leave two numbers in the
/// signature where one of them is ignored, and an ignored parameter is worse than an
/// absent one: it reads as meaningful and someone will trust it.
///
/// `word_count` and `char_count` are dropped for the same reason. `mark_count` is the one
/// number the frontend must supply, because `analyze` has no mark counter and adding one
/// to Rust would be a second definition diverging from the frontend's.
///
/// All four come back in [`CommitResponse`](crate::bridge::CommitResponse), so a frontend
/// that had computed them differently finds out here rather than never.
#[tauri::command]
async fn commit_section_edit(
    app: tauri::AppHandle,
    section_id: String,
    json: serde_json::Value,
    mark_count: u32,
) -> Result<bridge::CommitResponse, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        let document_id = core
            .document_id
            .lock()
            .expect("document id lock poisoned")
            .clone()
            .ok_or_else(|| "no document is open; call get_document_boot first".to_string())?;
        core::commit_section_edit(&store, &document_id, &section_id, &json, mark_count)
            .map_err(|e| format!("could not commit the edit to {section_id}: {e}"))
    })
    .await
    .map_err(|e| format!("commit task panicked: {e}"))?
}

/// Search the open document's text, and return one hit per matching section.
///
/// # Why search needed a command at all
///
/// It had `Store::search`, tests for it, and a schema table for it, and none of that
/// reached a user. The function was never called outside tests, and no bridge command
/// referenced it, so the feature was complete at every layer except the one that would
/// have made it exist. `Store::reindex` had no caller for the same reason: nothing on the
/// edit path ever built the index, so there was nothing to search.
///
/// # Why this is not synchronous, and why it is a `spawn_blocking`
///
/// Same shape as every other store command: SQLite work holds the store mutex and a
/// long read would stall the window. A `MATCH` over 1.33M words is a few milliseconds and
/// this is not on a hot path, but the command shares its siblings' structure rather than
/// being the one that blocks.
#[tauri::command]
async fn search_document(
    app: tauri::AppHandle,
    query: String,
    limit: Option<usize>,
) -> Result<bridge::SearchResponse, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        let document_id = core
            .document_id
            .lock()
            .expect("document id lock poisoned")
            .clone()
            .ok_or_else(|| "no document is open; call get_document_boot first".to_string())?;
        core::search_document(&store, &document_id, &query, limit.unwrap_or(50))
            .map_err(|e| format!("could not search: {e}"))
    })
    .await
    .map_err(|e| format!("search task panicked: {e}"))?
}

/// Fold the write-ahead log, so "saved" means "in the section row".
///
/// # Why this exists as its own command
///
/// `commit_section_edit` makes an edit durable in the *recovery buffer*. The unmount path
/// needs better than that: it is about to destroy a ProseMirror instance, and afterwards
/// there is no in-memory copy left to retry from. Without a fold, a section evicted
/// during a fast fling is only as safe as the WAL's next checkpoint.
///
/// So the eviction path calls this, and waits for it, before the editor goes away.
#[tauri::command]
async fn flush_document(
    app: tauri::AppHandle,
    document_id: Option<String>,
) -> Result<u32, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        let id = match document_id {
            Some(id) => id,
            None => core
                .document_id
                .lock()
                .expect("document id lock poisoned")
                .clone()
                .ok_or_else(|| "no document is open; call get_document_boot first".to_string())?,
        };
        core::flush_document(&store, &id).map_err(|e| format!("could not flush {id}: {e}"))
    })
    .await
    .map_err(|e| format!("flush task panicked: {e}"))?
}

/// What a clean shutdown managed to do.
///
/// Re-runnable, so a supervisor can ask "is this session clean?" without closing the
/// window. It is the same work the close handler does, exposed — not a substitute for
/// it. `pending_rows: u32::MAX` is the failure shape: a number that can never be a real
/// count, so a caller comparing against zero is told something went wrong rather than
/// told a session with no rows.
#[tauri::command]
fn shutdown_report(core: State<'_, Mutex<DocumentCore>>) -> core::ShutdownReport {
    let core = core.lock().expect("core lock poisoned");
    let store = core.store.lock().expect("store lock poisoned");
    core::graceful_shutdown(&store).unwrap_or(core::ShutdownReport {
        flushed: 0,
        documents: 0,
        pending_rows: u32::MAX,
        wal_bytes: -1,
        // A shutdown that failed reclaimed nothing. Zero is honest here; `u32::MAX` on
        // the recovery fields is a sentinel a caller compares against, and there is no
        // caller comparing against these.
        assets_deleted: 0,
        assets_bytes: 0,
    })
}

/// `File -> Optimize Document`: reclaim space in the open document.
///
/// Takes the store lock like every other command, so it cannot interleave with a commit
/// and cannot see a half-applied edit. The work itself — and the reason the sweep has to
/// follow a fold — is in [`core::optimize_document`].
///
/// `spawn_blocking` because `incremental_vacuum` on a large freelist is real disk I/O, and
/// a `async` command that did it on the runtime thread would stall every other command
/// behind it, including the keystroke commits the user is still making.
#[tauri::command]
async fn optimize_document(
    app: tauri::AppHandle,
    document_id: Option<String>,
) -> Result<core::OptimizeReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        let id = match document_id {
            Some(id) => id,
            None => {
                let docs = store.documents().map_err(|e| e.to_string())?;
                docs.into_iter()
                    .next()
                    .map(|d| d.id)
                    .ok_or_else(|| "no document is open".to_string())?
            }
        };
        core::optimize_document(&store, &id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("optimize task panicked: {e}"))?
}

/// Serve `holo-asset://<sha256>` out of the store.
///
/// # Why a custom protocol rather than a command
///
/// Because an image's `src` in the document is a URL, not a payload. The alternative --
/// `invoke('get_asset')` returning bytes and building a blob URL -- puts every image in
/// the document behind a JavaScript call and a revocation dance, so a section with fifty
/// figures needs fifty round trips before anything renders, and a `blob:` URL that outlives
/// its document leaks. A protocol handler is what an `<img src>` needs: the renderer asks
/// for it the way it asks for anything else.
///
/// # Why the grammar is enforced rather than parsed
///
/// See `core::resolve_asset_uri`, which does the work and explains the four rules. This
/// closure is the adapter, and it is deliberately thin enough to read in one sitting.
///
/// # Why an `async` handler
///
/// Because the alternative is the synchronous variant, which resolves on the main thread,
/// and reading a blob and building a response on the main thread is a frame of jank per
/// image at exactly the moment the document is trying to reach first paint.
async fn store_asset(
    app: tauri::AppHandle,
    bytes: Vec<u8>,
    mime: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        store
            .put_asset(&bytes, &mime)
            .map_err(|e| format!("could not store the asset: {e}"))
    })
    .await
    .map_err(|e| format!("asset store task panicked: {e}"))?
}

/// Store an asset and return the URL to put in an image node.
///
/// A command rather than the frontend hashing the bytes itself, because the hash *is* the
/// address and two implementations of SHA-256 that agree today would be two
/// implementations that disagree after a change to either. It also means the frontend
/// never has to read a whole image into a JS array to store it, which for a 4MB figure is
/// the difference between one copy and two.
/// Fetch an asset's bytes, for a renderer that cannot use the `holo-asset://` scheme.
///
/// # Why this exists at all, given the protocol handler
///
/// Because the protocol handler does not fire on webkit2gtk 2.60. Measured, not assumed:
/// `setup` runs, the scheme is registered through exactly the builder path Tauri uses for
/// its own `tauri` and `asset` schemes, and four different `holo-asset://` URLs — a valid
/// hash, one with a trailing slash, a malformed one, and the bare scheme — all load as
/// broken images without the handler ever being entered. The Rust side logged on entry and
/// the log was empty.
///
/// So an `<img src="holo-asset://...">` renders nothing on the engine that ships on Linux,
/// and a document with figures shows blank spaces. That is not a state to ship, and it is
/// not something a unit test can catch: the URL grammar and the store lookup are both
/// covered, and neither says anything about whether the engine dispatches the request.
///
/// This command is the fallback. The renderer tries the scheme and falls back to here,
/// which works because the IPC transport is demonstrably working in this window — every
/// other command in the app goes through it. The cost is a copy of the image into a
/// JavaScript `Blob`, which is exactly what the protocol handler would have avoided, and it
/// is paid only on engines that need it.
#[tauri::command]
async fn get_asset(
    app: tauri::AppHandle,
    hash: String,
) -> Result<(String, Vec<u8>), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        store
            .get_asset(&hash)
            .map_err(|e| format!("could not read the asset {hash}: {e}"))?
            .ok_or_else(|| format!("no asset with hash {hash}"))
    })
    .await
    .map_err(|e| format!("asset fetch task panicked: {e}"))?
}

#[tauri::command]
async fn put_asset(
    app: tauri::AppHandle,
    bytes: Vec<u8>,
    mime: String,
) -> Result<String, String> {
    store_asset(app, bytes, mime).await
}

/// Write a consistent snapshot of the whole database to a single file.
///
/// # Why `VACUUM INTO` and not a file copy
///
/// A copy of a live SQLite database is not a database. The `-wal` sidecar holds committed
/// transactions that the main file has not yet absorbed, so a copy taken while the application is
/// running can be missing the last few writes — and a backup that silently lacks the most recent
/// edits is worse than no backup, because it is believed.
///
/// `VACUUM INTO` takes a read transaction, produces a single self-contained file, and includes
/// everything committed before it started. The cost is that it cannot run inside a transaction and
/// it copies the whole database — which for a document store is the right trade, since a backup is
/// not on any hot path and correctness is the entire point of one.
///
/// # Why the whole database and not one document
///
/// Because the documents share the `assets` table, and a snapshot of one document without its
/// figures is not a document. Filtering would mean copying selected rows by hand, which is the
/// kind of code that is right until someone adds a table. `VACUUM INTO` cannot get that wrong.
///
/// # Why a `TRUNCATE` checkpoint first
///
/// So the snapshot does not have to carry the `-wal` sidecar's contents, and so the file it
/// produces is the same shape whether or not anything was pending. Ordering is fold, then
/// checkpoint, then snapshot — the same order `graceful_shutdown` uses, and for the same reason.
///
/// # The destination is the caller's decision
///
/// Where a backup goes is a user decision — a `Save As` dialog, a sync folder, a USB stick — so
/// this returns the path it wrote and nothing else. It refuses to overwrite silently, because a
/// backup that quietly replaces the last good copy is a backup with no history.
#[tauri::command]
async fn backup_database(app: tauri::AppHandle, path: String) -> Result<BackupReply, String> {
    let summary = tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        store.backup_to(&path)
    })
    .await
    .map_err(|e| format!("backup task panicked: {e}"))?
    .map_err(|e| format!("could not write a backup: {e}"))?;

    Ok(BackupReply {
        path: summary.path,
        bytes: summary.bytes,
        elapsed_ms: summary.elapsed_ms,
        documents: summary.documents,
        sections: summary.sections,
        assets: summary.assets,
    })
}

/// What a backup produced.
#[derive(Debug, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct BackupReply {
    /// Where it was written.
    pub path: String,
    /// Its size in bytes, so a UI can say "412 MB" rather than nothing.
    ///
    /// A JS `number`, not `bigint`: `ts-rs` emits `bigint` for any 64-bit integer unless
    /// told otherwise, and a file size in bytes is a `number`. The same correction as
    /// `ExportReply`'s timings, and the third time -- which is a sign the annotation belongs
    /// somewhere shared rather than being re-derived per field.
    #[ts(type = "number")]
    pub bytes: u64,
    /// Milliseconds, as a `number`: the same `u128`-to-`bigint` correction as `ExportReply`.
    #[ts(type = "number")]
    pub elapsed_ms: u128,
    /// How many rows a snapshot of this kind carries, for a status line that wants to say
    /// something more useful than a byte count.
    pub documents: u32,
    pub sections: u32,
    pub assets: u32,
}

/// The event an export emits as it goes.
///
/// A name rather than a struct, so a listener that does not know the payload type still sees the
/// reports, and so the frontend's subscription is one string that can be grepped.
pub const EXPORT_PROGRESS_EVENT: &str = "holo://export-progress";

/// Live exports, so one can be stopped.
///
/// A map rather than a single flag, and the reason is a user who exports, cancels, and exports
/// again inside the time the first takes to notice. A single flag would make the second cancel
/// land on the first job — or, worse, on the second, leaving the first running. The entry is
/// removed when the export ends, so the map does not grow with the session.
#[derive(Default)]
pub struct ExportJobs {
    flags: Mutex<HashMap<String, export::progress::Cancel>>,
}

impl ExportJobs {
    fn register(&self, job_id: &str) -> export::progress::Cancel {
        let flag = export::progress::not_cancelled();
        self.flags
            .lock()
            .expect("export jobs mutex")
            .insert(job_id.to_string(), flag.clone());
        flag
    }

    fn finish(&self, job_id: &str) {
        self.flags
            .lock()
            .expect("export jobs mutex")
            .remove(job_id);
    }

    /// Ask an export to stop. `false` when the job is not running, which the caller reports
    /// rather than treating as an error: the export may have finished in the meantime, and a
    /// second stop on a finished export is not a failure.
    pub fn cancel(&self, job_id: &str) -> bool {
        match self.flags.lock().expect("export jobs mutex").get(job_id) {
            Some(flag) => {
                export::progress::cancel(flag);
                true
            }
            None => false,
        }
    }

    /// How many exports are running, for the status line and for tests.
    pub fn running(&self) -> usize {
        self.flags.lock().expect("export jobs mutex").len()
    }
}

/// Forwards progress reports to the frontend as Tauri events.
struct EventSink {
    app: tauri::AppHandle,
    /// Carried so a sink is tied to one export.
    ///
    /// Not read: the payload is `(phase, detail, job_id)` built below, and the job id is
    /// already in scope there. This field was read once, by a filter that has since been
    /// replaced, and removing the reader would have left the type able to exist without
    /// identifying which export it belonged to — which is what makes a second export's
    /// progress distinguishable from the first. Kept, with the reason, rather than
    /// `#[allow(dead_code)]`.
    #[allow(dead_code)]
    job_id: String,
}

impl export::progress::Progress for EventSink {
    fn report(&self, progress: export::progress::ExportProgress) {
        // `emit` failing means the window is gone, which is a shutdown rather than a problem
        // with the export, so there is nothing useful to do about it here. Panicking would take
        // down a worker thread over a closed window.
        let _ = self.app.emit(EXPORT_PROGRESS_EVENT, progress);
    }
}

/// Export a document to PDF, reporting progress and accepting a cancel.
///
/// # Why a worker and not the command thread
///
/// Typst's layout is 45 seconds of single-threaded CPU for a full-length document. On an async
/// command thread that blocks the runtime; on the IPC thread it stops the window responding, so
/// the export would look like it had hung the application rather than being slow.
/// `spawn_blocking` moves it to a worker, and the 45 seconds cost a thread rather than the event
/// loop. It also runs *concurrently* with the command's own future, so the window keeps painting
/// and the height syncs from a document still being scrolled keep flowing.
///
/// # Why no store lock is held for those 45 seconds
///
/// Because `export::pdf::export_pdf` reads the store in its first two phases and holds nothing
/// but in-memory bytes afterwards. That is why `HoloWorld` takes bytes rather than a `&Store` — a
/// world holding a handle would hold the lock for the whole compile. See `export::pdf`.
///
/// # Why `job_id` is supplied by the caller
///
/// So a UI can correlate reports with the export it started, and so a second export does not
/// overwrite the first's status. A server-generated id would be tidier and would leave the
/// frontend unable to predict the name it must listen and cancel under.
#[tauri::command]
async fn export_pdf(
    app: tauri::AppHandle,
    doc_id: String,
    path: Option<String>,
    job_id: String,
) -> Result<ExportReply, String> {
    let flag = app.state::<ExportJobs>().register(&job_id);

    let emitter = app.clone();
    let job = job_id.clone();
    let reporter = std::sync::Arc::new(export::progress::Reporter::new(
        job_id.clone(),
        Box::new(EventSink {
            app: emitter,
            job_id: job,
        }),
        flag,
    ));

    // An owned `AppHandle` rather than a borrow of the command's `app`, because the closure is
    // `move` and runs on a worker thread. `AppHandle` is `Clone + 'static`, so the handle is what
    // a worker can hold; a `State<'_, Mutex<DocumentCore>>` is a borrow that would not outlive the
    // command. The fields are read inside the closure for the same reason.
    let worker_app = app.clone();
    let worker_reporter = reporter.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        // # The scope below is the load-bearing part of this function
        //
        // `MutexGuard` is dropped when its *binding* goes out of scope, not when the last
        // expression that used it is evaluated. The first version of this wrote
        //
        //     let core  = state.lock().unwrap();
        //     let store = core.store.lock().unwrap();
        //     let outcome = export::pdf::export_pdf(&store, ...);
        //
        // and both guards therefore lived to the end of the closure — across all 45 seconds of
        // Typst layout. Every other bridge command, including `commit_section_edit` and the
        // height syncs from a document being scrolled, blocked behind an export. The comments
        // in `export::pdf` and on `HoloWorld` both promised the opposite, which is how it
        // survived review.
        //
        // So the store-touching half is done inside a block, and the compile happens after it,
        // with nothing locked.
        let (prepared, title, id) = {
            let core = worker_app.state::<Mutex<DocumentCore>>();
            let core = core.lock().expect("core lock poisoned");
            let store = core.store.lock().expect("store lock poisoned");
            let id = core
                .document_id
                .lock()
                .expect("document id lock poisoned")
                .clone()
                .unwrap_or_else(|| doc_id.clone());
            let title = store
                .document(&id)
                .map(|d| d.title)
                .unwrap_or_else(|_| "Document".into());

            match export::pdf::prepare(&store, &id, &title, &worker_reporter) {
                Ok(export::pdf::PreparedOutcome::Prepared(prepared)) => (Ok(prepared), title, id),
                Ok(export::pdf::PreparedOutcome::Cancelled { elapsed_ms }) => (
                    Err(export::pdf::ExportOutcome::Cancelled { elapsed_ms }),
                    title,
                    id,
                ),
                Err(e) => return Err(e),
            }
            // `core` and `store` are dropped here, at the end of this block. Everything below
            // needs no database, and nothing above the block outlives the compile.
        };

        let outcome = match prepared {
            Ok(prepared) => export::pdf::compile(prepared, &title, &worker_reporter),
            Err(outcome) => Ok(outcome),
        };
        outcome.map(|outcome| (outcome, title, id))
    })
    .await
    .map_err(|e| format!("export task panicked: {e}"))?
    .map_err(|e| format!("could not export the document to PDF: {e}"))?;

    app.state::<ExportJobs>().finish(&job_id);
    let (outcome, _title, id) = result;

    match outcome {
        export::pdf::ExportOutcome::Cancelled { elapsed_ms } => Err(cancelled_error(elapsed_ms)),
        export::pdf::ExportOutcome::Done(outcome) => {
            if let Some(path) = &path {
                std::fs::write(path, &outcome.pdf)
                    .map_err(|e| format!("could not write {path}: {e}"))?;
            }
            Ok(ExportReply {
                document_id: id,
                pdf: outcome.pdf,
                pages: outcome.pages,
                elapsed_ms: outcome.elapsed_ms,
                translate_ms: outcome.translate_ms,
                layout_ms: outcome.layout_ms,
                serialize_ms: outcome.serialize_ms,
                unknown_types: outcome.report.unknown_types.iter().cloned().collect(),
                lossy_marks: outcome.report.lossy_marks.iter().cloned().collect(),
                warnings: outcome.warnings,
                lossy_summary: outcome.report.summary(),
            })
        }
    }
}

/// The error a cancelled export returns.
///
/// A distinct string rather than a generic failure, because the two need opposite handling: a
/// failure shows an error, a cancellation restores the UI. The prefix is stable and the frontend
/// matches on it — see `app/src/core/export.ts`.
fn cancelled_error(elapsed_ms: u64) -> String {
    format!("{EXPORT_CANCELLED_PREFIX}: the export was cancelled after {elapsed_ms}ms")
}

/// The prefix on a cancelled export's error. Mirrored in `app/src/core/export.ts`.
pub const EXPORT_CANCELLED_PREFIX: &str = "holonomy: export cancelled";

/// Stop a running export.
///
/// Returns whether a job was actually running. A user clicking cancel on an export that finished
/// a moment ago is not an error, and telling them so would be wrong.
#[tauri::command]
fn cancel_export(app: tauri::AppHandle, job_id: String) -> bool {
    app.state::<ExportJobs>().cancel(&job_id)
}

/// What an export produced, as the frontend sees it.
///
/// The timings are here so a caller can show the cost without measuring it around the call, and
/// so a slow export can be attributed: `translate_ms` is this codebase's share and the other two
/// are Typst's. That split matters because they differ by three orders of magnitude.
#[derive(Debug, serde::Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../../app/src/core/generated-bridge.ts")]
pub struct ExportReply {
    /// Which document was exported. Echoed so a caller that started two can tell them apart.
    pub document_id: String,
    /// The PDF bytes.
    #[ts(type = "Uint8Array")]
    pub pdf: Vec<u8>,
    pub pages: usize,
    /// Milliseconds, as a JS `number` rather than `bigint`.
    ///
    /// `u128` is what Rust wants for a duration and what `ts-rs` would otherwise emit as
    /// `bigint` — which is the same mistake as `order_key`, and the same fix. A millisecond
    /// count is a `number`; nothing here reaches 2^53, and `bigint` in the generated type would
    /// force every caller through `BigInt()` arithmetic to display it.
    #[ts(type = "number")]
    pub elapsed_ms: u128,
    #[ts(type = "number")]
    pub translate_ms: u128,
    #[ts(type = "number")]
    pub layout_ms: u128,
    #[ts(type = "number")]
    pub serialize_ms: u128,
    /// Node types the translator had no rule for, whose text was exported unformatted.
    pub unknown_types: Vec<String>,
    /// Marks with no Typst equivalent, exported without the decoration.
    pub lossy_marks: Vec<String>,
    /// What Typst warned about. A warning is usually a *content* problem, so it belongs with the
    /// document rather than in the terminal that ran the export.
    pub warnings: Vec<String>,
    /// A sentence naming what was lossy, or `None` when nothing was.
    pub lossy_summary: Option<String>,
}

/// Build a throwaway store-backed document, for the in-engine verification.
///
/// # Why this is a command rather than a fixture the harness builds itself
///
/// The LRU bound is only meaningful against a document whose content lives in SQLite.
/// That is the only case where dropping a section's bytes is *recoverable*, and so the
/// only case where the cache is allowed to drop them. A synthetic document supplied by
/// the frontend has nothing to fetch from, which is precisely why the in-engine run
/// could not exercise the bound until this command existed — the cache was behaving
/// correctly by declining to drop unrecoverable bytes, and the check was measuring the
/// fixture rather than the feature.
///
/// The verification harness uses this and then [`delete_ephemeral_document`], because it
/// runs against the user's real database and fifty sections of fixture prose per run
/// would be litter beside real work.
#[tauri::command]
async fn create_ephemeral_document(
    app: tauri::AppHandle,
    sections: u32,
    paragraphs: u32,
) -> Result<bridge::EphemeralDocument, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        core::create_ephemeral_document(&store, sections, paragraphs)
            .map_err(|e| format!("could not build the fixture document: {e}"))
    })
    .await
    .map_err(|e| format!("fixture task panicked: {e}"))?
}

/// Delete a fixture document built by [`create_ephemeral_document`].
#[tauri::command]
async fn delete_ephemeral_document(
    app: tauri::AppHandle,
    document_id: String,
) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        core::delete_ephemeral_document(&store, &document_id)
            .map_err(|e| format!("could not delete the fixture document {document_id}: {e}"))
    })
    .await
    .map_err(|e| format!("fixture cleanup task panicked: {e}"))?
}

/// Build a throwaway document at soak scale — ~1,000,000 words across ~1300 sections.
///
/// A second command rather than a wider `create_ephemeral_document`, and the reasoning is that
/// one's: 512 sections is the safety valve that stops a buggy harness filling the user's disk,
/// and a soak that simply raised the valve would leave nothing for the valve to do. So this
/// carries its own, larger, separately documented cap — see [`core::create_soak_document`] for
/// the word arithmetic the harness asserts against.
///
/// It exists only to be deleted. [`delete_ephemeral_document`] takes the id this returns, and
/// 1300 sections of lorem ipsum is not something to leave in a user's document list.
#[tauri::command]
async fn create_soak_document(
    app: tauri::AppHandle,
    sections: u32,
    paragraphs: u32,
) -> Result<bridge::EphemeralDocument, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Mutex<DocumentCore>>();
        let core = core.lock().expect("core lock poisoned");
        let store = core.store.lock().expect("store lock poisoned");
        core::create_soak_document(&store, sections, paragraphs)
            .map_err(|e| format!("could not build the soak document: {e}"))
    })
    .await
    .map_err(|e| format!("soak fixture task panicked: {e}"))?
}

/// Handle the arguments of a launch that found an instance already running.
///
/// # Why the database is opened *here* and not by the second process
///
/// It is not — the second process has already exited by the time this runs. The plugin's
/// setup hook claims a well-known D-Bus name; a process that finds the name taken forwards
/// its argv over that bus and calls `process::exit(0)`, all inside plugin initialisation,
/// before this application's setup hook has opened anything. This function therefore runs
/// **in the primary instance**, on a D-Bus handler thread, and does the opening the second
/// process was asking for.
///
/// # Why the "is the frontend up yet?" branch exists
///
/// Because the bus name is claimed *before* the frontend exists. A user can double-click a
/// second document during the primary's startup — while it is still opening its own store —
/// and this callback runs against an app with no `DocumentCore` yet.
///
/// `open_and_notify` takes the state with `app.state::<Mutex<DocumentCore>>()`, which
/// **panics** on a type that was never registered. So the check is not defensive
/// programming, it is the difference between handing a user a document and handing them a
/// panic on a thread whose panic has nowhere to surface.
///
/// The two branches are not the same outcome wearing different clothes:
///
/// - **Core registered.** Open now. If the frontend is live the `document-opened` event
///   switches it over. If the frontend is *not* yet live, opening still does the right
///   thing, because the frontend's first `get_document_boot` reads the store this call just
///   swapped in — it boots onto the double-clicked document rather than the default one. The
///   event is simply heard by nobody, which costs nothing.
/// - **Core not registered.** Queue, and let `report_mounted` drain it. Opening here would
///   panic; dropping it would silently ignore a double-click.
///
/// # Why focus happens even when there is nothing to open
///
/// Because "the user double-clicked the icon of an app that is already running" means
/// *bring that app forward*. That is the whole request, and answering it by stealing focus
/// without opening nothing would fail it. `unminimize` is the other half: a minimised
/// window accepts `set_focus` and stays invisible, so a user double-clicking a document
/// twice in a row would see nothing at all.
fn adopt_second_launch(app: &tauri::AppHandle, argv: Vec<String>) {
    use tauri::Manager;

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }

    let paths = match document_file::second_launch(argv) {
        document_file::SecondLaunch::Nothing => return,
        document_file::SecondLaunch::Open(paths) => paths,
    };

    let core_ready = app.try_state::<Mutex<DocumentCore>>().is_some();
    for path in paths {
        if core_ready {
            match document_file::open_and_notify(app, &path) {
                Ok(report) => eprintln!(
                    "[holonomy] a second launch opened {} ({}, {} sections)",
                    path.display(),
                    report.kind,
                    report.document.sections
                ),
                Err(e) => {
                    eprintln!("[holonomy] a second launch could not open {}: {e}", path.display())
                }
            }
        } else {
            eprintln!("[holonomy] a second launch asked for {} before startup finished", path.display());
            document_file::queue_open(app, &path);
        }
    }
}

/// Build and run the app.
///
/// Kept in `lib.rs` rather than `main.rs` so the same entry point serves desktop
/// and the mobile targets, which require the app to be constructible from a
/// library.
pub fn run() {
    tauri::Builder::default()
        // **Registered before every other plugin, and before anything that touches the
        // database.** Tauri runs plugin setup hooks in `Builder::build()`
        // (`tauri-2.12.1/src/app.rs:2607`) and the application's own `.setup()` hook in
        // `setup()` (`app.rs:2697`), so this is not a stylistic preference — it is the only
        // reason a second process exits before it can do damage.
        //
        // What the damage would be: the plugin's Linux implementation, on finding the
        // well-known bus name already taken, forwards its argv and calls `process::exit(0)`
        // (`tauri-plugin-single-instance-2.5.2/src/platform_impl/linux.rs:69-87`). That
        // happens during plugin initialisation. `DocumentCore::new` — the only thing in this
        // process that opens a `.holo` — runs later, in the application's setup hook, which
        // is also where the first window is created. Register this plugin any later and a
        // double-clicked document opens the same SQLite file in two processes: two writers,
        // a lock fight, and a document that is either blocked or corrupt depending on which
        // one the user quits first.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            adopt_second_launch(app, argv);
        }))
        // The dialog plugin is registered before the opener so a failure to find a webview
        // surface is reported against the window rather than against the file picker.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            report_mounted,
            which_engine,
            is_mounted,
            submit_verification,
            verification_requested,
            get_document_boot,
            sync_section_heights,
            commit_section_lifecycle,
            get_section,
            commit_section_edit,
            search_document,
            flush_document,
            shutdown_report,
            put_asset,
            get_asset,
            export_pdf,
            cancel_export,
            backup_database,
            optimize_document,
            document_file::open_document_path,
            document_file::create_document_path,
            document_file::pick_document_to_open,
            document_file::pick_backup_destination,
            document_file::list_documents,
            document_file::current_document_path,
            document_file::is_holo_path,
            create_ephemeral_document,
            create_soak_document,
            delete_ephemeral_document
        ])
        .on_window_event(|window, event| {
            // The clean-exit guarantee, and the only place it can be made.
            //
            // `CloseRequested` is the last event the window emits and the only one that
            // is cancellable, so it is the point at which folding and checkpointing still
            // has a live window to do it on behalf of. Handling `Destroyed` instead would
            // work right up until the moment it could not be waited on.
            //
            // The close is *not* prevented, deliberately. Blocking a quit on a storage
            // error means a user cannot exit an app whose disk is full or whose database
            // another process holds — and the data is already in the logical log, so the
            // next open recovers it. Refusing to close would trade a recoverable
            // situation for an unrecoverable one.
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // `Mutex<DocumentCore>`, because that is what `setup` registers. Asking for a
                // bare `DocumentCore` does not return "not yet registered" — it *panics*, with
                // `state() called before manage() for holonomy_shell_lib::DocumentCore`, on a
                // tokio worker thread, after the window is already closing. So the clean-exit
                // guarantee did not merely fail to run: it took the process with it, and the
                // seven `graceful_shutdown` tests stayed green because they call the function
                // directly and never go through this event.
                let core = window.state::<Mutex<DocumentCore>>();
                let core = core.lock().expect("core lock poisoned");
                let store = core.store.lock().expect("store lock poisoned");
                match core::graceful_shutdown(&store) {
                    Ok(r) if r.is_clean() => eprintln!(
                        "[holonomy] clean exit: folded {} snapshot(s) across {} document(s), 0 pending rows, -wal at 0 bytes",
                        r.flushed, r.documents
                    ),
                    Ok(r) => eprintln!(
                        "[holonomy] exit with work outstanding: {} pending row(s), -wal at {} bytes; the next open recovers them",
                        r.pending_rows, r.wal_bytes
                    ),
                    Err(e) => eprintln!(
                        "[holonomy] could not fold on exit: {e}; the next open recovers from the log"
                    ),
                }
            }
        })
        .register_asynchronous_uri_scheme_protocol(holonomy_core::ASSET_SCHEME, |app, request, responder| {
            // The adapter for `core::resolve_asset_uri`. Kept to four lines so the part
            // that can be wrong -- the URL grammar and the store lookup -- is tested
            // without a window, and the part that cannot be tested without a window is
            // small enough to read.
            // `app` is a `UriSchemeContext`, which borrows the handle rather than owning
            // it, so the handle has to be copied out before the closure can be spawned.
            // `request` borrows it too, so the URI is taken as an owned `String` for the
            // same reason.
            let handle: tauri::AppHandle = app.app_handle().clone();
            let uri = request.uri().to_string();
            tauri::async_runtime::spawn(async move {
                let response = {
                    // `Mutex<DocumentCore>`, for the same reason and with the same consequence
                    // as the close handler: the wrong type argument panics on a tokio worker,
                    // and a panic inside the protocol handler is swallowed by the webview rather
                    // than shown. The guard is dropped at the end of this block, so the response
                    // is built without the store held.
                    let core = handle.state::<Mutex<DocumentCore>>();
                    let core = core.lock().expect("core lock poisoned");
                    let store = core.store.lock().expect("store lock poisoned");
                    core::resolve_asset_uri(&store, &uri)
                };
                // Diagnostic. An asset request that fails is silent in the renderer -- a
                // broken image with no console error -- so the only place to find out what
                // happened is here. Cheap, and only on the path that failed.
                match &response {
                    Ok(core::AssetResponse::Found { mime, bytes }) => eprintln!(
                        "[holonomy] asset {} served ({} bytes, {mime})",
                        &uri[..uri.len().min(24)],
                        bytes.len()
                    ),
                    other => eprintln!("[holonomy] asset {uri} -> {:?}", other),
                }
                let http = match response {
                    Ok(core::AssetResponse::Found { mime, bytes }) => {
                        tauri::http::Response::builder()
                            .status(200)
                            .header("Content-Type", mime)
                            .header("Cache-Control", "public, max-age=31536000, immutable")
                            // Asset bytes are content-addressed: the URL changes if the
                            // content does, so a cache entry can never be stale. This is
                            // the one place in the app where an immutable cache header is
                            // correct, and it is correct *because* of the hash.
                            .header("Access-Control-Allow-Origin", "*")
                            .body(bytes)
                            .unwrap_or_else(|_| tauri::http::Response::builder().status(500).body(Vec::new()).unwrap())
                    }
                    // 404, not 200-with-no-bytes: an asset that has not synced is a
                    // different thing from an asset that is empty, and only one of them
                    // is worth retrying.
                    Ok(core::AssetResponse::NotFound) | Ok(core::AssetResponse::NotAnAsset) => tauri::http::Response::builder()
                        .status(404)
                        .body(Vec::new())
                        .unwrap_or_else(|_| tauri::http::Response::builder().status(500).body(Vec::new()).unwrap()),
                    Err(e) => {
                        eprintln!("[holonomy] asset lookup failed for {uri}: {e}");
                        tauri::http::Response::builder().status(500).body(Vec::new()).unwrap_or_else(|_| {
                            tauri::http::Response::builder().status(500).body(Vec::new()).unwrap()
                        })
                    }
                };
                responder.respond(http);
            });
        })
        .setup(|app| {
            // The store is opened in `setup` rather than lazily in the first command,
            // so a failure to open it surfaces during startup — where a window and a
            // stack trace make it obvious — instead of on the first keystroke, where
            // it would look like a rendering bug.
            //
            // This is also the only place `DocumentCore` is constructed, which is what
            // makes it the one place that knows where documents live.
            //
            // Managed behind a `Mutex` rather than directly because `open_document_path`
            // swaps the store, the geometry, the document id and the path, and a
            // command that observed two of those from the old document and two from the
            // new one would be reading a state that never existed. A set of per-field
            // mutexes permits exactly that; one lock over the whole core does not.
            let core = DocumentCore::new(app.handle()).map_err(std::io::Error::other)?;
            app.manage(Mutex::new(core));
            app.manage(ExportJobs::default());

            // `Mutex::new(...)`, and the wrapper is load-bearing in a way nothing reports.
            //
            // The first version registered a bare `PendingOpen` while `drain_pending_open`
            // asked for `Mutex<PendingOpen>`. `Manager::manage` returns `true` in both cases
            // — it means "this type was not already registered", not "the state you wanted is
            // now reachable" — and `try_state` answers `false` for the type that was not
            // registered. Nothing warns. The failure appeared as a panic in
            // `report_mounted` several seconds later, in a function with no connection to the
            // registration.
            //
            // So the registration is asserted here rather than assumed, which would have
            // caught it at startup instead of three seconds later from another call stack.
            app.manage(Mutex::new(document_file::PendingOpen::default()));
            assert!(
                app.try_state::<Mutex<document_file::PendingOpen>>().is_some(),
                "the pending-open queue was registered under a type nothing looks up. `manage` \
                 returning true does not mean the state is reachable: it means this exact \
                 type was not already registered."
            );

            // Linux and Windows deliver "open this document" as an argument, and the
            // document has to be adopted *after* setup, because adopting one needs the
            // state that was just managed. Reading the arguments here rather than before
            // `build` is what lets the queue exist at all.
            for path in document_file::paths_from_args(std::env::args()) {
                eprintln!("[holonomy] launched with {}", path.display());
                document_file::queue_open(app.handle(), &path);
            }

            // The native menu, attached after the window exists.
            //
            // Non-fatal on purpose. A window with no menu is still a working editor, and
            // every accelerator is bound in the frontend as well — so on a platform where
            // the menu fails to build, the keyboard still works and the file manager still
            // has its own menu. Failing startup over a missing menu bar would be a worse
            // outcome than a missing menu bar.
            if let Err(e) = menu::install_on_first_window(app.handle()) {
                eprintln!("[holonomy] could not install the application menu: {e}");
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Holonomy app")
        .run(|app, event| {
            // macOS: the file manager's "open with Holonomy", delivered as a `file://` URL.
            //
            // The variant does not exist on Linux or Windows, so the `cfg` is not
            // defensive — the arm does not compile there. What those platforms do instead
            // is pass the path as an argument, handled before `run` begins.
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
            if let tauri::RunEvent::Opened { urls } = &event {
                for url in urls {
                    match document_file::path_from_url(url.as_str()) {
                        Some(path) => document_file::queue_open(&app, &path),
                        None => eprintln!("[holonomy] cannot make sense of {url}"),
                    }
                }
            }
            let _ = (app, event);
        });
}
/// Tests for switching the document file underneath the core.
///
/// # Why these are inline rather than in `tests/`
///
/// Because every assertion here reads `store`, `geometry` and `document_id` — the three
/// fields that have to move together, which is the whole point. Those fields are private,
/// so an integration test could only reach them through commands that do not exist, and the
/// obvious workaround (a `#[cfg(test)]`-free `pub` accessor surface) would put test-only
/// methods on the production type forever. An integration test that had to take the
/// `tauri::AppHandle` to get at this would be testing less, not more.
///
/// # Why no `tauri::mock_builder`
///
/// Because it would give a handle with no webview, which is exactly the subset of behaviour
/// under test. It would look like an end-to-end test of the app while being a test of
/// SQLite.
#[cfg(test)]
mod document_file_tests {
    use super::*;

    fn core_at(path: &std::path::Path) -> DocumentCore {
        let store = holonomy_core::Store::open(path).expect("could not open the test store");
        DocumentCore {
            store: Mutex::new(store),
            geometry: Mutex::new(holonomy_core::Geometry::new()),
            document_id: Mutex::new(None),
            mounted: Mutex::new(None),
            path: Mutex::new(Some(path.to_path_buf())),
        }
    }

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("could not make a temp dir")
    }

    fn current_document_id(core: &DocumentCore) -> Option<String> {
        core.document_id.lock().unwrap().clone()
    }

    fn geometry_section_count(core: &DocumentCore) -> usize {
        core.geometry.lock().unwrap().len()
    }

    #[test]
    fn opening_a_file_with_a_document_in_it_reports_existing() {
        let dir = temp();
        let path = dir.path().join("novel.holo");
        {
            let store = holonomy_core::Store::open(&path).unwrap();
            store.create_document("Novel").unwrap();
            store.flush_all().unwrap();
        }

        let mut core = core_at(&dir.path().join("other.holo"));
        let report = core.open_path(&path).unwrap();

        assert_eq!(report.kind, "existing", "a file with a document in it is existing");
        assert_eq!(report.document.title, "Novel");
        assert_eq!(report.path, path.display().to_string());
        assert_eq!(current_document_id(&core).as_deref(), Some(report.document.id.as_str()));
    }

    #[test]
    fn opening_an_empty_but_valid_file_creates_a_document_and_says_so() {
        // The case most likely to be reported wrongly. A file that opens and contains no
        // documents is not an error — but reporting `existing` would be true and useless.
        let dir = temp();
        let path = dir.path().join("blank.holo");
        holonomy_core::Store::open(&path).unwrap();

        let mut core = core_at(&dir.path().join("other.holo"));
        let report = core.open_path(&path).unwrap();

        assert_eq!(report.kind, "empty");
        assert_eq!(report.document.title, "Untitled");
        assert_eq!(report.document.sections, 0);
    }

    #[test]
    fn opening_a_path_that_does_not_exist_creates_a_real_document_file() {
        let dir = temp();
        let path = dir.path().join("fresh.holo");

        let mut core = core_at(&dir.path().join("other.holo"));
        let report = core.open_path(&path).unwrap();

        assert_eq!(report.kind, "created");
        assert!(path.exists(), "the file was not created");
        // Not merely "a file appeared": something that is a document, per the same
        // classifier the OS-facing path uses.
        assert_eq!(
            holonomy_core::holo::probe(&path).unwrap(),
            holonomy_core::FileKind::Holonomy(holonomy_core::schema::SCHEMA_VERSION)
        );
    }

    #[test]
    fn switching_files_carries_nothing_across() {
        // The reason `DocumentCore` sits behind one lock rather than four. A geometry
        // seeded from document A while `document_id` names document B is a state where
        // `sync_section_heights` receives section ids the Fenwick tree has never heard of,
        // and the scrollbar's total becomes the sum of two documents. Every field is
        // internally consistent, so nothing inside the core can notice; the check has to
        // be made from outside, after the switch.
        let dir = temp();
        let first = dir.path().join("first.holo");
        let second = dir.path().join("second.holo");

        let mut first_sections = 0;
        for (path, title, sections) in [(&first, "First", 9usize), (&second, "Second", 3)] {
            let store = holonomy_core::Store::open(path).unwrap();
            let doc = store.create_document(title).unwrap();
            let mut ids = Vec::new();
            for _ in 0..sections {
                ids.push(store.add_section(
                    &doc.id,
                    &serde_json::json!({
                        "type": "doc",
                        "content": [{"type": "paragraph", "content": [
                            {"type": "text", "text": "Some words in this section."}
                        ]}]
                    }),
                ).unwrap());
            }
            store.flush_all().unwrap();
            if std::ptr::eq(path, &first) {
                first_sections = ids.len();
            }
        }

        let mut core = core_at(&first);
        let first_report = core.open_path(&first).unwrap();
        // Seed the geometry the way `get_document_boot` does, so the switch has something
        // stale to drop.
        *core.geometry.lock().unwrap() =
            holonomy_core::Geometry::from_manifest(&store_manifest(&first, &first_report.document.id));
        assert_eq!(geometry_section_count(&core), first_sections);

        let second_report = core.open_path(&second).unwrap();
        assert_ne!(first_report.document.id, second_report.document.id);

        assert_eq!(
            current_document_id(&core).as_deref(),
            Some(second_report.document.id.as_str()),
            "the remembered document id is the previous document's"
        );
        assert_eq!(
            geometry_section_count(&core),
            0,
            "the geometry still holds the previous document's sections, so the scrollbar's \
             total height would be the sum of two documents"
        );
    }

    fn store_manifest(
        path: &std::path::Path,
        document_id: &str,
    ) -> holonomy_core::Manifest {
        holonomy_core::Store::open(path)
            .unwrap()
            .manifest(document_id)
            .unwrap()
    }

    #[test]
    fn opening_a_non_document_leaves_the_previous_document_open() {
        // A refused open must not clear what was there. If it did, a double-click on a
        // screenshot would end the session: the document is still on disk, but the app has
        // forgotten where it was, and the next autosave would go to the wrong file.
        let dir = temp();
        let good = dir.path().join("good.holo");
        let original = {
            let store = holonomy_core::Store::open(&good).unwrap();
            let d = store.create_document("Still here").unwrap();
            store.flush_all().unwrap();
            d.id
        };

        let junk = dir.path().join("junk.holo");
        std::fs::write(&junk, b"not a database at all").unwrap();

        let mut core = core_at(&good);
        core.open_path(&good).unwrap();

        let err = core.open_path(&junk).expect_err("a non-document must not open");
        assert!(
            matches!(err, holonomy_core::Error::NotADocument { .. }),
            "got {err:?}"
        );

        assert_eq!(
            current_document_id(&core).as_deref(),
            Some(original.as_str()),
            "a refused open moved the session"
        );
        assert_eq!(core.current_path().unwrap(), good, "a refused open changed the file");
    }

    #[test]
    fn creating_over_an_existing_file_is_refused() {
        // The destructive case. "Save As…" onto a path that exists is how a document is
        // lost, and the OS overwrite prompt cannot be relied on: a file opened from a
        // command line never saw one.
        let dir = temp();
        let existing = dir.path().join("precious.holo");
        {
            let store = holonomy_core::Store::open(&existing).unwrap();
            store.create_document("Precious").unwrap();
            store.flush_all().unwrap();
        }

        let mut core = core_at(&dir.path().join("other.holo"));
        let err = core
            .create_path(&existing)
            .expect_err("creating over a file must be refused");
        let already_exists = matches!(
            err,
            holonomy_core::Error::Io(ref e) if e.kind() == std::io::ErrorKind::AlreadyExists
        );
        assert!(already_exists, "got {err:?}");

        let store = holonomy_core::Store::open(&existing).unwrap();
        assert_eq!(store.documents().unwrap()[0].title, "Precious");
    }

    #[test]
    fn creating_over_a_non_database_is_refused_by_the_same_rule() {
        // Same outcome, different reason, deliberately the *same* rule: the check is "is
        // anything there", not "is there something I could open". A rule that only
        // protected openable files would let a new document land on top of a spreadsheet.
        let dir = temp();
        let path = dir.path().join("spreadsheet.holo");
        std::fs::write(&path, b"PK\x03\x04 pretend zip").unwrap();

        let mut core = core_at(&dir.path().join("other.holo"));
        core.create_path(&path).expect_err("must be refused");
        assert_eq!(std::fs::read(&path).unwrap(), b"PK\x03\x04 pretend zip");
    }

    #[test]
    fn documents_are_listed_with_counts_they_actually_have() {
        let dir = temp();
        let path = dir.path().join("many.holo");
        {
            let store = holonomy_core::Store::open(&path).unwrap();
            let a = store.create_document("Has content").unwrap();
            for _ in 0..4 {
                store.add_section(
                    &a.id,
                    &serde_json::json!({
                        "type": "doc",
                        "content": [{"type": "paragraph", "content": [
                            {"type": "text", "text": "Some words in this section."}
                        ]}]
                    }),
                ).unwrap();
            }
            store.create_document("Empty").unwrap();
            store.flush_all().unwrap();
        }

        let core = core_at(&path);
        let docs = core.documents().unwrap();
        assert_eq!(docs.len(), 2);

        let filled = docs.iter().find(|d| d.title == "Has content").unwrap();
        let empty = docs.iter().find(|d| d.title == "Empty").unwrap();

        // The distinction a switcher exists to show. Both being zero would be a switcher
        // that cannot tell a written document from an emptied one, so this asserts the
        // counts came from the manifest rather than from a default.
        assert_eq!(filled.sections, 4, "a four-section document reported something else");
        assert!(filled.words > 0, "no words reported for a document with text in it");
        assert_eq!(empty.sections, 0);
        assert_eq!(empty.words, 0);
    }

    /// Create a symlink at `link` pointing to `real`, or say why it could not be done.
    ///
    /// # Why this is not one call
    ///
    /// `std::os::unix::fs::symlink` does not exist on Windows and
    /// `std::os::windows::fs::symlink_file` does not exist anywhere else, so the original
    /// `std::os::unix::fs::symlink(...).unwrap()` meant the `holonomy-shell` **lib test**
    /// target did not compile on `windows-latest` at all:
    ///
    /// ```text
    ///   error[E0433]: cannot find `unix` in `os`
    ///      --> crates\holonomy-shell\src\lib.rs:2060:18
    /// ```
    ///
    /// That is the whole `rust (windows-latest)` leg, gone, on a line that has nothing to do
    /// with Windows. A `#[cfg(unix)]` on the test would have compiled — and left the Windows
    /// leg asserting nothing about symlinks, which is the one platform where a document filed
    /// in two folders is likeliest.
    ///
    /// The failure is returned rather than unwrapped because on Windows creating a symlink
    /// needs `SeCreateSymbolicLinkPrivilege`, which an interactive user has and a service
    /// account may not. Unwrapping would turn a missing privilege into a red leg that looks
    /// like a product failure; the caller turns it into an explicit skip that says so.
    #[cfg(unix)]
    fn symlink_at(real: &std::path::Path, link: &std::path::Path) -> Result<(), String> {
        std::os::unix::fs::symlink(real, link).map_err(|e| e.to_string())
    }

    #[cfg(windows)]
    fn symlink_at(real: &std::path::Path, link: &std::path::Path) -> Result<(), String> {
        // `symlink_file` rather than `symlink_dir`: the target is a regular file, and asking
        // for a directory symlink to a file is an error on Windows rather than a guess.
        std::os::windows::fs::symlink_file(real, link).map_err(|e| e.to_string())
    }

    #[test]
    fn a_symlink_is_opened_under_the_name_that_was_asked_for() {
        // What happens when a document is filed in two folders. Both names address the same
        // bytes, so both must open — and the reported path is the one asked for, because
        // that is what the title bar shows and therefore what "Save" must write to.
        let dir = temp();
        let real = dir.path().join("real.holo");
        let link = dir.path().join("alias.holo");
        {
            let store = holonomy_core::Store::open(&real).unwrap();
            store.create_document("Shared").unwrap();
            store.flush_all().unwrap();
        }
        if let Err(why) = symlink_at(&real, &link) {
            // Skipped loudly rather than passed quietly. A test that cannot run has not
            // asserted anything, and the difference matters most on the platform where this
            // is most likely to happen.
            eprintln!("SKIPPED a_symlink_is_opened_under_the_name_that_was_asked_for: {why}");
            return;
        }

        let mut core = core_at(&dir.path().join("other.holo"));
        let report = core.open_path(&link).unwrap();
        assert_eq!(report.path, link.display().to_string());
        assert_eq!(report.kind, "existing");
        assert_eq!(core.current_path().unwrap(), link);
    }

    #[test]
    fn switching_away_flushes_the_document_being_left() {
        // The moment where work in flight becomes unreachable: the store holding it is
        // about to be dropped. The rows are not *lost* — the old file still has them and
        // folds them on its own next open — but the user was looking at that document, so
        // their last few seconds must appear to have been saved.
        let dir = temp();
        let first = dir.path().join("first.holo");
        let second = dir.path().join("second.holo");
        holonomy_core::Store::open(&second).unwrap();

        let mut core = core_at(&first);
        let section_id;
        {
            let store = core.store.lock().unwrap();
            let doc = store.create_document("Unflushed").unwrap();
            let section = store
                .add_section(
                    &doc.id,
                    &serde_json::json!({"type": "doc", "content": [{"type": "paragraph"}]}),
                )
                .unwrap();
            section_id = section.clone();
            // `add_section` writes a snapshot and leaves nothing pending, so a fixture
            // built from it cannot test a flush at all -- it proved nothing, and said so.
            // The WAL is only fed by `log_edit`, which is what `commit_section_edit` calls,
            // so that is what the fixture has to use.
            store
                .log_edit(
                    &doc.id,
                    &section,
                    &serde_json::json!({"type": "doc", "content": [
                        {"type": "paragraph", "content": [
                            {"type": "text", "text": "Typed but not yet flushed."}
                        ]}
                    ]}),
                    holonomy_core::SectionMetrics::new(5, 0, 27),
                    "Typed but not yet flushed.",
                )
                .unwrap();
            // Deliberately not flushed: the WAL has rows the section blob does not.
            assert!(
                store.documents_with_pending_wal().unwrap().contains(&doc.id),
                "the fixture did not leave anything pending, so this test proves nothing"
            );
        }

        core.open_path(&second).unwrap();

        // Re-read the first file directly, as a separate session would. The first version
        // of this asserted that pending rows *survived* the switch, on the reasoning that
        // "not lost" means "still in the log". That is backwards: `open_path` folds and
        // checkpoints the store it is leaving, so the correct end state is no pending rows
        // and the text in the section blob. The assertion now states what the user needs,
        // which is that their words are in the file.
        let reopened = holonomy_core::Store::open(&first).unwrap();
        assert!(
            reopened.documents_with_pending_wal().unwrap().is_empty(),
            "the store was dropped without folding: the pending rows were left for recovery"
        );
        let content = reopened.load_section(&section_id).unwrap();
        let text = serde_json::to_string(&content).unwrap();
        assert!(
            text.contains("Typed but not yet flushed."),
            "the last edit is not in the section blob of the file that was left behind: {text}"
        );
    }
}
