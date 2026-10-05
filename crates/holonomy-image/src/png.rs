//! The PNG chunk reader: signature, chunk walk, `IHDR`, and the filters' undo.
//!
//! # Why a hand-written reader
//!
//! PROJECT.md §2.9.5 is explicit -- "No `png` crate. Hand-written chunk reader plus `miniz_oxide`" --
//! and §2.9.1 budgets the entire decoder at **60 KiB** of binary, measured by section delta. The
//! `png` crate is 80-120 KiB once `flate2` and `crc32fast` are in, so it is over budget before a
//! single line of ours is added.
//!
//! What this module is *not* is a general PNG implementation. It reads exactly the subset a document
//! format needs and refuses everything else **loudly**, because the alternative -- accepting a file
//! and rendering something subtly wrong from it -- is the worst outcome available to an editor. The
//! supported subset is tabulated on [`Header`].
//!
//! # What the chunk walk refuses, and why each check is there
//!
//! A PNG is a signature then a sequence of chunks, each `length:u32be type:4 data[length] crc:u32be`.
//!
//! * **`length` against the remaining input.** A length that runs past the end is
//!   [`PngError::TruncatedChunk`], not a slice index panic.
//! * **`length` against [`MAX_CHUNK`].** A 4 GiB length field is not a chunk; it is an attempt to
//!   make the reader allocate before it has validated anything.
//! * **The CRC.** Checked, and *a mismatch is an error* rather than a warning, per §2.9.5's reasoning
//!   above.
//! * **Compression and filter method.** Both must be 0, the only values PNG defines. A file claiming
//!   method 1 is not something this reader can make any promise about, and quietly treating it as
//!   method 0 is how a decoder ends up rendering garbage it calls an image.
//!
//! **Ancillary chunks are skipped, not read.** `tEXt`, `zTXt`, `pHYs`, `gAMA` and friends are stepped
//! over by length. The renderer needs pixels; metadata is the exporter's business and it comes from
//! the document, not from the file.
//!
//! # The five filters, and why the undo is one function
//!
//! PNG stores each scanline as a filter byte plus a payload, and the filter describes how the row
//! relates to the row above and to the pixel to its left. [`unfilter_row`] undoes all five. It is one
//! function with a five-way match rather than five functions because the bpp-dependent arithmetic is
//! shared, and because `Sub`/`Up` differ only in which byte they add while `Average`/`Paeth` are the
//! same shape with a different predictor.
//!
//! **Four of the five read the row being reconstructed**, which is why rows are serially dependent
//! and unfiltering cannot be parallelised across them or reordered.

use crate::error::{PngError, Result};

/// The eight bytes every PNG begins with: `\x89PNG\r\n\x1a\n`.
///
/// Checked byte for byte rather than by length, because a file long enough but wrong in the middle is
/// exactly the input a length-only check accepts.
pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The largest chunk length this reader will believe: 16 MiB.
///
/// The largest legitimate chunk in this subset is `IDAT`, and a 640x360 8-bit RGBA image is well
/// under 1 MiB compressed. 16 MiB leaves two orders of magnitude of headroom and still refuses a
/// length field that is trying to pre-allocate a gigabyte. §2.9.5's budget is 60 KiB of *binary*;
/// this is a runtime ceiling on one buffer.
pub const MAX_CHUNK: u32 = 16 * 1024 * 1024;

/// Largest image this reader accepts, by pixel count: 32 megapixels (8,192 x 4,096).
///
/// A 640x360 page-column raster is 230,400 pixels. The ceiling exists so a crafted `IHDR` claiming
/// 100,000 x 100,000 is refused *before* the caller sizes a buffer from it: every caller allocates
/// from [`Header`] before a pixel arrives, so an unchecked header is an unchecked allocation.
pub const MAX_PIXELS: u64 = 32 * 1024 * 1024;

/// What a PNG's `IHDR` said.
///
/// The supported subset; everything outside it is a named error rather than a best-effort guess.
///
/// | field | accepted | refused |
/// |---|---|---|
/// | bit depth | 8 | 1, 2, 4, 16 -- the sub-8 depths are palette-only, and 16 would need a `/257` rescale to reach 8-bit RGBA |
/// | colour type | 0 grey, 2 RGB, 3 palette, 4 grey+alpha, 6 RGBA | 1, 5 -- hYCb, which needs a colour transform this decoder deliberately does not have |
/// | interlace | 0 none | 1 Adam7 -- see below |
///
/// **Interlace is refused rather than implemented.** Adam7 is a second independent pass over the
/// same filters with different pixel addressing, so supporting it is roughly doubling the decoder for
/// a case a word processor does not hit: §2.9.5 buys exactly one decoder precisely so its cost is
/// bounded, and every writer that matters emits non-interlaced. [`PngError::Interlaced`] says so by
/// name rather than rendering a half-image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per channel. Always 8; see the table above.
    pub bit_depth: u8,
    /// PNG colour type. See the table above.
    pub colour_type: u8,
    /// Interlace method. Always 0; see the note above.
    pub interlace: u8,
}

impl Header {
    /// Channels per pixel for `colour_type`, or 0 if the type is not one of the five.
    ///
    /// `const` on a `match` over a `u8`, so this is a compile-time table wherever the colour type is
    /// already known -- which is the decoder's inner loop.
    pub const fn channels(colour_type: u8) -> usize {
        match colour_type {
            0 => 1, // greyscale
            2 => 3, // truecolour
            3 => 1, // palette index
            4 => 2, // greyscale + alpha
            6 => 4, // truecolour + alpha
            _ => 0, // invalid; validate() rejects it before this is consulted
        }
    }

    /// Bytes per *pixel*, which is what the filter undo needs as `bpp`.
    ///
    /// PNG's spec defines `bpp` in bytes, and at `bit_depth == 8` that is the channel count. The
    /// multiplication is kept because the spec defines it that way, so a future bit depth would not
    /// silently break every filter.
    pub fn bytes_per_pixel(&self) -> usize {
        Self::channels(self.colour_type) * usize::from(self.bit_depth) / 8
    }

    /// Whether this image carries a real alpha *channel*.
    ///
    /// Palette images may carry transparency through `tRNS` instead, so this is false for colour type
    /// 3. [`crate::decode`] returns RGBA either way, so a consumer of decoded output never has to
    /// ask; it exists for the paths that inspect a `Header` before decoding.
    pub const fn has_alpha_channel(&self) -> bool {
        matches!(self.colour_type, 4 | 6)
    }

    /// Pixel count, as `u64` so the multiplication cannot overflow.
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Reject the values this decoder does not support, each with its own error.
    ///
    /// Split out from [`Header::parse`] so the list is one reviewable block rather than something
    /// scattered through the reader, and so every refusal is named.
    fn validate(&self) -> Result<()> {
        if self.interlace != 0 {
            return Err(PngError::Interlaced {
                method: self.interlace,
            });
        }
        if self.bit_depth != 8 {
            return Err(PngError::UnsupportedBitDepth {
                depth: self.bit_depth,
            });
        }
        if Self::channels(self.colour_type) == 0 {
            return Err(PngError::UnsupportedColourType {
                colour_type: self.colour_type,
            });
        }
        if self.width == 0 || self.height == 0 {
            return Err(PngError::ZeroDimension);
        }
        if self.pixels() > MAX_PIXELS {
            return Err(PngError::TooManyPixels {
                pixels: self.pixels(),
                cap: MAX_PIXELS,
            });
        }
        Ok(())
    }

    /// Parse the 13 bytes of an `IHDR` payload.
    ///
    /// `IHDR` is required to be first and is always 13 bytes, so this takes the payload slice rather
    /// than trusting a length the caller has already checked.
    fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < 13 {
            return Err(PngError::TruncatedIhdr {
                have: b.len(),
                want: 13,
            });
        }
        let be = |at: usize| u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        // 8 signature + 4 length + 4 type = the 16 bytes before IHDR's payload, so index 10 here is
        // the compression method byte in PNG's own numbering.
        if b[10] != 0 {
            return Err(PngError::UnsupportedCompressionMethod { method: b[10] });
        }
        if b[11] != 0 {
            return Err(PngError::UnsupportedFilterMethod { method: b[11] });
        }
        let h = Self {
            width: be(0),
            height: be(4),
            bit_depth: b[8],
            colour_type: b[9],
            interlace: b[12],
        };
        h.validate()?;
        Ok(h)
    }
}

/// What the chunk walk found.
#[derive(Debug)]
pub(crate) struct Chunks<'a> {
    /// The image header.
    pub header: Header,
    /// Every `IDAT` payload, in file order.
    ///
    /// A slice of slices rather than one joined buffer, because the join is the decoder's business
    /// and is visible in its allocation count. See [`crate::inflate`] for why joining is mandatory.
    pub idat: Vec<&'a [u8]>,
    /// `PLTE` payload, if present.
    pub palette: Option<&'a [u8]>,
    /// `tRNS` payload, if present.
    pub transparency: Option<&'a [u8]>,
    /// Total `IDAT` bytes, for the caller's allocation arithmetic.
    pub idat_len: usize,
}

/// Walk the chunks, validating every length and CRC, and collect the ones this decoder reads.
///
/// `png` must already have its signature stripped; [`strip_signature`] does that and nothing else.
pub(crate) fn walk(png: &[u8]) -> Result<Chunks<'_>> {
    let mut header: Option<Header> = None;
    let mut idat: Vec<&[u8]> = Vec::new();
    let mut idat_len = 0usize;
    let mut palette = None;
    let mut transparency = None;
    let mut seen_iend = false;
    let mut at = 0usize;

    while at < png.len() {
        if seen_iend {
            // Trailing bytes after IEND are ignored rather than rejected: some writers append
            // metadata, and refusing the whole file for it would be pedantry. The image is already
            // fully described by this point.
            break;
        }
        let remaining = png.len() - at;
        // 4 length + 4 type + 4 crc is the smallest a chunk can be.
        if remaining < 12 {
            return Err(PngError::TruncatedChunk {
                offset: at,
                need: 12,
                have: remaining,
            });
        }
        let len = u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
        if len > MAX_CHUNK {
            return Err(PngError::ChunkTooLarge {
                len,
                cap: MAX_CHUNK,
            });
        }
        let len = len as usize;
        // 8 bytes of framing around the payload.
        if remaining < len + 12 {
            return Err(PngError::TruncatedChunk {
                offset: at,
                need: len + 12,
                have: remaining,
            });
        }
        let kind = &png[at + 4..at + 8];
        let data = &png[at + 8..at + 8 + len];
        let carried = u32::from_be_bytes([
            png[at + 8 + len],
            png[at + 9 + len],
            png[at + 10 + len],
            png[at + 11 + len],
        ]);
        // The CRC covers the type and the payload, not the length.
        let computed = crc32(&png[at + 4..at + 8 + len]);
        if computed != carried {
            return Err(PngError::BadCrc {
                chunk: String::from_utf8_lossy(kind).into_owned(),
                offset: at,
                want: carried,
                got: computed,
            });
        }

        match kind {
            b"IHDR" => {
                if header.is_some() {
                    return Err(PngError::DuplicateChunk { chunk: "IHDR" });
                }
                // `walk` sees the file with the signature already stripped, so the first chunk is at
                // body offset 0. (This check said `at != 8` once, on the reasoning that the 8-byte
                // signature put `IHDR` at 8 -- but that offset is in the *whole file*, and by the time
                // the walk runs the signature is gone. Every fixture failed on it.)
                if at != 0 {
                    return Err(PngError::IhdrNotFirst { offset: at });
                }
                header = Some(Header::parse(data)?);
            }
            b"PLTE" => palette = Some(data),
            b"tRNS" => transparency = Some(data),
            b"IDAT" => {
                if header.is_none() {
                    return Err(PngError::ChunkBeforeHeader { chunk: "IDAT" });
                }
                idat.push(data);
                idat_len += len;
            }
            b"IEND" => seen_iend = true,
            // Everything else is ancillary or unused: stepped over deliberately.
            _ => {}
        }
        at += len + 12;
    }

    let header = header.ok_or(PngError::MissingIhdr)?;
    if idat.is_empty() {
        return Err(PngError::NoImageData);
    }
    // A palette image with no `PLTE` has no colour, and one whose `PLTE` is not a whole number of
    // 3-byte entries is truncated rather than short.
    if header.colour_type == 3 {
        let plte = palette.ok_or(PngError::MissingPalette)?;
        if plte.is_empty() || plte.len() % 3 != 0 || plte.len() > 256 * 3 {
            return Err(PngError::BadPaletteLength { len: plte.len() });
        }
    }
    Ok(Chunks {
        header,
        idat,
        palette,
        transparency,
        idat_len,
    })
}

/// Strip and check the signature, returning the body.
pub(crate) fn strip_signature(png: &[u8]) -> Result<&[u8]> {
    if png.len() < SIGNATURE.len() {
        return Err(PngError::BadSignature {
            have: png.len(),
            want: SIGNATURE.len(),
        });
    }
    if png[..SIGNATURE.len()] != SIGNATURE {
        return Err(PngError::BadSignature {
            have: png.len(),
            want: SIGNATURE.len(),
        });
    }
    Ok(&png[SIGNATURE.len()..])
}

/// Undo one scanline's filter, in place.
///
/// `payload` is the row's `stride` bytes with the filter byte **already removed**; `filter` is that
/// byte. `prev` is the already-reconstructed row above -- also without its filter byte -- or `None`
/// for the first row.
///
/// **The filter byte is a separate argument, not a prefix of `payload`.** A signature that took the
/// whole stored row and did `&mut row[1..]` internally could be called with a payload that had already
/// been split, and would then filter from the wrong offset -- producing plausible bytes rather than an
/// obvious failure. Making the one-way transformation explicit removes that failure mode.
///
/// `bpp` is [`Header::bytes_per_pixel`] -- **bytes**, not samples, because PNG's filters are defined
/// on bytes and a 16-bit image's `Up` adds two bytes at a time.
pub(crate) fn unfilter_row(
    filter: u8,
    payload: &mut [u8],
    prev: Option<&[u8]>,
    bpp: usize,
) -> Result<()> {
    // `payload` arrives with the filter byte already removed, so it *is* the row. See the signature's
    // docs: an earlier version took the whole stored row and stripped the filter byte here, so a
    // caller that had already split it off filtered from the wrong offset and produced a plausible
    // but wrong image rather than an obvious failure.
    let data = payload;
    let stride = data.len();
    match filter {
        // None: the payload is the row.
        0 => Ok(()),
        // Sub: each byte adds the byte `bpp` earlier in the same row.
        1 => {
            // `bpp..stride` rather than `0..stride`: for the first pixel there is nothing to add, and
            // indexing `i - bpp` below zero would be a panic on a truncated file. The spec defines
            // those bytes as unchanged, so starting at `bpp` is the same operation.
            if bpp == 0 {
                return Ok(());
            }
            for i in bpp..stride {
                data[i] = data[i].wrapping_add(data[i - bpp]);
            }
            Ok(())
        }
        // Up: each byte adds the byte directly above. `None` means row 0, where the spec says the
        // prior row is all zeros -- which adding zero reproduces, so the missing case is handled by
        // doing nothing rather than by a second branch.
        2 => {
            if let Some(p) = prev {
                let n = stride.min(p.len());
                for i in 0..n {
                    data[i] = data[i].wrapping_add(p[i]);
                }
            }
            Ok(())
        }
        // Average: add floor((left + above) / 2).
        3 => {
            for i in 0..stride {
                let left = if i >= bpp {
                    u16::from(data[i - bpp])
                } else {
                    0
                };
                let up = prev.and_then(|p| p.get(i)).map_or(0u16, |&v| u16::from(v));
                data[i] = data[i].wrapping_add(((left + up) / 2) as u8);
            }
            Ok(())
        }
        // Paeth: add whichever of left, above and above-left the predictor picks.
        4 => {
            for i in 0..stride {
                let left = if i >= bpp {
                    i16::from(data[i - bpp])
                } else {
                    0
                };
                let up = prev.and_then(|p| p.get(i)).map_or(0i16, |&v| i16::from(v));
                let upleft = if i >= bpp {
                    prev.and_then(|p| p.get(i - bpp))
                        .map_or(0i16, |&v| i16::from(v))
                } else {
                    0
                };
                data[i] = data[i].wrapping_add(paeth(left, up, upleft) as u8);
            }
            Ok(())
        }
        other => Err(PngError::UnknownFilter { filter: other }),
    }
}

/// The Paeth predictor: whichever of `left`, `above` and `above-left` is nearest their linear
/// estimate.
///
/// The estimate is `left + above - above_left`; the answer is the closest of the three to it. The
/// inputs are all bytes and the estimate is bounded by +/-255, so `i16` is safe with room to spare --
/// and using `i32` would have been three times the register traffic for no bound it provides.
#[inline]
fn paeth(left: i16, above: i16, above_left: i16) -> i16 {
    let estimate = left + above - above_left;
    let dl = (estimate - left).abs();
    let da = (estimate - above).abs();
    let dal = (estimate - above_left).abs();
    if dl <= da && dl <= dal {
        left
    } else if da <= dal {
        above
    } else {
        above_left
    }
}

/// CRC-32 as PNG defines it: IEEE 802.3, reflected, polynomial `0xEDB88320`.
///
/// **Written out rather than pulled from `crc32fast`.** §2.9.1's 60 KiB budget is the reason, and the
/// reason it is *this* function is that the table is `const`-constructible: 256 entries computed at
/// compile time by the same recurrence, so the binary carries no 1 KiB table and there is no lazy
/// initialisation or lock at runtime. The cost is one `crc32` call per chunk over bytes just read.
fn crc32(data: &[u8]) -> u32 {
    /// The reflected CRC-32 polynomial.
    const POLY: u32 = 0xEDB8_8320;

    /// Byte-indexed CRC table, built at compile time by `crc_at`'s own recurrence.
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0usize;
        while i < 256 {
            t[i] = crc_at(i as u32, POLY);
            i += 1;
        }
        t
    };

    let mut crc = !0u32;
    for &b in data {
        let idx = ((crc ^ u32::from(b)) & 0xFF) as usize;
        crc = TABLE[idx] ^ (crc >> 8);
    }
    !crc
}

/// One CRC table entry: `crc` shifted through `poly` eight times, least significant bit first.
const fn crc_at(mut crc: u32, poly: u32) -> u32 {
    let mut bit = 0;
    while bit < 8 {
        crc = if crc & 1 != 0 {
            (crc >> 1) ^ poly
        } else {
            crc >> 1
        };
        bit += 1;
    }
    crc
}
