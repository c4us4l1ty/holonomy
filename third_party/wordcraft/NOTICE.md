# Wordcraft attribution

Holonomy's native ribbon layout, colour tokens, and procedural icon masks in
`crates/holonomy-render/src/ribbon.rs` and `crates/holonomy-render/src/icons.rs`
are adapted from storytold/wordcraft (MIT OR Apache-2.0), used under the MIT
license. Upstream reference commit:
`242e850aaad6bf38e1cd2be4122cc7cb9e5a15a8`.

The MIT license text is preserved in `LICENSE-MIT`. Holonomy does not link
Wordcraft's egui/GPU frontend; the adaptation targets Holonomy's integer
framebuffer renderer and preserves Holonomy's sealed execution model.
