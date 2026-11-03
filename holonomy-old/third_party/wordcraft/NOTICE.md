# WordCraft attribution

Holonomy incorporates selected UI source from
[`storytold/wordcraft`](https://github.com/storytold/wordcraft), revision
`ae5c65b9dd11faf359ce039a2a487e57e3d824a2`.

Upstream is offered under `MIT OR Apache-2.0`. Holonomy uses these portions under
the **MIT License**; the full text is preserved in [`LICENSE-MIT`](LICENSE-MIT):

> Copyright (c) 2026 ArtCraft Team and the WordCraft contributors

## Adapted material

- `crates/holonomy-render/src/wordcraft/tokens.rs` adapts
  `crates/ui-egui/src/theme.rs` (light/dark colour tokens).
- `crates/holonomy-render/src/wordcraft/ribbon_fit.rs` adapts
  `crates/ui-egui/src/ribbon_fit.rs` (responsive group planner and tests).
- `crates/holonomy-render/src/wordcraft/gallery.rs` adapts
  `crates/ui-egui/src/previews.rs` (style-gallery model and tile counts).
- `crates/holonomy-render/src/ribbon.rs`, `chrome.rs`, `widgets.rs`, and
  `crates/holonomy/src/session.rs` adapt the ribbon/navigation behaviour from
  `crates/ui-egui/src/ribbon.rs`, `crates/ui-egui/src/panes/nav.rs`, and related
  frontend code into Holonomy's native framebuffer renderer.
- `tools/port_wordcraft_icons.py` transcribes procedural icon path coordinates
  from `crates/ui-egui/src/icons.rs` into
  `crates/holonomy-render/assets/icons/wordcraft-*.svg`. The SVGs are reviewed,
  trusted build inputs; `build.rs` rasterises them to 16/20 px masks. They are
  not parsed or evaluated in the sealed editor.
- The earlier document/folder/pencil/left-arrow SVGs in
  `crates/holonomy-render/assets/icons/*.svg` were adapted from
  `crates/render/src/icon_lib.rs` at
  `ffb155a0321bba08da495efda66495104c1c8b9a` and remain under MIT.

Verbatim upstream snapshots are kept in [`upstream/`](upstream/) for review and
are not compiled. See [`README.md`](README.md) for the path map.

Holonomy does not link WordCraft's egui/GPU frontend. No ArtCraft/WordCraft brand
logos, Microsoft/Google icon assets, screenshots, or sample documents were copied.
