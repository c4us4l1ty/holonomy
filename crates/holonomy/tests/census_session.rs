//! Step 4: the Phase 8 syscall census, and the two properties it has to have.
//!
//! # What this closes
//!
//! Phase 7 left an explicit debt, recorded in `holonomy_jail::seccomp::table`'s own docs: *"Phase 8
//! re-runs this census against the real session loop and asserts the table still suffices; that is
//! the check that closes this, and it is not optional."*
//!
//! Phase 7's `census_session.rs` example touches every *subsystem* the session is built from. This
//! drives the actual [`holonomy::session::Session`] — the real `Editor`, `Chrome`, atlas, painter
//! and input decoder, and a real export to a descriptor opened at boot stage 4.
//!
//! # Two assertions, and why both
//!
//! 1. **The real table suffices.** The loop runs to completion under all 50 entries. If a Phase 8
//!    addition needed a syscall nobody measured, this fails with the number.
//! 2. **The harness can still detect a gap.** H2's DOCTRINE §4: a harness that cannot demonstrate
//!    the failure it detects proves nothing about the passes. So one entry is dropped and the census
//!    is required to notice.
//!
//! (2) is the one that goes stale silently. A census that always passes because it never runs is
//! indistinguishable from a census that passes because the table is right.
//!
//! # The census runs in a subprocess because the jail cannot be entered from `#[test]`
//!
//! `unshare(CLONE_NEWUSER)` returns `EINVAL` in a multi-threaded process and libtest always spawns
//! one. The example's `main` is single-threaded, so it can.
//!
//! # What it does *not* cover
//!
//! DRM, a real keyboard, the KDF, and the ring's staged commit. Those are Phase 9. This closes the
//! *loop's* syscall surface, not the whole program's, and saying otherwise would be the kind of
//! claim this project has already had to correct twice.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `deps/census_session-<hash>` -> `<profile>/examples/census_session`. Two `parent()`s, then
/// `examples`. Nothing has to be built or looked up on disk.
fn program() -> PathBuf {
    let mut p = std::env::current_exe().expect("the test binary's own path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    // Named `phase8_census_session`, not `census_session`: `holonomy-jail` already owns that name and
    // cargo writes both to the same directory. See `WHOAMI` in the example.
    p.join("examples").join("phase8_census_session")
}

/// A writable directory for the child. Per-process, because libtest shares a pid across threads.
fn scratch() -> PathBuf {
    let dir = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("holonomy-census-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// Run the census once, with `action` and an optional comma-separated `drop` list.
fn census(action: &str, drop: &str) -> Output {
    let dir = scratch();
    Command::new(program())
        .env("HOLONOMY_CENSUS_ACTION", action)
        .env("HOLONOMY_CENSUS_DROP", drop)
        .env("HOLONOMY_CENSUS_DIR", &dir)
        .output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", program().display()))
}

fn stderr_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The child's identity, from its first line of stdout.
///
/// Checked on every run. `holonomy-jail` publishes an example called `census_session` too, both land
/// in `target/<profile>/examples/`, and whichever cargo compiled last wins. Without this check a
/// full-workspace run quietly executes the wrong program and reports its failure as this phase's --
/// which is exactly what happened: `session.fail unknown action ""`, from a binary that reads
/// `HOLONOMY_CENSUS_ACTION` and this one reads `HOLONOMY_CENSUS_ACTION` and got an empty string.
fn whoami(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Did the child reach its own success marker?
fn ok(o: &Output) -> bool {
    stderr_of(o).contains("CENSUS-OK")
}

#[test]
fn the_real_session_loop_completes_under_the_shipped_allowlist() {
    let out = census("trap", "");
    let err = stderr_of(&out);
    assert_eq!(
        whoami(&out),
        "phase8-session-loop",
        "the wrong census binary was executed -- two examples share a name in one directory"
    );
    assert!(
        ok(&out),
        "the Phase 8 session did not complete under the {} shipped entries.\n\
         exit={:?}\n{err}",
        holonomy_jail::seccomp::table::ALLOWLIST.len(),
        out.status.code()
    );
    // And it actually did the work, rather than completing trivially. A census of a loop that types
    // nothing and paints nothing would pass this and measure nothing.
    assert!(
        err.contains("pixels=") && !err.contains("pixels=0 "),
        "the session painted no pixels, so the blitter was never measured:\n{err}"
    );
    let frames: u32 = err
        .split("frames=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    assert!(frames > 0, "no frames were painted:\n{err}");
    let commands: u32 = err
        .split("commands=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    assert!(
        commands > 10,
        "the loop consumed only {commands} events:\n{err}"
    );
}

#[test]
fn the_real_session_loop_also_completes_under_kill_process() {
    // `Action::Trap` and `Action::KillProcess` differ in what happens to an *unlisted* syscall, not
    // in what happens to a listed one -- so a loop that completes under Trap must complete under
    // KillProcess too. Running both is what stops the census from passing because the handler is
    // lenient rather than because the table is right.
    let out = census("kill-process", "");
    assert!(
        ok(&out),
        "the loop did not complete under KILL_PROCESS.\nexit={:?}\n{}",
        out.status.code(),
        stderr_of(&out)
    );
}

#[test]
fn the_census_detects_an_entry_the_session_needs() {
    // `write` is issued many times over -- `eprintln!` in the boot report, the PPM, both exports --
    // so removing it must stop the run. If this ever passes, the census is not looking.
    for entry in ["write", "close"] {
        let out = census("trap", entry);
        assert!(
            !ok(&out),
            "dropping `{entry}` from the allowlist did not stop the session, so the census cannot \
             detect a missing entry.\n{}",
            stderr_of(&out)
        );
        assert!(
            !out.status.success(),
            "dropping `{entry}` produced a successful exit code, which means nothing caught it"
        );
    }
}

#[test]
fn the_real_loop_needs_fewer_syscalls_than_the_table_grants() {
    // Measured, and recorded rather than left implicit: **the Phase 8 loop does not issue `lseek` or
    // `epoll_wait`**, so both entries survive being dropped and the session still completes.
    //
    // That is slack in the *permissive* direction, and it is the right kind of slack. Phase 7's
    // workload used an `epoll` pipe to stand in for a blocking read, and the real loop's scripted
    // source and buffered exporter never reach for one. Keeping the entries costs two filter
    // instructions and means a future source that does `epoll_wait` -- which is where an evdev
    // session should go -- is not killed on its first keypress.
    //
    // Asserted so that the next person to trim the table knows the trim is safe *today* and has to
    // re-measure rather than assume.
    for entry in ["lseek", "epoll_wait"] {
        let out = census("trap", entry);
        assert!(
            ok(&out),
            "the loop appears to need `{entry}` now, which is a change from the Phase 8 \
             measurement.\nexit={:?}\n{}",
            out.status.code(),
            stderr_of(&out)
        );
    }
}

#[test]
fn a_drop_that_matches_nothing_is_refused_rather_than_measured() {
    // A typo in a name would otherwise produce a *complete* table and a green result from a run
    // that removed nothing -- the harness would report success for a measurement it never performed.
    let out = census("trap", "not_a_syscall_name");
    assert!(
        !ok(&out),
        "an unmatched drop was accepted:\n{}",
        stderr_of(&out)
    );
    assert!(
        stderr_of(&out).contains("exactly one allowlist entry"),
        "the refusal should say what was wrong:\n{}",
        stderr_of(&out)
    );
}

#[test]
fn the_census_leaves_real_artifacts_behind() {
    // A census that completed without writing an HTML file, a PDF and a PPM has proven the loop does
    // not fault, and nothing about whether the sinks work. The exports are the sinks.
    //
    // This runs its **own** census rather than reading whatever the other tests left behind. They run
    // in parallel and share a pid, so the directory is shared too -- and a first version of this
    // read files that a sibling test was in the middle of truncating, which failed intermittently
    // for a reason that had nothing to do with the code under test.
    let out = census("kill-process", "");
    assert!(
        ok(&out),
        "the census did not complete:\n{}",
        stderr_of(&out)
    );
    let dir = scratch();
    for name in ["census.html", "census.pdf", "census.ppm"] {
        let path = dir.join(name);
        let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(meta.len() > 100, "{name} is only {} bytes", meta.len());
    }
    let ppm = std::fs::read(dir.join("census.ppm")).expect("read the PPM");
    assert!(
        ppm.starts_with(b"P6\n1280 800\n255\n"),
        "the PPM header is wrong"
    );
}

#[test]
fn the_census_binary_exists_where_the_test_expects_it() {
    // A missing binary would make every other test in this file fail with `spawn ...: No such file`,
    // which reads like a census problem rather than a build-order one. Worth its own assertion.
    let p: &Path = &program();
    assert!(
        p.is_file(),
        "{} does not exist; run `cargo test -p holonomy` from the workspace root",
        p.display()
    );
}
