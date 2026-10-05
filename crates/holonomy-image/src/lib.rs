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
pub use iceberg::{CacheError, Entry, IcebergCache, DEFAULT_BUDGET};
pub use inflate::decoded_len;
pub use png::Header;
pub use scale::{resample, Rgba};

/// Read only the `IHDR` of a PNG file, without inflating anything.
///
/// # Why this is a separate step rather than an argument to `decode`
///
/// The Iceberg cache must size its `SecureBlock` *before* it decodes, because the block is the thing
/// the budget is measured in and it is allocated once. It cannot decode into a `Vec` and copy the
/// result into a block -- that is a second 921,600-byte allocation per raster, and it makes the
/// cache's resident figure a peak rather than a resident total.
///
/// So the sequence is: read the header, size the block, decode into the block. Parsing the header
/// twice (once here, once inside `decode`) is the price, and it is worth it: the header walk is
/// integer arithmetic over a few dozen bytes.
///
/// Everything this rejects is the same set [`decode`] rejects, and it rejects it *first*: a header
/// claiming 32 megapixels is refused before any buffer is sized from it.
pub fn read_header(input: &[u8]) -> Result<Header> {
    let body = png::strip_signature(input)?;
    // `walk` needs to reach `IEND` to finish, so this is a full chunk walk. That is a little more work
    // than reading 16 bytes, and it buys the IDAT and palette checks at no extra cost -- `walk` already
    // does them, and duplicating that logic to save a few hundred byte comparisons would be a second
    // place for the two to disagree.
    Ok(png::walk(body)?.header)
}

/// Decode `input` (a whole PNG file) into RGBA in `dst`.
///
/// `dst` must be at least [`decoded_len`] long for the image's header; [`read_header`] gives it one
/// without decoding. The length is *checked*, so a caller that mis-sizes gets
/// [`PngError::DestinationTooSmall`] rather than a panic on a hostile file.
pub fn decode(input: &[u8], dst: &mut [u8]) -> Result<Header> {
    inflate::decode(input, dst)
}
