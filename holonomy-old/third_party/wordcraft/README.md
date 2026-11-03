# WordCraft UI port

Holonomy copies and adapts selected UI source from
[`storytold/wordcraft`](https://github.com/storytold/wordcraft) under the **MIT**
license. The upstream workspace offers `MIT OR Apache-2.0`; Holonomy elects MIT.

- Upstream revision: `ae5c65b9dd11faf359ce039a2a487e57e3d824a2`
- License text: [`LICENSE-MIT`](LICENSE-MIT)
- Attribution and adaptation notes: [`NOTICE.md`](NOTICE.md)
- Verbatim upstream reference files: [`upstream/`](upstream/)

This is a source port into Holonomy's native integer framebuffer renderer, not a
runtime dependency on WordCraft's egui/GPU frontend. Holonomy's sealed execution
model, dependency boundary, and encrypted-container design are unchanged.

## What was copied/adapted

| Upstream file at `ae5c65b…` | Holonomy destination | Adaptation |
| --- | --- | --- |
| `crates/ui-egui/src/theme.rs` | `crates/holonomy-render/src/wordcraft/tokens.rs` | Exact light/dark colour tokens, dependency-free `Tokens`/`rgb` API. `ribbon::Palette` maps the tokens onto native bands. |
| `crates/ui-egui/src/ribbon_fit.rs` | `crates/holonomy-render/src/wordcraft/ribbon_fit.rs` | Responsive FULL/COMPACT/COLLAPSED planner and regression tests, ported to integer arithmetic. Drives `ribbon::layout`. |
| `crates/ui-egui/src/previews.rs` | `crates/holonomy-render/src/wordcraft/gallery.rs` | Style-gallery tile model, sample string, full/compact tile counts, and action mapping. Painted as native style-preview buttons. |
| `crates/ui-egui/src/icons.rs` | `tools/port_wordcraft_icons.py` → `crates/holonomy-render/assets/icons/wordcraft-*.svg` | Procedural Pen path coordinates transcribed into trusted 20×20 monochrome SVGs; build-time rasterisation to 16/20 px masks. |
| `crates/ui-egui/src/panes/nav.rs` | `crates/holonomy-render/src/ribbon.rs` navigation geometry/paint + `session.rs` routing | Headings/Pages tabs, search, close, heading/page rows, and wide-default Navigation pane. |

`upstream/` contains byte-for-byte snapshots of the five source files above for
review. They are reference material and are **not** compiled into Holonomy.

## License boundaries

Only the UI code and procedural icon geometry listed above are treated as MIT
WordCraft material. Upstream's `ATTRIBUTION.md` and `NOTICE` state that ArtCraft
brand names/logos are separately protected; no ArtCraft/WordCraft brand assets
are copied here. Sample documents, screenshots, Microsoft/Google icon sets, and
other third-party assets from upstream were not copied.
