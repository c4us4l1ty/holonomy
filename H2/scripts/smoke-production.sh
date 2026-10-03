#!/usr/bin/env bash
# Run the *built* binary, in its own engine, under the production CSP.
#
# # What this is for
#
# Everything else in this repository runs in one of two places, and neither is the product:
#
# - the Node and Playwright suites run against a Vite dev server, which sends no
#   Content-Security-Policy at all;
# - `verify-engine.sh` runs in a real webview, but through `cargo tauri dev`, which loads
#   the *unbundled* frontend over `http://localhost:5184` — again with no CSP.
#
# So `tauri.conf.json`'s policy
#
#     default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:
#     holo-asset:; script-src 'self'
#
# has never been in force for a single assertion in this project. `font-src` is absent from
# it, so it inherits `default-src 'self'`; KaTeX's stylesheet references its webfonts by
# relative URL, which resolves same-origin and is therefore allowed. That reasoning is
# correct, entirely untested, and exactly the kind of thing that stops being correct when
# someone adds a CDN.
#
# This script is the only thing in the repository that can answer the question, because it is
# the only thing that runs a *packaged* frontend over `tauri://localhost` with the policy
# applied.
#
# # Why it asserts three things and not "it launched"
#
# #   1. Asset bundles compile cleanly. `cargo tauri build` has already run; this checks the
# #      fonts are *in the binary*, which a successful build does not prove.
# #   2. The CSP permits `holo-asset://` images and the KaTeX styles and webfonts. Checked
# #      by effect, in the verification report.
# #   3. The binary launched and mounted the virtualised scroller. The existing 45 checks.
#
# # Usage
#
#   scripts/smoke-production.sh                # build is expected to have happened
#   scripts/smoke-production.sh --build        # run `cargo tauri build --no-bundle` first
#
# Env:
#   HOLO_SMOKE_BIN   path to the binary (default: target/release/holonomy, or `.exe` on Windows)
#   HOLO_VERIFY_OUT  where the report goes (default: target/smoke-report.json)

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# # Why this file is written the way it is
#
# The `smoke` matrix runs on Linux, macOS and Windows, and every one of those runners uses a
# different default shell for `run:` — `sh` on Linux, `bash` on macOS, **PowerShell** on
# Windows. A `./script.sh` line therefore does nothing at all on the Windows leg: PowerShell
# does not execute shell scripts. The CI step now pins `shell: bash`, and what follows is the
# part that had to change for that to be enough:
#
#   - `python3` is on the Linux and macOS images and is not on the Windows one. `node` is
#     installed by the same `actions/setup-node` step the job already uses, on all three.
#   - `/tmp` does not exist on Windows. `mktemp -d` does.
#   - the binary carries a `.exe` on Windows, so a plain `-x` on a fixed name reports a binary
#     that is sitting right there as absent.
#   - `GDK_BACKEND` is a GTK variable. macOS is WKWebView and Windows is WebView2, so setting
#     it there asserts nothing and is noise; it is now Linux-only, which is the only platform
#     it ever meant.

OUT="${HOLO_VERIFY_OUT:-target/smoke-report.json}"
LOG_DIR="$(mktemp -d)"
LOG="$LOG_DIR/holonomy-smoke.log"
BUILD=0
[ "${1:-}" = "--build" ] && BUILD=1

cleanup() { rm -f "$LOG" "$LOG.app"; return 0; }
trap cleanup EXIT

if [ "$BUILD" = "1" ]; then
  echo "building the release binary (no installer: the bundling config is checked by"
  echo "tests/tauri-config.rs, and AppImage tooling fails for reasons unrelated to the app)"
  if ! cargo tauri build --no-bundle > "$LOG.build" 2>&1; then
    echo "FAIL: cargo tauri build failed"
    tail -40 "$LOG.build"
    exit 2
  fi
  echo "  built"
fi

# Resolved *after* the build, because resolving before it is what made `--build` useless from
# a clean tree: the lookup ran, found nothing, and exited before the build that would have
# produced the answer.
BIN="${HOLO_SMOKE_BIN:-}"
if [ -z "$BIN" ]; then
  for candidate in target/release/holonomy-shell target/release/holonomy-shell.exe \
                   target/release/holonomy target/release/holonomy.exe; do
    if [ -x "$candidate" ]; then BIN="$candidate"; break; fi
  done
  if [ -z "$BIN" ]; then
    echo "FAIL: no built binary under target/release. Looked for holonomy-shell{,.exe} and"
    echo "      holonomy{,.exe}. Run with --build, or set HOLO_SMOKE_BIN."
    exit 2
  fi
fi

# A 1x1 red PNG and its SHA-256, written before the app starts, so the run has something
# real to serve. Not used by this script directly -- the verification builds its own asset in
# SQLite -- but it proves the *home directory* is writable, which on a read-only CI runner is
# a failure that otherwise surfaces as an unexplained panic in a subprocess.
#
# # Bounded, and the bound is the point
#
# The verification is supposed to end by calling `app.exit()`. When it does not — a panic on a
# tokio worker, a webview that never fires its last callback, a window that will not open on a
# headless runner — this waited forever. Twice on this machine, before the bound existed: two
# runs of eight minutes each, killed by hand, with the failure visible only in a temp log the
# script had already promised to delete.
#
# In CI that is not eight minutes, it is the job. A smoke test that cannot fail cannot report.
# `timeout` is coreutils, present on all three runners. `HOLO_SMOKE_TIMEOUT` overrides it for a
# slower machine, and the app log is dumped on timeout because that log is the only evidence.
DEADLINE="${HOLO_SMOKE_TIMEOUT:-180}"
# Two variables, because "passed" and "never happened" are both an empty string and only one of
# them is a pass.
#
# `PROBE_VERDICT` is 0 only when the probe ran to completion and everything it looked for held.
# Any other value -- an early return, a timeout, a non-zero exit -- leaves it non-zero.
# `SECOND_RESULT` is the *reason* it did not, and is empty on success.
#
# An earlier version tried to carry both in one variable, twice: first as a sentinel string that
# a successful probe overwrote with the empty string, and then as an empty string that the report
# could not tell apart from a probe which never ran. Both reported "did not run" against a
# second launch that had worked perfectly, which is worse than no probe at all because it looks
# like a gate.
PROBE_VERDICT=1
SECOND_RESULT=""
echo "==> launching $BIN with the production CSP (deadline ${DEADLINE}s)"
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"

# The same two environment settings `verify-engine.sh` needs, and for the same reasons: the
# webview sandbox and a seat-less Wayland session are properties of this machine, not of
# Holonomy. Set rather than assumed, so a future machine without them fails visibly instead
# of mysteriously.
#
# `GDK_BACKEND` is Linux-only. It is a GTK variable; macOS is WKWebView and Windows is WebView2,
# so on those runners it names nothing that anything reads, and listing it as part of "what this
# script needs" describes one platform's setup as if it were the contract.
if [ "$(uname -s)" = "Linux" ]; then
  export GDK_BACKEND="${GDK_BACKEND:-wayland}"
fi
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1

# The primary is backgrounded rather than run in the foreground, because the second-launch probe
# below has to fire *while it is running* -- and the whole point of that probe is that the two
# overlap. See `probe_the_second_launch` for why the overlap is the thing being tested.
HOLO_VERIFY_OUT="$ROOT/$OUT" timeout "$DEADLINE" "$BIN" > "$LOG.app" 2>&1 &
APP_PID=$!

probe_the_second_launch() {
  # Launch the app a second time, while the first is still up, and prove it did not become a
  # second process with a second handle on the database.
  #
  # # Why this needs two processes and cannot be a unit test
  #
  # The claim is about what does *not* happen: a second writer. Every version of it that could
  # run in one process would be a test of a mock, and the failure it guards against -- two
  # `Store`s open on one `.holo` file, fighting over the WAL lock -- is precisely the thing that
  # cannot be arranged inside a single process.
  #
  # # Why the app log is the readiness signal
  #
  # Because it is the only one that is already there. The plugin claims its D-Bus name during
  # `Builder::build()`, long before anything writes a file, and "the process has started" is
  # not enough: fire the probe before the name is claimed and the *probe* becomes the primary,
  # runs to the deadline, and fails a run that is actually correct. `frontend mounted` is
  # printed by `report_mounted`, which is after the name is claimed, so it cannot fire early.
  #
  # # What "correct" looks like
  #
  # The second process exits 0 in milliseconds, having forwarded its argv and never having
  # opened anything. Without the plugin it would run to `SECOND_DEADLINE` and be killed --
  # a second window, a second `Store`, a lock fight, and a document that is blocked or corrupt
  # depending on which instance the user quits first.
  SECOND_DEADLINE=20
  WAITED=0
  while [ "$WAITED" -lt 30 ]; do
    grep -q "frontend mounted" "$LOG.app" 2>/dev/null && break
    kill -0 "$APP_PID" 2>/dev/null || break
    sleep 1
    WAITED=$((WAITED + 1))
  done

  if ! grep -q "frontend mounted" "$LOG.app" 2>/dev/null; then
    SECOND_RESULT="the primary never reported a mount, so the second-launch probe was not run"
    return
  fi

  # A document in a directory of its own, so a process that opened it would leave evidence
  # that is unmistakably not the primary's: a `-wal` and a `-shm` beside it.
  DECOY_DIR="${TMPDIR:-/tmp}/holonomy-second-launch-$$"
  mkdir -p "$DECOY_DIR"
  DECOY="$DECOY_DIR/Second Launch.holo"

  timeout "$SECOND_DEADLINE" "$BIN" "$DECOY" > "$LOG.second-launch" 2>&1
  SECOND_STATUS=$?

  if [ "$SECOND_STATUS" -eq 124 ] || [ "$SECOND_STATUS" -eq 143 ]; then
    SECOND_RESULT="the second instance ran for ${SECOND_DEADLINE}s instead of handing its \
argument to the running one, so it opened its own store: two processes, one file"
  elif [ "$SECOND_STATUS" -ne 0 ]; then
    SECOND_RESULT="the second instance exited $SECOND_STATUS, not 0"
  else
    # The primary should have adopted the document the second one named. That is the other
    # half of the claim, and without it the check would pass for an app that correctly refuses
    # to start twice and also silently ignores every double-click.
    sleep 2
    if grep -q "$DECOY" "$LOG.app" 2>/dev/null; then
      # Everything held. The reason stays empty and the verdict is what says so.
      PROBE_VERDICT=0
    else
      # The evidence, in the failure. `LOG_DIR` is a `mktemp -d` the script deletes on the way
      # out, so a probe that fails without printing what it saw has thrown away the only copy —
      # which is what happened the first time this ran, and it cost a seven-minute rebuild to
      # find out that the primary had simply never been asked to say anything.
      SECOND_RESULT="the second launch exited 0 but the running instance never mentioned \
$DECOY, so the argument was dropped somewhere between the bus and the editor. \
second-process output: $(head -c 400 "$LOG.second-launch" 2>/dev/null | tr '\n' ' ')"
      echo "--- what the second process printed ---"
      head -20 "$LOG.second-launch" 2>/dev/null || echo "(nothing)"
      echo "--- what the running instance printed about it ---"
      grep -n "second launch\|launched with" "$LOG.app" 2>/dev/null | tail -10 || echo "(nothing)"
    fi
  fi
  rm -rf "$DECOY_DIR"
}

# In the *foreground*, deliberately. The application is the thing that has to run in the
# background -- it is what the probe races against -- and backgrounding the probe as well put it
# in a subshell, where `SECOND_RESULT` was assigned and died. The parent then read its own empty
# initialiser and reported "the probe did not run" against a probe that had run perfectly. A
# shell variable set in a background function is the same class of mistake as mutating a
# registry and then asserting the value moved.
probe_the_second_launch

wait "$APP_PID"
STATUS=$?

if [ "$STATUS" -eq 124 ] || [ "$STATUS" -eq 143 ]; then
  echo "FAIL: the binary did not exit within ${DEADLINE}s."
  echo "The verification ends by calling app.exit(); a run that reaches the deadline did not."
  echo "--- app output ---"
  tail -60 "$LOG.app"
  exit 4
fi

# The exit code is the verdict. `app.exit()` in `submit_verification` is the only way a Tauri
# process ends with a chosen status, and a build that launched but failed its checks must not
# read as a pass.
if [ "$STATUS" -ne 0 ]; then
  echo "FAIL: the smoke run exited $STATUS"
  echo "--- app output ---"
  tail -40 "$LOG.app"
  [ -f "$ROOT/$OUT" ] && { echo "--- report: $OUT ---"; cat "$ROOT/$OUT"; }
  exit "$STATUS"
fi

[ -f "$ROOT/$OUT" ] || { echo "FAIL: no report at $OUT"; tail -40 "$LOG.app"; exit 3; }

echo "==> $(grep -o '"passed": [0-9]*' "$ROOT/$OUT" | head -1) of $(grep -o '"ran": [0-9]*' "$ROOT/$OUT" | head -1) checks passed"

# The three assertions, as shell over the report. The *checks* themselves are TypeScript in
# `core/verify.ts`; what is asserted here is the part Rust knows and the frontend cannot --
# what the binary was built with, and what policy it ran under.
# `node`, not `python3`. This script runs on all three smoke runners and the Windows one has
# no `python3` on it; `node` is installed by the `actions/setup-node` step this job already
# has, on every platform. The assertions are unchanged -- only the reader changed.
node -e '
const fs = require("fs");
const report = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
const env = report.environment || {};
const problems = [];

if (!env.verification) {
  problems.push("the run was not started as a verification (HOLO_VERIFY_OUT was not seen)");
}
if (env.profile !== "release") {
  problems.push(`the binary is a ${env.profile} build; this checks the shipping profile`);
}

const csp = env.csp || "";
if (!csp.includes("default-src")) {
  problems.push("no CSP was recorded, so nothing below was tested under a policy");
} else {
  // `img-src` is where `holo-asset:` has to be. Checked here as well as by effect, because
  // an effect check alone cannot tell "the policy allows it" from "the policy is absent".
  const img = csp.split(";").find(d => d.trim().startsWith("img-src")) || "";
  if (!img.includes("holo-asset:")) {
    problems.push(`img-src does not include holo-asset: -- ${JSON.stringify(img.trim())}`);
  }
}

const fonts = env.bundled_fonts || [];
if (fonts.length < 2) {
  problems.push(`only ${fonts.length} bundled font family/families: ${fonts}`);
}

const failed = (report.results || []).filter(r => !r.pass).map(r => r.name);
if (failed.length) {
  problems.push(`${failed.length} check(s) failed: ${failed.slice(0, 5).join("; ")}`);
}

// The second-launch probe, asserted here rather than as an in-engine check because it is the
// only claim in this file that needs two live processes.
//
// Empty means pass. Every other value is the reason it failed, including the sentinel the
// variable starts at: a smoke run whose probe never reached a verdict has not proved the
// single-instance path, and reporting that as a pass is how a gate stops being one.
const second = process.argv[2];
if (process.argv[3] !== "0") {
  problems.push(second || "the second-launch probe did not run, so two-instance behaviour is unverified");
}

if (problems.length) {
  console.error("FAIL: " + problems.join("\n      "));
  process.exit(1);
}

console.log(`  profile:      ${env.profile}`);
console.log(`  csp:          ${csp.slice(0, 96)}${csp.length > 96 ? "..." : ""}`);
console.log(`  fonts:        ${fonts.join(", ")}`);
console.log(`  association:  declared=${env.declares_file_association}`);
console.log(`  second launch: ${process.argv[3] === "0" ? "exited 0 and the running instance adopted it" : second}`);
console.log("  (the association only reaches a file manager once an installer has run;");
console.log("   `cargo tauri build --no-bundle` does not produce one, so that claim stays open)");
' "$ROOT/$OUT" "$SECOND_RESULT" "$PROBE_VERDICT"

STATUS=$?
[ "$STATUS" -ne 0 ] && exit "$STATUS"

echo
echo "PASS: the built binary launched, mounted, and ran under the production CSP"
echo "report: $OUT"