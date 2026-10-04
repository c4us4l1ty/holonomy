//! The Holonomy binary: the boot sequence and the session.
//!
//! # The boot order is load-bearing
//!
//! ```text
//!   seal core dumps -> raise RLIMIT_MEMLOCK -> install tripwires
//!     -> open every descriptor            <- the last point a file can be named
//!     -> mlockall(MCL_CURRENT|MCL_FUTURE) -> isolate the network
//!     -> drop privileges (no_new_privs)   -> install the seccomp filter
//! ```
//!
//! Each arrow is a distinct Rust type, so the order is a compile error to get wrong: there is no
//! method on `Opened` that skips `lock_all_pages`, and no way to reach `Sealed` except through
//! `PrivilegesDropped::seal`.
//!
//! # Every descriptor is opened at stage 4, and nothing opens a path after
//!
//! The filter contains no `openat` -- see `holonomy_jail::seccomp::table::ALLOWLIST` -- and it is
//! installed at stage 8. So stage 4 is the last moment a *path* can become a descriptor, and the
//! [`open_descriptors`] closure is where the container and both export files are opened. Reaching
//! for a path afterwards is not a slow failure, it is `SIGSYS` and exit 137, which reads like a
//! crash rather than like a design rule.
//!
//! That is also why the session is handed `File`s and not paths: [`session::Session`] has no way to
//! open anything, so there is nothing for it to get wrong.
//!
//! # `--headless` skips the chain, and says so
//!
//! The jail cannot be entered from a `#[test]`: `unshare(CLONE_NEWUSER)` returns `EINVAL` in any
//! multi-threaded process and libtest always spawns one. So the integration gate drives
//! [`session::Session`] directly, headless, and this file's chain is covered by `holonomy-jail`'s own
//! 60 tests plus the census in `examples/census_session.rs`. `--headless` is therefore not a
//! convenience flag: it is the mode that says "run the loop without the jail", and it is the only
//! mode that is honest about what it has not sealed.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use holonomy::args::{Args, ParseError};
use holonomy::session::{ExportSink, Session};
use holonomy_container::io::DirectFile;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_export::Format;
use holonomy_input::{EvdevSource, ScriptedInputSource};
use holonomy_jail::{Action, AltStack, Enter, ALT_STACK_BYTES};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_secure::SecureBlock;
use holonomy_text::Editor;

/// Everything stage 4 built or opened, threaded through the boot and handed to the session.
///
/// **The session lives in here, not in `main`.** It borrows the atlas, so it cannot be constructed
/// inside `run_session` without either leaking a borrow past the closure or rebuilding the atlas
/// after `seccomp` -- and the second is a `mmap` the filter does not permit. Carrying it as the boot
/// context means it is *built* before the jail and *run* inside it, which is exactly the discipline:
/// nothing in the session allocates, and nothing in it opens anything.
///
/// A first version built the session inside `run_session` and dropped it there, so the sealed path
/// never actually ran a session. It compiled, and it would have shipped a binary that sealed itself
/// and exited.
struct SessionContext {
    /// The editor and its loop.
    session: Session<'static>,
    /// The container, opened `O_DIRECT | O_SYNC`.
    #[allow(dead_code)]
    container: DirectFile,
    /// The export sinks, opened `O_WRONLY | O_CREAT | O_TRUNC`.
    sinks: Vec<ExportSink>,
    /// A PPM dump target, if asked for.
    screenshot: Option<File>,
}

/// The atlas, built once. Leaked so the session's lifetime is not tied to a local.
static ATLAS: std::sync::OnceLock<holonomy_assets::atlas::Atlas> = std::sync::OnceLock::new();

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail::Usage(e)) => {
            eprintln!("holonomy: {e}\n\n{}", Args::USAGE);
            ExitCode::from(2)
        }
        Err(Fail::Boot(e)) => {
            eprintln!("holonomy: the boot sequence refused: {e}");
            ExitCode::from(3)
        }
        Err(Fail::Session(e)) => {
            eprintln!("holonomy: {e}");
            ExitCode::from(4)
        }
    }
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(e) => write!(f, "{e}"),
            Self::Boot(e) => write!(f, "{e}"),
            Self::Session(e) => write!(f, "{e}"),
        }
    }
}

/// The keyboard. Opened at boot, before the filter, or not at all.
const EVDEV_NODE: &str = "/dev/input/event0";

/// Lift an `io::Error` into a [`Fail`].
///
/// A one-liner rather than four call sites of `map_err(|e| Fail::Session(e.into()))`, and the reason
/// it is a *function* rather than a `From` impl is that `Fail` is a `main`-local type: an orphan rule
/// would otherwise force it into the library, where it does not belong.
fn io(e: std::io::Error) -> Fail {
    Fail::Session(e.into())
}

/// Why `main` stopped.
#[derive(Debug)]
enum Fail {
    /// The command line was wrong.
    Usage(ParseError),
    /// The boot chain refused.
    Boot(holonomy_jail::JailError),
    /// The session failed.
    Session(holonomy::session::SessionError),
}

/// The passphrase, from the environment.
///
/// Not from the command line: an argument is in `/proc/*/cmdline`, readable by every process on the
/// machine. Absent is an error rather than an empty passphrase, because an empty passphrase that
/// silently derives a working key is the worst possible failure for an encrypted container.
fn passphrase() -> Result<String, Fail> {
    std::env::var("HOLONOMY_PASSPHRASE").map_err(|_| {
        Fail::Session(holonomy::session::SessionError::Sink(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "$HOLONOMY_PASSPHRASE is unset; an empty passphrase is not a default",
        )))
    })
}

fn run() -> Result<(), Fail> {
    let args = Args::parse(std::env::args().skip(1)).map_err(Fail::Usage)?;

    if args.dry_run {
        print_plan(&args);
        return Ok(());
    }

    // Headless is the gate's mode and the only one this file can honestly run in a test binary, so it
    // is the default when no hardware is present rather than an error.
    let headless = args.headless || cfg!(not(feature = "hardware"));

    // The atlas is the largest allocation the session makes, and it is made *before* the jail so
    // that nothing after `seccomp` has to touch the allocator.
    let atlas = ATLAS.get_or_init(|| {
        holonomy_assets::build_atlas(&[16])
            .expect("the packed faces must rasterise")
            .0
    });
    let metrics = ChromeMetrics {
        width: args.width,
        height: args.height,
        ..ChromeMetrics::DESKTOP
    };

    if headless {
        let mut sinks = open_sinks(&args).map_err(io)?;
        let mut screenshot = open_screenshot(&args).map_err(io)?;
        let mut s = Session::new(
            Editor::new(),
            Painter::new(atlas, 0),
            HeadlessScanout::new(metrics.width, metrics.height),
            metrics,
        );
        s.state.zoom_percent = args.zoom;
        s.state.sealed = false;
        s.repaint_all().map_err(Fail::Session)?;
        drive(&mut s, &args, &mut sinks, screenshot.as_mut())?;
        return Ok(());
    }

    // --- The boot chain. Everything that names a path happens inside stage 4.
    //
    // The alternate signal stack must be a *registered* `SecureBlock`, not a `Vec<u8]`: the whole
    // point of the tripwire is that the signal frame the kernel pushes onto it is scrubbed by the
    // handler that is about to write to it, and an unregistered stack's bytes survive the process.
    // So the block is allocated here and **kept alive across the whole chain** -- dropping it early
    // would leave `sigaltstack` pointing at unmapped memory, which is a segfault on the first
    // fault rather than a clean report.
    let alt_block =
        SecureBlock::allocate(ALT_STACK_BYTES).expect("allocate the alternate signal stack");
    let alt = AltStack::install(alt_block.as_ptr() as usize, ALT_STACK_BYTES)
        .expect("install the alternate signal stack");

    let chain = Enter
        .seal_core_dumps()
        .raise_memlock()
        .install_tripwires(alt)
        .map_err(Fail::Boot)?;
    let opened = chain
        .open_descriptors(|| {
            Ok(SessionContext {
                session: Session::new(
                    Editor::new(),
                    Painter::new(atlas, 0),
                    HeadlessScanout::new(metrics.width, metrics.height),
                    metrics,
                ),
                container: DirectFile::create_or_open(
                    args.container
                        .as_deref()
                        .unwrap_or_else(|| Path::new("untitled.wavefunction")),
                )
                .expect("open the container"),
                sinks: open_sinks(&args).expect("open the export sinks"),
                screenshot: open_screenshot(&args).expect("open the screenshot"),
            })
        })
        .map_err(Fail::Boot)?;

    let mut sealed = opened
        .lock_all_pages()
        .isolate_network()
        .drop_privileges()
        .map_err(Fail::Boot)?
        .seal(Action::KillProcess)
        .map_err(Fail::Boot)?;

    let report = sealed.report();
    eprintln!(
        "holonomy: sealed. mlockall={} (errno {}), no_new_privs={}, ASLR={:?}",
        report.all_pages_locked, report.mlock_errno, report.no_new_privs, report.aslr
    );

    // The passphrase is read *after* sealing, which is why it has to come from the environment: there
    // is no `open` and no `getenv` guarantee post-filter, and the value must already be in the
    // process's own memory to be scrubbed on the way out.
    let phrase = passphrase()?;

    sealed.run_session(|sealed| {
        let ctx = sealed.context_mut();
        ctx.session.state.sealed = true;
        ctx.session.state.zoom_percent = args.zoom;
        // The first paint is a *full* repaint, because nothing has been painted yet and a
        // damage-limited pass would leave the framebuffer black.
        ctx.session.repaint_all().expect("the first paint");
        drive(
            &mut ctx.session,
            &args,
            &mut ctx.sinks,
            ctx.screenshot.as_mut(),
        )
        .unwrap_or_else(|e| {
            eprintln!("holonomy: {e}");
        });
    });
    let _ = &phrase;

    sealed.teardown_and_exit(-1);
}

/// Run the session to completion and write the sinks.
fn drive(
    s: &mut Session<'_>,
    args: &Args,
    sinks: &mut [ExportSink],
    screenshot: Option<&mut File>,
) -> Result<(), Fail> {
    let exit = match &args.script {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(io)?;
            let mut src = ScriptedInputSource::new(&bytes);
            s.run(&mut src).map_err(Fail::Session)?
        }
        None => {
            let mut src =
                EvdevSource::open(Path::new(EVDEV_NODE)).map_err(|e| Fail::Session(e.into()))?;
            s.run(&mut src).map_err(Fail::Session)?
        }
    };
    eprintln!(
        "holonomy: {exit:?} after {} commands, {} edits, {} frames",
        s.stats.commands, s.stats.edits, s.stats.frames
    );

    s.repaint_all().map_err(Fail::Session)?;

    for sink in sinks.iter_mut() {
        let report = s.export(sink, "holonomy").map_err(Fail::Session)?;
        eprintln!(
            "holonomy: wrote {} bytes of {} to {}",
            report.bytes,
            Format::extension(sink.format),
            sink.path
        );
    }

    if let Some(f) = screenshot {
        let n = s
            .scanout()
            .dump_to_file(f)
            .map_err(|e| Fail::Session(e.into()))?;
        f.flush().map_err(io)?;
        eprintln!("holonomy: wrote a {n}-byte PPM");
    }
    Ok(())
}

/// Open every export sink. **Before** the jail, always.
fn open_sinks(args: &Args) -> std::io::Result<Vec<ExportSink>> {
    args.exports
        .iter()
        .map(|t| {
            Ok(ExportSink {
                format: t.format,
                file: File::create(&t.path)?,
                path: t.path.display().to_string(),
            })
        })
        .collect()
}

/// Open the PPM target, if there is one.
fn open_screenshot(args: &Args) -> std::io::Result<Option<File>> {
    args.screenshot.as_deref().map(File::create).transpose()
}

/// Print the boot plan and exit, having opened nothing.
///
/// Useful because the plan is the thing worth reviewing: if `--dry-run` and a real run disagree,
/// the bug is in the plan.
fn print_plan(args: &Args) {
    println!("holonomy --dry-run");
    println!("  container : {:?}", args.container.as_deref());
    println!("  panel     : {}x{}", args.width, args.height);
    println!("  zoom      : {}%", args.zoom);
    println!("  exports   :");
    for t in &args.exports {
        println!(
            "    {} -> {}",
            Format::extension(t.format),
            t.path.display()
        );
    }
    println!("  screenshot: {:?}", args.screenshot.as_deref());
    println!("  script    : {:?}", args.script.as_deref());
    println!();
    println!("boot order:");
    println!("  1. seal core dumps          (RLIMIT_CORE=0, PR_SET_DUMPABLE=0)");
    println!("  2. raise RLIMIT_MEMLOCK     soft -> hard");
    println!("  3. install tripwires        SIGSEGV, SIGBUS, SIGSYS, SIGILL, SIGFPE");
    println!("  4. open every descriptor    <<< the last point a path can be named");
    println!("  5. mlockall                 MCL_CURRENT | MCL_FUTURE");
    println!("  6. isolate the network      CLONE_NEWUSER | CLONE_NEWNET");
    println!("  7. drop privileges          PR_SET_NO_NEW_PRIVS");
    // Read from the table rather than hardcoded. Phase 7's census named 50 entries, and a banner
    // that says 50 while the table grows to 52 is a banner that is quietly wrong about the machine's
    // own security posture -- which is the one number here that must never drift.
    println!(
        "  8. install seccomp          {} entries",
        holonomy_jail::seccomp::table::ALLOWLIST.len()
    );
    println!();
    println!("after stage 8: no open, no mmap, no brk. everything is already resident.");
}
