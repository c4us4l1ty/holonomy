# Wordcraft attribution

Holonomy's native ribbon layout, colour tokens, and procedural icon masks in
`crates/holonomy-render/src/ribbon.rs` and `crates/holonomy-render/src/icons.rs`
are adapted from storytold/wordcraft (MIT OR Apache-2.0), used under the MIT
license. Upstream reference commit:
`242e850aaad6bf38e1cd2be4122cc7cb9e5a15a8`.

The MIT license text is preserved in `LICENSE-MIT`. Holonomy does not link
Wordcraft's egui/GPU frontend; the adaptation targets Holonomy's integer
framebuffer renderer and preserves Holonomy's sealed execution model.

The document, folder, pencil and left-arrow path geometry in
`crates/holonomy-render/assets/icons/*.svg` is adapted from WordCraft's
`crates/render/src/icon_lib.rs` at `ffb155a0321bba08da495efda66495104c1c8b9a`.
These **trusted, reviewed** SVGs are converted to 16/20 px bit masks at build time, not
parsed or evaluated in the sealed editor. WordCraft's license is MIT OR Apache-2.0;
these four pieces are used under MIT. No Microsoft/Google icon assets were copied.
