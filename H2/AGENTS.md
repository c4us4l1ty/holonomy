# Working in this repository

Read this before running a build or a test. This machine has 12 CPUs, ~15 GB RAM and a
slow disk, and the default "just run the tests" reflex will fill the disk and stall the
box for everyone using it.

## The one rule: never build the whole workspace

```
cargo test --workspace            # DO NOT RUN
cargo build                        # DO NOT RUN
```

`cargo test --workspace` has driven this machine to **load average 43.98 on 12 CPUs**
and pushed the disk to 100%. The cause is the dependency tree: `holonomy-shell` pulls in
the whole `typst` crate, which is thousands of translation units across three targets.
Building it is expensive in isolation and ruinous in parallel.

Always name the package:

```
cargo test -p holonomy-core        # fast: ~0.3s once built
cargo test -p holonomy-shell      # slow but bounded
cargo check  -p holonomy-core     # when you only need to know it compiles
```

There are only two crates. `-p holonomy-core -p holonomy-shell` together is acceptable and
is what CI-equivalent verification looks like: **326 tests, ~10s** once built.

## The resource ceiling — read this before running anything heavy

This is a small machine (12 CPUs, ~15 GB RAM, slow NVMe) that a person is trying to *use*:
Zed, Firefox and a terminal share it. A build that saturates the disk makes their editor
stop responding, and that is worse than a slow build. So the ceiling is not "don't waste
time", it is **"do not make the machine unusable"**.

Concretely:

* **One heavy tool at a time.** Never a `cargo` build alongside a Playwright suite. Never
  two Playwright suites. Never a build while a browser suite runs.
* **Batch your edits, then build once.** Every `cargo` invocation after a `src/*.rs` edit
  pays a fresh `holonomy-shell` compile + link. Three separate `cargo test` calls on three
  edits costs three times one build.
* `cargo clippy --all-targets` on `holonomy-shell` pulls the whole typst tree through the
  checker and is the single most expensive thing in this repo. Run it once, at the end, and
  only if you touched Rust. `cargo clippy -p holonomy-core` is cheap and catches most of it.
* **Do not run `--release` locally, ever.** It is a 30–60 minute build of the entire typst
  dependency tree and it is the fastest way to make this machine unusable. CI builds release
  on its own runners.
* Page cache fills to ~10 GB after a build and the machine *feels* full while `free` still
  reports several GB available. That is reclaimable cache, not a leak, and it clears itself.
  Do not "fix" it by deleting things.
* If the machine is already struggling, stop. Do not queue another build "while waiting".

## Rust: build less, test less often

* `cargo check -p <pkg>` while iterating. Only `cargo test -p <pkg>` when you actually
  need assertions.
* **Edit one crate at a time.** Every edit to a `src/*.rs` file invalidates that crate's
  test binary, and a fresh `holonomy-shell` link is the expensive step. Batching edits
  across both crates means paying for two full rebuilds instead of one.
* Never use `--release`. Never use `-j` above the default; the disk, not the CPU, is the
  bottleneck and more jobs just queue on it.
* `target/` is **80 GB** (was 92 GB). It is not a build artefact to be cleaned casually. If
  you need space, reclaim in this order, cheapest rebuild cost first:

  ```bash
  rm -rf target/debug/incremental   # 11 GB, pure cache, always regenerated
  rm -rf target/debug/examples target/release/examples   # 2.5 GB, only --all-targets needs these
  ```

  Do **not** run `cargo clean` — it costs hours of rebuild. `cargo clean -p <pkg>` is the
  surgical version if a single package's artefacts have gone bad.

  `target/release` (32 GB) is kept deliberately: it is only needed to run
  `scripts/smoke-production.sh` locally, and rebuilding it is a 30–60 minute release build of
  the whole typst tree. CI builds release on its own runners, so nothing else wants it.
  Delete it if you need the space and are not about to run a smoke build.

  The remaining bulk is `target/debug/deps` at 44 GB, which holds ~244 hashed copies of the
  test binaries — cargo keeps one per feature/metadata combination and never prunes them.
  Pruning those safely needs `cargo-sweep`; hand-deleting by hash risks removing the current
  artifact and forcing a relink of the 772 MB `holonomy_shell` binary.

## TypeScript: `--noEmit` only

```
npx tsc --noEmit -p tsconfig.json
```

Never emit to `dist/` for a check, and never run a project-wide build "to be sure". A
known **pre-existing and harmless** error will show up in any run:

```
/tmp/holonomy-blocks-*/probe.ts   TS2741
```

That file is a generated probe under `/tmp`. Do not try to fix it and do not let it stop
you.

## Tests: run the one suite you touched

The browser suites are Playwright and each one launches Chromium. They are not cheap and
they contend with everything else.

```
node --experimental-strip-types test/<name>.ts    # one suite
npm run test:node                                 # ~15 fast node suites, fine
npm run test:browser                              # 6 Chromium suites, slow
```

Rules that keep this affordable:

* **One suite per command.** Do not loop over every `test/*.ts` "for completeness".
* Before running a browser suite, confirm the Vite dev server is already up and has
  **finished** its hot reload. A reload that lands mid-run destroys the execution context
  and the suite fails with `Execution context was destroyed, most likely because of a
  navigation` — a false failure that costs a whole re-run. If you have just edited
  `app/src/**`, `sleep 20` before running a browser suite.
* Never run two browser suites concurrently.
* `npm run test:all` is for a deliberate final verification, not for iteration.

## Debugging a failing test: instrument narrowly, then remove it

The temptation when a suite fails is to re-run it with more instrumentation. Each re-run
of a browser suite costs a Chromium launch. So:

* Add the probe, run **once**, read the value, remove the probe.
* Do not add three probes to guess between three hypotheses. Read the code path and pick
  the one guard that can return the observed value.
* `src/core/registry.ts` and `src/main.ts` are the hot files for browser behaviour; a
  temporary `console.warn` or a `(globalThis as any).__probe` push is usually enough.

## Before you push

Verify with the narrow set, in this order:

```
npx tsc --noEmit -p tsconfig.json
cargo test -p holonomy-core -p holonomy-shell
node --experimental-strip-types test/<the suites you touched>
```

That is sufficient. Escalate to `npm run test:all` only when you have changed something
whose blast radius you cannot name.
