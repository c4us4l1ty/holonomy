# Shell icons are placeholders.

Every icon here is a flat generated block, not Holonomy branding. They exist because a
packaged app requires an icon set, and an absent one is a build failure rather than a
warning — on some platforms only.

## Why `icon.ico` and `icon.icns` are in the repository

`tauri-build` resolves the Windows resource icon by taking the first `bundle.icon` entry
ending in `.ico`, falling back to a literal `icons/icon.ico`, and **returning an error if
that path does not exist**. So a list of PNGs alone builds on Linux and macOS and fails on
Windows, naming a file that nobody remembered to add. `icon.icns` is the same idea for the
macOS bundle. Both are checked by `tests/tauri-config.rs`, which is a repository test and
therefore runs on the platform where the failure cannot be reproduced.

Replace them before any release build: `cargo tauri icon <source.png>` regenerates the
whole set from one image, and then `git add` the result — the point of committing them is
that the next `cargo check` on Windows finds them.