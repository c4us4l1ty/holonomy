# Shell configuration notes

## `withGlobalTauri: true`

Required for `window.__TAURI__` to exist. Tauri 2 does **not** expose the global
API by default, so without this the frontend has no way to invoke any command and
the bridge is unreachable.

The symptom is unusually quiet: the window launches, webkit spawns its processes,
the Rust side logs nothing wrong, and the frontend simply never talks to it. That
cost one full debugging cycle to find, and it is recorded here because the config
key is not something you would look for when nothing is erroring.

The alternative is bundling `@tauri-apps/api` and importing `invoke` explicitly,
which is the recommended approach and does not need this flag. It is not used yet
because the harness has no bundler-visible Tauri dependency, and adding one to
support a single call would be more machinery than the flag. Revisit when the
bridge has more than a handful of commands.

## `capabilities/default.json` is the real permission surface

Tauri 2 grants nothing implicitly. An ungranted command fails **at runtime**, not
at build time, so a missing entry presents as a frontend that silently does
nothing — the same failure mode as the flag above.

**But not for the bridge commands, and the distinction is worth being precise
about.** The ACL governs *plugin* commands: the `core:` and `opener:` entries here.
The four bridge commands are registered on the application itself through
`tauri::generate_handler!`, which is a different path and is not gated by a
capability. This file used to say "when the bridge commands land, each one needs an
entry here" — that was wrong. Adding entries for them would have been cargo cult,
and omitting them was correct.

It matters because the review question is "can the frontend reach this?", and it has
two different answers:

- an app command registered in `generate_handler!` — always reachable;
- a plugin command — reachable only if its `plugin:name|permission` is listed here.

Moving a command behind a plugin, or adding a plugin command, changes which question
applies.

## Running the CLI: from the repository root, not from `app/`

`cargo tauri` requires a `tauri.conf.json` in *some subfolder of the current
directory*, and every path inside that config resolves relative to **the config
file's own directory**, not the working directory.

Both facts point the same way, so the npm scripts `cd ..` first and pass a
root-relative config path:

```json
"tauri:build": "cd .. && cargo tauri build --config crates/holonomy-shell/tauri.conf.json"
```

Two ways this was got wrong first, and both failures point at the wrong thing:

- `--config` before the subcommand (`cargo tauri --config X build`) is rejected as
  an unknown argument. It is a subcommand flag.
- Running from `app/` panics with "couldn't recognize the current folder as a Tauri
  project", which reads like a missing config file rather than a wrong cwd.

## `devUrl` and `frontendDist`

`devUrl` points at the same Vite server the browser harness uses, on the same
port. That is deliberate: one frontend, two hosts, so a behaviour verified in
Chromium is verified against the same code the shell runs. Two Vite instances on
different ports would be one more thing to drift.

`frontendDist` is `../../app/dist`, which `beforeBuildCommand` produces. Note
that `app/index.html` is the product surface and `app/m3.html` is the M3 harness;
a Vite build only emits `index.html` unless other entries are added to
`build.rollupOptions.input`, so a packaged build will not include the harness
pages. That is correct and intended.