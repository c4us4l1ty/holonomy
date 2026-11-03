# SVG conversion dependencies

`resvg` / `usvg` v0.45.1 and `tiny-skia` v0.11.4 are used under their
MIT OR Apache-2.0 licenses; the MIT text is retained here in `LICENSE-MIT`.
The unsealed desktop import uses `resvg` to rasterise a restricted static SVG
subset before handing PNG bytes to Holonomy's existing document/image pipeline.
The renderer's *build script* also uses these libraries to turn four checked-in,
MIT-licensed WordCraft icon SVGs into fixed bit masks. This does **not** put
an XML/SVG parser in the sealed default runtime.

The desktop-only PNG encoder is the `png` crate (MIT OR Apache-2.0); the
sealed product retains Holonomy's original bounded `holonomy-image` decoder.
The full dependency and license list for a release should be produced from
the committed `Cargo.lock` before distribution.
