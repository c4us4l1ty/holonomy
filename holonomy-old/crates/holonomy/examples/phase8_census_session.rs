//! The Phase 8 syscall census: the **real session loop** under a seccomp filter.
//!
//! # Why this exists separately from `holonomy-jail`'s own census example
//!
//! Phase 7's `census_session.rs` exercises every *subsystem* the session is built from, so that a
//! later addition cannot silently need a syscall nobody measured. But it is not the session: it is
//! a hand-written workload that touches a container, an `Editor`, the geometry, the surface tree and
//! an `epoll` pipe.
//!
//! `seccomp::table::ALLOWLIST` says in its own docs that Phase 8 re-runs this against the real loop
//! and that the check is not optional. This is that check. It is Phase 7's explicit debt.
//!
//! # Why an example and not a `#[test]`
//!
//! `unshare(CLONE_NEWUSER)` returns `EINVAL` in any multi-threaded process and libtest always
//! spawns a thread. So the boot chain can only be entered from a single-threaded `main`, which is
//! what an example target is.
//!
//! # How it is driven
//!
//! Environment, because the parent is a test binary and the child is a separate process:
//!
//! * `HOLONOMY_CENSUS_ACTION` -- `trap`, `kill` or `kill-process`. Default `trap`.
//! * `HOLONOMY_CENSUS_DROP` -- comma-separated allowlist entry names to **remove**, so the harness
//!   can prove it can detect a gap.
//! * `HOLONOMY_CENSUS_DIR` -- a writable directory for the container, the exports and the PPM.
//!
//! The census handler is installed **unconditionally**, before the filter, in every mode. A census
//! whose syscall path differs from the production run by an `rt_sigaction` is measuring a different
//! program -- which is exactly the kind of difference that makes a census wrong.
//!
//! # What this workload does and does not cover
//!
//! It drives the real [`holonomy::session::Session`]: the real `Editor`, the real `Chrome`, the real
//! atlas, the real painter, the real `ScriptedInputSource` decoder, and a real export to a
//! descriptor opened at boot stage 4.
//!
//! It does **not** cover DRM, a real keyboard, the KDF, or the ring's staged commit. Those are the
//! Phase 9 debts, and the honest statement is that this census closes the *loop's* syscall surface,
//! not the whole program's.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use holonomy::session::{ExportSink, Session};
use holonomy_assets::atlas::Atlas;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_export::Format;
use holonomy_input::{InputEvent, ScriptedInputSource};
use holonomy_jail::seccomp::table::{self, Allowed};
use holonomy_jail::{Action, AltStack, Enter, ALT_STACK_BYTES};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_secure::SecureBlock;
use holonomy_text::Editor;

/// Printed on success, so a parent can tell "the session ran" from "it exited 0 without running".
const OK_MARKER: &str = "CENSUS-OK ";
/// Identifies *this* program, checked by the parent before it measures anything.
///
/// Not paranoia. `holonomy-jail` has an example of the same name, cargo writes both to the same
/// `target/<profile>/examples/` path, and the last one built wins -- non-deterministically, because
/// it depends on which crate cargo happened to compile last. A full-workspace run therefore executed
/// the *jail's* workload and reported its failure as this phase's: `session.fail unknown action ""`.
/// A rename fixes it; the marker is what stops it coming back.
const WHOAMI: &str = "phase8-session-loop";
/// Printed when the workload itself failed, as opposed to the filter catching a syscall.
const FAIL_MARKER: &str = "CENSUS-FAIL ";
/// Printed before the boot, so a wrong binary is caught before the seal rather than after.
///
/// Everything stage 4 opened. Carried as the boot context so nothing after the filter can open.
struct SessionContext {
    session: Session<'static>,
    html: File,
    pdf: File,
    ppm: File,
}

impl SessionContext {
    /// Stage 4: the last point a path can become a descriptor.
    fn open() -> Result<Self, String> {
        let dir = PathBuf::from(std::env::var("HOLONOMY_CENSUS_DIR").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("holonomy-census")
                .display()
                .to_string()
        }));
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

        // Everything the session will touch, allocated *now*.
        let atlas: &'static Atlas = Box::leak(Box::new(
            holonomy_assets::build_atlas(&[16])
                .map_err(|e| format!("build the atlas: {e}"))?
                .0,
        ));
        let container = File::create(dir.join("census.wavefunction"))
            .map_err(|e| format!("open the container: {e}"))?;
        drop(container); // The workload's write goes through the session's own sinks.

        let m = ChromeMetrics::DESKTOP;
        let session = Session::new(
            Editor::new(),
            Painter::new(atlas, 0),
            Box::new(HeadlessScanout::new(m.width, m.height)),
            m,
        );
        Ok(Self {
            session,
            html: File::create(dir.join("census.html")).map_err(|e| format!("html: {e}"))?,
            pdf: File::create(dir.join("census.pdf")).map_err(|e| format!("pdf: {e}"))?,
            ppm: File::create(dir.join("census.ppm")).map_err(|e| format!("ppm: {e}"))?,
        })
    }
}

fn main() {
    // First thing, before anything is sealed or measured.
    println!("{WHOAMI}");

    let action = match std::env::var("HOLONOMY_CENSUS_ACTION")
        .unwrap_or_default()
        .as_str()
    {
        "trap" => Action::Trap,
        "kill" => Action::Kill,
        "kill-process" => Action::KillProcess,
        other => fail(format!("unknown action {other:?}")),
    };
    let dropped: Vec<String> = std::env::var("HOLONOMY_CENSUS_DROP")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let entries: Vec<Allowed> = table::ALLOWLIST
        .iter()
        .copied()
        .filter(|entry| !dropped.iter().any(|name| name == entry.name))
        .collect();
    // Refuse an ambiguous drop rather than silently measuring with a table that is not what the
    // caller asked for: a typo in a name would otherwise produce a *complete* run and a green
    // result from a harness that never removed anything.
    if entries.len() + dropped.len() != table::ALLOWLIST.len() {
        fail(format!(
            "each dropped name must match exactly one allowlist entry; dropped={dropped:?}"
        ));
    }

    // Installed in every mode, before the filter: the filter is what delivers SIGSYS.
    if let Err(e) = holonomy_jail::census::install_handler(2) {
        fail(format!("install the census SIGSYS handler: {e}"));
    }

    // The alternate signal stack must be a registered `SecureBlock` so the signal frame is itself
    // scrubbed, and it must outlive the whole chain.
    let alt_block = SecureBlock::allocate(ALT_STACK_BYTES)
        .unwrap_or_else(|e| fail(format!("alt stack: {e:?}")));
    let alt = AltStack::install(alt_block.as_ptr() as usize, ALT_STACK_BYTES)
        .unwrap_or_else(|e| fail(format!("install the alt stack: {e:?}")));

    let mut sealed = Enter
        .seal_core_dumps()
        .raise_memlock()
        .install_tripwires(alt)
        .unwrap_or_else(|e| fail(format!("stage 3, tripwires: {e}")))
        .open_descriptors(SessionContext::open)
        .unwrap_or_else(|e| fail(format!("stage 4, descriptors: {e}")))
        .lock_all_pages()
        .isolate_network()
        .drop_privileges()
        .unwrap_or_else(|e| fail(format!("stage 7, drop privileges: {e}")))
        // `seal_with`, not `seal`, so the census installs a deliberately incomplete table *through
        // the real boot*. A hand-assembled filter standing next to the boot would not be testing
        // the boot.
        .seal_with(action, &entries)
        .unwrap_or_else(|e| fail(format!("stage 8, seal: {e}")));

    // From here the filter is in force.
    sealed.run_session(|sealed| {
        if let Err(e) = workload(sealed.context_mut()) {
            eprintln!("{FAIL_MARKER}{e}");
            std::process::exit(1);
        }
        let report = sealed.report();
        eprintln!(
            "{OK_MARKER}entries={} frames={} pixels={} commands={} edits={}",
            entries.len(),
            sealed.context().session.stats.frames,
            sealed.context().session.stats.pixels,
            sealed.context().session.stats.commands,
            sealed.context().session.stats.edits,
        );
        eprintln!(
            "CENSUS-REPORT mlockall={} mlock_errno={} no_new_privs={}",
            report.all_pages_locked, report.mlock_errno, report.no_new_privs
        );
    });

    sealed.teardown_and_exit(-1);
}

/// The real session: type, move, edit, undo, redo, paint, export, dump.
///
/// Written against the same public API the integration gate uses, so the census measures the loop
/// that ships rather than a second, simpler one.
fn workload(ctx: &mut SessionContext) -> Result<(), String> {
    let s = &mut ctx.session;

    // The scripted stream is built *before* sealing would be nicer, but building it here is
    // deliberate: `ScriptedInputSource::new` copies the bytes into the decoder's own tail buffer,
    // and doing that under the filter is exactly the allocation the census needs to see refused if
    // the discipline is broken.
    // Built here, deliberately: `ScriptedInputSource::from_events` encodes into the decoder's own
    // tail buffer, and doing that under the filter is exactly the allocation the census needs to see
    // refused if the discipline is broken.
    let mut events: Vec<InputEvent> = type_str("Census. A");
    // Two lefts, then a backspace, so the workload's damage path is a *delete* as well as an insert.
    events.extend(tap(holonomy_input::KEY_LEFT));
    events.extend(tap(holonomy_input::KEY_LEFT));
    events.extend(tap(holonomy_input::KEY_BACKSPACE));
    events.extend(ctrl(holonomy_input::KEY_Z));
    events.extend(ctrl(holonomy_input::KEY_Y));

    let mut src = ScriptedInputSource::from_events(&events);
    let exit = s.run(&mut src).map_err(|e| format!("the session: {e}"))?;
    if s.stats.commands == 0 {
        return Err("the stream produced no commands".into());
    }
    if s.stats.pixels == 0 {
        // A session that painted nothing has not exercised the blitter, and a census that did not
        // exercise the blitter has not measured it.
        return Err("the session painted no pixels".into());
    }
    let _ = exit;

    s.repaint_all()
        .map_err(|e| format!("the final paint: {e}"))?;

    for (file, format, name) in [
        (&mut ctx.html, Format::Html, "census.html"),
        (&mut ctx.pdf, Format::Pdf, "census.pdf"),
    ] {
        let mut sink = ExportSink {
            format,
            file: file.try_clone().map_err(|e| format!("clone {name}: {e}"))?,
            path: name.to_string(),
        };
        let report = s
            .export(&mut sink, "census")
            .map_err(|e| format!("export {name}: {e}"))?;
        if report.bytes == 0 {
            return Err(format!("{name} was empty"));
        }
    }

    let n = s
        .dump_ppm_to_file(&mut ctx.ppm)
        .map_err(|e| format!("dump the frame: {e}"))?;
    if n < 1000 {
        return Err(format!("the PPM is {n} bytes, which is only a header"));
    }
    ctx.ppm.flush().map_err(|e| format!("flush the PPM: {e}"))?;
    Ok(())
}

/// Type `s` as a keyboard would, with Shift where the layout needs it.
///
/// Searches the unshifted table first and the shifted one second, **from a fresh modifier state each
/// time**. The first version of this searched using the current shift flag, so after an uppercase
/// letter it searched the shifted table for the next lowercase one and found nothing.
fn type_str(s: &str) -> Vec<InputEvent> {
    let km = holonomy_input::Keymap::us();
    let mut events = Vec::new();
    let mut shift = false;
    let shifted = || {
        let mut m = holonomy_input::ModifierState::new();
        m.update(holonomy_input::KEY_LEFTSHIFT, 1);
        m
    };
    for c in s.chars() {
        let (code, needs) = match (0u16..128)
            .find(|&code| km.text_for(code, &holonomy_input::ModifierState::new()) == Some(c))
        {
            Some(code) => (code, false),
            None => (
                (0u16..128)
                    .find(|&code| km.text_for(code, &shifted()) == Some(c))
                    .unwrap_or_else(|| panic!("no key for {c:?}")),
                true,
            ),
        };
        if needs && !shift {
            events.push(InputEvent::press(holonomy_input::KEY_LEFTSHIFT));
            shift = true;
        } else if !needs && shift {
            events.push(InputEvent::release(holonomy_input::KEY_LEFTSHIFT));
            shift = false;
        }
        events.push(InputEvent::press(code));
        events.push(InputEvent::release(code));
    }
    if shift {
        events.push(InputEvent::release(holonomy_input::KEY_LEFTSHIFT));
    }
    events
}

/// One key: press, release.
fn tap(code: u16) -> Vec<InputEvent> {
    vec![InputEvent::press(code), InputEvent::release(code)]
}

/// Ctrl+`code`: press, press, release, release.
fn ctrl(code: u16) -> Vec<InputEvent> {
    vec![
        InputEvent::press(holonomy_input::KEY_LEFTCTRL),
        InputEvent::press(code),
        InputEvent::release(code),
        InputEvent::release(holonomy_input::KEY_LEFTCTRL),
    ]
}

/// Report and exit 1. Distinct from the census's exit 90 so a parent can tell "the workload failed"
/// from "the filter caught a syscall".
fn fail(e: String) -> ! {
    eprintln!("{FAIL_MARKER}{e}");
    std::process::exit(1)
}

/// Unused in this build, but kept so the `--` arguments are documented rather than ignored.
#[allow(dead_code)]
fn _paths(dir: &Path) -> (PathBuf, PathBuf) {
    (dir.join("census.html"), dir.join("census.ppm"))
}
