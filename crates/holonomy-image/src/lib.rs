//! Phase 9C: the image path, from a PNG in the container to pixels on the page.
//!
//! # The three stages, and why they are separate types
//!
//! | module | type | owns |
//! |---|---|---|
//! | [`png`] | [`png::Header`] | what the file claimed |
//! | [`inflate`] | nothing | the decoded pixels, into a caller-owned buffer |
//! | [`scale`] | [`scale::Rgba`] | a raster at one particular size |
//! | [`iceberg`] | [`IcebergCache`] | *which* rasters exist, and scrubs the ones that do not |
//!
//! The separation is what makes the cache's budget auditable. A raster is a [`SecureBlock`], and
//! `SecureBlock::len()` is the number the gate asserts on, so "how much decoded image memory exists"
//! is a sum of `usize`s rather than a guess about how many pixels a decoder happened to produce.
//!
//! # No floats, no curves, no runtime evaluation
//!
//! The Zero-Bézier Invariant (§2.2) forbids runtime curve evaluation, and [`scale`] extends it to
//! resampling: the scaler is integer-only fixed point, so there is no interpolation *evaluation* of
//! anything, only integer multiply-add. This is stated in [`scale`] too; it is repeated here because
//! the tempting "just use `f32` and bilinear" version of a scaler is the single most likely way this
//! invariant gets broken by accident.
//!
//! # The decoder's cost, and what it is not allowed to become
//!
//! §2.9.1 budgets the decoder at **60 KiB** of binary, measured by section delta. The `png` crate is
//! 80-120 KiB, so the chunk reader in [`png`] is hand-written. What is *not* hand-written is inflate:
//! [`miniz_oxide`] does DEFLATE, because writing an inflate from scratch would be both larger than
//! the whole budget and less correct.

mod error;
pub mod iceberg;
mod inflate;
pub mod png;
pub mod scale;

pub use error::{PngError, Result};
pub use inflate::decoded_len;
pub use scale::{resample, Rgba};

/// Decode `input` (a whole PNG file) into RGBA in `dst`.
///
/// `dst` must be at least [`decoded_len`] for the image's header, which the caller cannot know until
/// the file is parsed -- so the two-step path is [`decode`] when the caller sizes the buffer itself
/// (the Iceberg cache does, from a header it has already read) and this convenience form otherwise.
pub fn decode(input: &[u8], dst: &mut [u8]) -> Result<png::Header> {
    inflate::decode(input, dst)
}
