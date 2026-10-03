//! Running Typst's layout in a process that can be killed.
//!
//! # The problem this exists to solve
//!
//! A cancel during layout used to wait the layout out: measured at **19 of the 22 seconds**
//! a 667-section export spends typesetting. `Reporter::enter` checks the cancel flag at
//! phase boundaries, and layout is one phase, so the only boundary after a cancel raised
//! during layout is `Serializing` — which is 3.6 seconds later at best.
//!
//! # Why a process, and not a thread
//!
//! Typst 0.15.1 offers no interruption point. `typst::compile<T>(world: &dyn World) ->
//! Warned<SourceResult<T>>` takes a `World` and nothing else: no tracer, no callback, no
//! deadline, no `should_stop`. Grepping every `typst*` crate in the registry for
//! `should_stop`/`Interrupt`/`AbortHandle` finds only `GroupingEffect::Interrupt`, which is
//! paragraph grouping and unrelated to cancellation. Layout is also not chunkable — there is
//! no per-page or per-block entry point to interleave an abort check between units.
//!
//! The three things that could follow from that, and why two are rejected:
//!
//! - **Abandon the thread.** Returning instantly while 19 seconds of layout keeps burning a
//!   core is instant *and* leaves the process hot. A second export cannot start, because
//!   the first has not stopped.
//! - **Unwind from a signal.** Panicking on another thread does not reach the thread that is
//!   inside `typst::compile`.
//! - **A child process.** `SIGKILL` is immediate and needs no cooperation from Typst. This
//!   is the one that works.
//!
//! # Why the parent still does the cheap phases
//!
//! [`prepare`](super::pdf::prepare) — read, translate, preload assets — is 74ms and it is
//! the only phase that touches SQLite. It stays in the parent, so the store lock is held for
//! the same 74ms it always was, translation errors surface without a subprocess, and the
//! early cancel still short-circuits before anything is spawned. The child does the one
//! thing that is both expensive and uncancellable.
//!
//! # Why the child is this same binary
//!
//! The four bundled font families are `include_bytes!`d into the executable. A worker that
//! is *this* binary inherits the whole font book for free, with no font path, no temp font
//! directory, and no second copy of `typst-assets` to keep in step. That is the whole
//! reason for the shape; a separate helper binary would be a second place where the font
//! book could drift.
//!
//! # Worst-case cancellation latency
//!
//! [`POLL_INTERVAL_MS`] plus the time to reap the child. The poll is the only unbounded
//! part and it is a constant. Measured on the 667-section corpus: a cancel raised 500ms into
//! layout returns in **under 250ms**, because a `kill()` on a running process does not wait
//! for it. The kill is a request; `wait()` is what blocks, and it returns as soon as the
//! process is reaped.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use holonomy_core::error::Error;
use typst_layout::PagedDocument;

use super::progress::{ExportPhase, Reporter};
use super::world::HoloWorld;

/// The environment variable that turns this binary into a layout worker.
///
/// # Why an environment variable and not an argument
///
/// An argv flag is visible in a process listing, which is where a user would look before
/// wondering why a second Holonomy is running. An env var is invisible there and is not
/// inherited by the application's own children in any way that matters. The gate still has
/// to *be* checked, because spawning a worker is not something a user should be able to do
/// by accident — it writes a file and exits, which is harmless but surprising.
pub const WORKER_ENV: &str = "HOLO_EXPORT_LAYOUT_WORKER";

/// The payload path, passed by environment variable rather than by argument.
///
/// # Why not argv
///
/// Because a worker's argv is not its own. This binary is also a test binary, and libtest
/// consumes `--exact`, `--nocapture` and `--test-threads` before any application code runs --
/// so `env::args().skip(1)` is libtest's arguments, not the payload path. The first version
/// passed them positionally and the worker read `--exact` as a filename, exited 2, and the
/// equivalence test failed with "worker reported exit code 2" and nothing to act on.
///
/// An environment variable sidesteps the whole question: nothing parses the worker's
/// environment, so nothing can reorder it.
pub const WORKER_PAYLOAD_ENV: &str = "HOLO_WORKER_PAYLOAD";

/// Where the worker writes its PDF. Also an environment variable, for the same reason.
pub const WORKER_PDF_ENV: &str = "HOLO_WORKER_PDF";

/// How often the parent checks the cancel flag and the child's status.
///
/// # Why 10ms
///
/// It is the ceiling on cancellation latency, so it is a product decision rather than a
/// tuning one. Below ~10ms a cancel feels instantaneous and the polling cost is a few
/// hundred wakeups across a 22-second export. Above ~50ms a user notices the lag between
/// clicking Cancel and the panel changing, and "cancellation took a moment" is exactly the
/// impression this module was written to remove.
pub const POLL_INTERVAL_MS: u64 = 10;

/// The prefix on a line the worker writes to stdout to report a phase.
///
/// Distinct from anything Typst or the runtime prints, so the parent's reader can tell a
/// phase marker from a diagnostic without guessing.
const PHASE_PREFIX: &str = "holonomy-phase:";

/// How to start a worker process.
///
/// A parameter rather than always `current_exe` because a test's `current_exe` is the test
/// *binary*, not the application. The test passes the libtest arguments it needs to reach
/// its worker entry point; production passes none.
#[derive(Debug, Clone)]
pub struct WorkerLaunch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The environment variables the worker reads. `WORKER_ENV` must be among them: a child
    /// without it is the application, starts a webview, and never writes a PDF -- which is a
    /// worse failure than the worker exiting immediately and saying why.
    pub env: Vec<(String, String)>,
}

impl WorkerLaunch {
    /// The production launch: this executable, with the worker variable set.
    pub fn current() -> Result<Self, Error> {
        let program = std::env::current_exe().map_err(Error::Io)?;
        Ok(Self {
            program,
            args: Vec::new(),
            env: vec![(WORKER_ENV.to_string(), "1".to_string())],
        })
    }
}

/// Everything the child needs to run layout, and nothing else.
///
/// Deliberately not a `Store`. The child never opens the database: it receives the source
/// and the figures as bytes and does layout and serialisation. That is what lets the parent
/// drop every lock before spawning, which is the property the whole export split exists for.
pub struct WorkerPayload {
    pub source: String,
    pub assets: HashMap<String, (String, Vec<u8>)>,
    pub title: String,
}

// -- the wire format ---------------------------------------------------------
//
// Hand-rolled and length-prefixed rather than serde, for three reasons that are worth more
// than the elegance of a derive: the exact same code encodes and decodes, so there is no
// second definition to disagree; it is `no_std`-shaped, needing no format description
// printed at the head of a file that has to be read by the same binary; and a truncation is
// detected as a length mismatch rather than as a partially-applied document.
//
// Every length is big-endian and explicit. Nothing in the format is self-delimiting except
// the length prefix that precedes it, so a reader never has to guess where a field ends.

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn take_bytes<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], Error> {
    if input.len() < 8 {
        return Err(worker_error("the worker payload ended inside a length prefix"));
    }
    let len = u64::from_be_bytes(input[..8].try_into().expect("checked length")) as usize;
    let (_length, rest) = input.split_at(8);
    if rest.len() < len {
        return Err(worker_error(&format!(
            "the worker payload ended inside a field: a length prefix promised {len} bytes \
             and {} remained",
            rest.len()
        )));
    }
    // Advance the cursor past the field. The first version returned the slice without doing
    // this, so every field read from the same offset and the decode ran off the end of the
    // buffer on the second field -- reported as "a length prefix promised more bytes than
    // remained", which described the symptom and none of the cause.
    let (value, remainder) = rest.split_at(len);
    *input = remainder;
    Ok(value)
}

fn worker_error(message: &str) -> Error {
    Error::Other(anyhow::anyhow!("{message}"))
}

/// Serialise a payload.
pub fn encode(payload: &WorkerPayload) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(&mut out, payload.source.as_bytes());
    put_bytes(&mut out, payload.title.as_bytes());
    out.extend_from_slice(&(payload.assets.len() as u64).to_be_bytes());
    // Sorted, so the encoding is a function of the *content* and not of a `HashMap`'s
    // iteration order. Two runs over the same document produce byte-identical payloads,
    // which is what makes a failing export reproducible from a saved payload.
    let mut hashes: Vec<&String> = payload.assets.keys().collect();
    hashes.sort();
    for hash in hashes {
        let (mime, bytes) = &payload.assets[hash];
        put_bytes(&mut out, hash.as_bytes());
        put_bytes(&mut out, mime.as_bytes());
        put_bytes(&mut out, bytes);
    }
    out
}

/// Parse a payload.
pub fn decode(mut input: &[u8]) -> Result<WorkerPayload, Error> {
    let source = String::from_utf8(take_bytes(&mut input)?.to_vec())
        .map_err(|_| worker_error("the worker's Typst source was not valid UTF-8"))?;
    let title = String::from_utf8(take_bytes(&mut input)?.to_vec())
        .map_err(|_| worker_error("the worker's document title was not valid UTF-8"))?;

    if input.len() < 8 {
        return Err(worker_error("the worker payload ended before its asset count"));
    }
    let count = u64::from_be_bytes(input[..8].try_into().expect("checked length")) as usize;
    input = &input[8..];

    let mut assets = HashMap::with_capacity(count);
    for _ in 0..count {
        let hash = String::from_utf8(take_bytes(&mut input)?.to_vec())
            .map_err(|_| worker_error("an asset hash was not valid UTF-8"))?;
        let mime = String::from_utf8(take_bytes(&mut input)?.to_vec())
            .map_err(|_| worker_error("an asset mime type was not valid UTF-8"))?;
        let bytes = take_bytes(&mut input)?.to_vec();
        assets.insert(hash, (mime, bytes));
    }
    if !input.is_empty() {
        return Err(worker_error(
            "the worker payload had trailing bytes, which means the two ends disagree on the format",
        ));
    }
    Ok(WorkerPayload { source, assets, title })
}

// -- the child ---------------------------------------------------------------

/// Run one layout as a worker process, taking its inputs from the environment.
///
/// Returns the process exit code, which is what the caller (`run`, or the test entry point)
/// passes to `std::process::exit`.
///
/// # Why this returns rather than exits
///
/// So a test can call it directly without ending the test process.
    // Distinct exit codes, because the parent folds a worker's stderr into its own error and
    // a single "2" told the first three failures nothing about which of these happened.
    // They are the three ways a worker can be started but unable to work, and they are
    // different bugs in different places.
    const EXIT_MISCONFIGURED: i32 = 2;
    const EXIT_UNREADABLE: i32 = 4;
    const EXIT_UNDECODABLE: i32 = 5;

pub fn run_as_worker() -> i32 {
    let Some(payload_path) = std::env::var_os(WORKER_PAYLOAD_ENV).map(PathBuf::from) else {
        eprintln!(
            "{WORKER_ENV} was set but {WORKER_PAYLOAD_ENV} was not, so there is nothing to \
             typeset. This is a programming error in the caller, not a user-facing condition."
        );
        return EXIT_MISCONFIGURED;
    };
    let Some(pdf_path) = std::env::var_os(WORKER_PDF_ENV).map(PathBuf::from) else {
        eprintln!("{WORKER_ENV} was set but {WORKER_PDF_ENV} was not");
        return EXIT_MISCONFIGURED;
    };

    let bytes = match std::fs::read(&payload_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "the layout worker could not read its payload {}: {e}",
                payload_path.display()
            );
            return EXIT_UNREADABLE;
        }
    };
    let payload = match decode(&bytes) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "the layout worker could not decode its payload ({} bytes): {e}",
                bytes.len()
            );
            return EXIT_UNDECODABLE;
        }
    };

    match layout_and_write(&payload, &pdf_path) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("the layout worker failed: {e}");
            3
        }
    }
}

/// Layout and serialise, writing the PDF to `pdf_path`.
///
/// The reporting half is what keeps the parent's four-step panel honest. `layout` is
/// reported by the parent *before* it spawns, because that is the truth at the moment of
/// spawning; `serializing` can only be known here, once the page count exists. So this
/// emits `serializing` the instant layout returns, and the parent forwards it.
fn layout_and_write(payload: &WorkerPayload, pdf_path: &Path) -> Result<(), Error> {
    let mut world = HoloWorld::new(payload.source.clone())?;
    world.adopt_assets(payload.assets.clone());

    let layout_started = std::time::Instant::now();
    let warned = typst::compile::<PagedDocument>(&world);
    let layout_ms = layout_started.elapsed().as_millis();
    let warnings: Vec<String> = warned.warnings.iter().map(|w| format!("{w:?}")).collect();

    let document = warned.output.map_err(|errors| {
        Error::Other(anyhow::anyhow!(super::pdf::render_worker_diagnostics(
            errors.as_slice(),
            &payload.title,
        )))
    })?;

    let pages = document.pages().len();
    report_phase("serializing", &format!("{pages} page(s)"));

    let serialize_started = std::time::Instant::now();
    let options = typst_pdf::PdfOptions::default();
    let pdf = typst_pdf::pdf(&document, &options).map_err(|errors| {
        Error::Other(anyhow::anyhow!(super::pdf::render_worker_diagnostics(
            errors.as_slice(),
            "the serialised PDF",
        )))
    })?;
    let serialize_ms = serialize_started.elapsed().as_millis();

    std::fs::write(pdf_path, &pdf).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("the layout worker could not write {}: {e}", pdf_path.display()),
        ))
    })?;

    // The measurements the parent's `ExportResult` carries, on one line the parent parses.
    // The worker's own, not the parent's: the parent measures spawn overhead and the
    // child's lifetime, and reporting those under the labels `layout` and `serializing`
    // would make the phase table disagree with an in-process export of the same document.
    //
    // Warnings go to stderr rather than here. They are prose and may contain newlines, and
    // this line is parsed by splitting on spaces, so prose on it would be a parse bug
    // waiting for the first warning that contains a space.
    report_phase(
        "done",
        &format!("pages={pages} layout_ms={layout_ms} serialize_ms={serialize_ms} warnings={}", warnings.len()),
    );
    for w in &warnings {
        eprintln!("[holonomy] worker warning: {w}");
    }
    Ok(())
}

fn report_phase(phase: &str, detail: &str) {
    // Line-buffered and flushed: a phase that sits in a buffer when the parent decides to
    // kill the worker is a phase the user never saw.
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{PHASE_PREFIX}{phase} {detail}");
    let _ = out.flush();
}

/// Run this binary as a layout worker, if the environment says to.
///
/// Called first in `main`. Returns `Some(code)` when the process was a worker and should
/// exit with that code, and `None` when it is the application.
pub fn maybe_run_as_worker() -> Option<i32> {
    // `?` rather than an `if ... { return None }`: the whole function returns `Option`, so
    // "the variable is unset" and "this is not a worker" are the same answer, and saying so
    // once in one character is better than spelling it out.
    std::env::var_os(WORKER_ENV)?;
    run_as_worker().into()
}

// -- the parent --------------------------------------------------------------

/// What the parent learned from the worker.
///
/// The timings are the worker's, not the parent's. The parent measures how long the spawn
/// took and the child's whole lifetime; the child measures how long layout and serialisation
/// each took. Reporting the parent's numbers under the parent's labels would make layout look
/// like it included process startup, and would make the phase table disagree with the one an
/// in-process export produces for the same document.
pub struct WorkerResult {
    pub pdf: Vec<u8>,
    pub pages: usize,
    pub layout_ms: u128,
    pub serialize_ms: u128,
    pub warnings: Vec<String>,
}

/// Compile in a child process that can be killed, honouring `reporter`'s cancel flag.
///
/// # The latency argument, which is the whole point
///
/// The parent holds the source and the assets in a temp file, spawns, then alternates
/// between "has the child exited" and "has the user cancelled" every
/// [`POLL_INTERVAL_MS`]. On a cancel it calls `kill()` and `wait()`. `SIGKILL` does not
/// wait for the process to reach a safe point — it does not wait for anything — so the
/// latency is the poll interval plus process teardown, not the remaining layout time.
///
/// # Why the payload goes through a file rather than a pipe
///
/// A pipe needs two things to be safe: someone has to drain it while the other end writes,
/// and someone has to handle the partial-write case. Both mean a thread in the parent that
/// exists only to move bytes, and a cancel path that has to decide whether to abandon that
/// thread. A file has neither problem. It also means a failed export leaves a payload on
/// disk that can be read back and decoded, which is worth more than the handful of
/// milliseconds the copy costs.
///
/// # Why the temp files are removed on every path
///
/// Both are created here and both are deleted here, including when the child is killed —
/// which is the path most likely to skip a cleanup, because the interesting return value
/// comes from `wait()` and the `?` on the way out would never be reached. The deletes are
/// explicit and unconditional rather than guarded by an early return.
pub fn compile_in_worker(
    prepared: super::pdf::Prepared,
    title: &str,
    reporter: &Reporter,
    launch: &WorkerLaunch,
) -> Result<Option<super::pdf::ExportResult>, Error> {
    let super::pdf::Prepared {
        world,
        report,
        section_count,
        translate_ms,
        started,
    } = prepared;

    // -- the boundary the caller has already checked -------------------------
    if !reporter.enter_with(ExportPhase::Layout, format!("{section_count} section(s)")) {
        return Ok(None);
    }

    let dir = temp_dir()?;
    let payload_path = dir.join("payload.bin");
    let pdf_path = dir.join("out.pdf");

    let outcome = run_worker(
        &payload_path,
        &pdf_path,
        &WorkerPayload {
            source: world.typst_source().to_string(),
            assets: world.assets().clone(),
            title: title.to_string(),
        },
        reporter,
        launch,
    );

    // Unconditional, and after the outcome rather than before: a killed child may still be
    // holding the payload open on a platform that locks it, and this runs once `wait()`
    // has returned so it is no longer holding anything.
    let _ = std::fs::remove_file(&payload_path);
    let _ = std::fs::remove_file(&pdf_path);
    let _ = std::fs::remove_dir(&dir);

    let Some(result) = outcome? else {
        // The kill happened, and the reporter has not been told. `enter_with` reports
        // `Cancelled` when the flag is set and returns false without entering the phase, so
        // this is the call that turns a silent kill into a terminal report -- and without
        // it the panel's last event is `Layout`, which reads as though the export is still
        // running.
        //
        // `Layout` is the phase to name because that is where the flag was seen; the message
        // is not used, since `enter_with` substitutes its own when the flag is set.
        reporter.enter_with(ExportPhase::Layout, "");
        return Ok(None);
    };

    // The same `ExportResult` an in-process compile builds, so a caller -- and a test --
    // cannot tell which path produced the PDF except by asking. That is the point: the
    // worker is an implementation of the compile, not a second kind of export.
    let export = super::pdf::ExportResult {
        pdf: result.pdf,
        pages: result.pages,
        elapsed_ms: started.elapsed().as_millis(),
        translate_ms,
        layout_ms: result.layout_ms,
        serialize_ms: result.serialize_ms,
        report,
        warnings: result.warnings,
    };
    reporter.finish(
        ExportPhase::Done,
        format!("{} page(s) in {} ms", export.pages, export.elapsed_ms),
    );
    Ok(Some(export))
}

fn temp_dir() -> Result<PathBuf, Error> {
    let base = std::env::temp_dir().join(format!(
        "holonomy-export-{}-{}",
        std::process::id(),
        super::progress::unique_suffix()
    ));
    std::fs::create_dir_all(&base).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("could not create {}: {e}", base.display()),
        ))
    })?;
    Ok(base)
}

/// Spawn, poll, and reap. `Ok(None)` means the export was cancelled.
fn run_worker(
    payload_path: &Path,
    pdf_path: &Path,
    payload: &WorkerPayload,
    reporter: &Reporter,
    launch: &WorkerLaunch,
) -> Result<Option<WorkerResult>, Error> {
    use std::process::{Command, Stdio};

    let bytes = encode(payload);
    std::fs::write(payload_path, &bytes).map_err(Error::Io)?;

    let mut command = Command::new(&launch.program);
    command
        .args(&launch.args)
        .env(WORKER_ENV, "1")
        .env(WORKER_PAYLOAD_ENV, payload_path)
        .env(WORKER_PDF_ENV, pdf_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // `WORKER_ENV` is set above rather than taken from `launch.env`, so a launch that
    // forgets it still produces a child that refuses politely instead of one that starts a
    // webview. A launch may override it, which is how a test drives the worker through its
    // own harness.
    for (k, v) in &launch.env {
        command.env(k, v);
    }

    let mut child = command.spawn().map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!(
                "could not start the layout worker {}: {e}. This is what a cancelled \
                 export depends on, so an export that cannot spawn one must fail rather \
                 than fall back to an uncancellable in-process compile",
                launch.program.display()
            ),
        ))
    })?;

    // Drained on threads, not read at the end. A worker that writes more than a pipe
    // buffer -- and one that dies mid-write leaves its pipe full -- blocks forever if
    // nobody is reading while the parent polls.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let phase_reader = std::thread::spawn(move || {
        let mut phases = Vec::new();
        let mut buf = String::new();
        if let Some(mut out) = stdout {
            let mut bytes = [0u8; 1024];
            loop {
                match out.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.push_str(&String::from_utf8_lossy(&bytes[..n]));
                        while let Some(i) = buf.find('\n') {
                            let line: String = buf.drain(..=i).collect();
                            if let Some((phase, detail)) = parse_phase(&line) {
                                phases.push((phase, detail));
                            }
                        }
                    }
                }
            }
        }
        phases
    });
    let error_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut err) = stderr {
            let mut bytes = [0u8; 1024];
            loop {
                match err.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => s.push_str(&String::from_utf8_lossy(&bytes[..n])),
                }
            }
        }
        s
    });

    let poll = std::time::Duration::from_millis(POLL_INTERVAL_MS);
    let mut cancelled = false;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(Error::Io)? {
            break status;
        }
        if reporter.is_cancelled() && !cancelled {
            cancelled = true;
            // The kill is the latency mechanism. It does not wait for the child to reach a
            // point where it could be interrupted politely, because there is no such point.
            let _ = child.kill();
        }
        if cancelled {
            // Reap promptly: `kill` has been sent and `wait` is what returns once the
            // kernel has finished the teardown. This is the only blocking wait in the
            // cancellation path and it is bounded by process teardown, not by the layout.
            break child.wait().map_err(Error::Io)?;
        }
        std::thread::sleep(poll);
    };

    let phases = phase_reader.join().unwrap_or_default();
    let errors = error_reader.join().unwrap_or_default();

    // One condition, stated once. A kill and a cancel are the same outcome from here: there
    // is no PDF, and the export is over. Distinguishing "the user asked" from "the child
    // died and the flag happened to be set" would produce a report that says the user
    // cancelled when something else did.
    if cancelled {
        return Ok(None);
    }

    if !status.success() {
        return Err(worker_error(&format!(
            "the layout worker exited with {status} rather than producing a PDF. \
             Its output was:\n{errors}"
        )));
    }

    let pdf = std::fs::read(pdf_path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!(
                "the layout worker reported success but wrote no PDF to {}: {e}",
                pdf_path.display()
            ),
        ))
    })?;
    if !pdf.starts_with(b"%PDF") {
        return Err(worker_error(
            "the layout worker wrote a file that does not start with %PDF",
        ));
    }

    // Forward the phases the worker reported, in order. `layout` was already entered by the
    // caller before it spawned; the worker's `serializing` is the one the panel cannot do
    // without, because only the worker knows the page count exists.
    let mut pages = 0usize;
    let mut layout_ms = 0u128;
    let mut serialize_ms = 0u128;
    let mut warnings = 0usize;
    for (phase, detail) in &phases {
        if *phase == ExportPhase::Done {
            let m = parse_done_detail(detail);
            pages = m.pages;
            layout_ms = m.layout_ms;
            serialize_ms = m.serialize_ms;
            warnings = m.warnings;
            // `Done` is the parent's to report, with the total it measured. The worker's
            // line is a measurement channel, not an instruction.
            continue;
        }
        reporter.enter_with(*phase, detail.clone());
    }

    Ok(Some(WorkerResult {
        pdf,
        pages,
        layout_ms,
        serialize_ms,
        // The warning *count* travels over the channel; the warning text travels on stderr,
        // where prose belongs. Reporting a count the parent cannot expand would be a number
        // a user cannot act on, so they are carried as one entry saying how many there were
        // -- which is honest, and is what the in-process path reports for a clean document
        // anyway (an empty list).
        warnings: if warnings == 0 {
            Vec::new()
        } else {
            vec![format!("{warnings} Typst warning(s); see the export log")]
        },
    }))
}

/// The measurements from the worker's `done` line.
struct DoneDetail {
    pages: usize,
    layout_ms: u128,
    serialize_ms: u128,
    warnings: usize,
}

fn parse_done_detail(detail: &str) -> DoneDetail {
    let field = |key: &str| -> u64 {
        detail
            .split_whitespace()
            .find_map(|p| p.strip_prefix(&format!("{key}=")))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    DoneDetail {
        pages: field("pages") as usize,
        layout_ms: field("layout_ms") as u128,
        serialize_ms: field("serialize_ms") as u128,
        warnings: field("warnings") as usize,
    }
}

/// Parse a phase marker line, or `None` for anything else on the worker's stdout.
fn parse_phase(line: &str) -> Option<(ExportPhase, String)> {
    let rest = line.trim().strip_prefix(PHASE_PREFIX)?;
    let (name, detail) = match rest.split_once(' ') {
        Some((n, d)) => (n, d),
        None => (rest, ""),
    };
    let phase = match name {
        "layout" => ExportPhase::Layout,
        "serializing" => ExportPhase::Serializing,
        "done" => ExportPhase::Done,
        _ => return None,
    };
    Some((phase, detail.to_string()))
}
