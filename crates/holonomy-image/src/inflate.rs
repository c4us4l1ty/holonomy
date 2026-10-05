//! `IDAT` -> RGBA: inflate, undo the filters, and expand whatever colour type arrived.
//!
//! # Everything is sized from the header, before a pixel exists
//!
//! [`decode`] takes `&mut [u8]` and writes exactly `width * height * 4` bytes. It allocates a joined
//! `IDAT` buffer and an inflated buffer, and nothing else, because the caller is the Iceberg cache,
//! which hands this a [`SecureBlock`](holonomy_secure::SecureBlock) slice it sized from the image's
//! *own* dimensions. Returning a `Vec<u8>` would be a third allocation and a 921,600-byte copy per
//! raster, and would make the cache's memory accounting an estimate rather than a measurement.
//!
//! # Why the `IDAT` parts must be joined before inflating
//!
//! A zlib stream is split across `IDAT` chunks at whatever boundary the writer chose, so the
//! concatenation of the payloads *is* the stream and it must be inflated as one piece. Inflating the
//! parts separately fails for every multi-`IDAT` PNG, which is most of them: `libpng` caps `IDAT` at
//! 8 KiB by default, so a 640x360 image is thirty-odd chunks. An earlier draft of this file inflated
//! each part *and* also inflated the join -- the kind of thing that passes a hand-written 40-byte
//! fixture and fails every real encoder's output.
//!
//! # Why the output is always RGBA
//!
//! Two of the five colour types carry no alpha and one carries it through a palette. Normalising to
//! RGBA here means the cache, the scaler and the painter each have exactly one pixel layout, and
//! `Frame`/`PaintStats` grow no per-format branch. The cost is that a greyscale image is 4x larger
//! than it needs to be -- the same trade §2.9.3 already made in choosing page-column-width rasters,
//! taken at the same place.
//!
//! # The stride the filters see is the *source* stride
//!
//! A palette image has `bpp == 1` and a stride of `width`; an RGBA one has `bpp == 4` and a stride of
//! `width * 4`. Unfiltering happens on the source layout and expansion is a separate pass. The other
//! order would be wrong: the filters were chosen by the *encoder* against the source layout and are
//! not required to be self-consistent under a different one.

use crate::error::{PngError, Result};
use crate::png::{self, Chunks, Header};

/// Decode `input` as a whole PNG file, writing `width * height * 4` bytes of RGBA into `dst`.
///
/// `dst` must be at least [`decoded_len`]. The length is *checked*, not trusted, so a caller's
/// arithmetic error is [`PngError::DestinationTooSmall`] rather than a panic on a hostile file.
pub fn decode(input: &[u8], dst: &mut [u8]) -> Result<Header> {
    let body = png::strip_signature(input)?;
    let chunks = png::walk(body)?;
    let header = chunks.header;

    let want = decoded_len(&header);
    if dst.len() < want {
        return Err(PngError::DestinationTooSmall {
            want,
            have: dst.len(),
        });
    }

    // Source bytes per row excluding the filter byte, and the exact inflated stream length:
    // one filter byte plus `stride` payload bytes, per row.
    let channels = Header::channels(header.colour_type);
    let stride = header.width as usize * channels;
    let raw_len = (stride + 1) * header.height as usize;

    let mut raw = inflate(&chunks, raw_len)?;
    if raw.len() != raw_len {
        return Err(PngError::UnexpectedDataLength {
            want: raw_len,
            got: raw.len(),
        });
    }

    unfilter(&header, stride, &mut raw)?;
    expand(
        &header,
        stride,
        &raw,
        chunks.palette,
        chunks.transparency,
        &mut dst[..want],
    )?;
    Ok(header)
}

/// Bytes of RGBA [`decode`] writes for `header`: `width * height * 4`.
///
/// Saturating, and `u64` internally, because the cache calls this *before* decoding in order to size
/// its `SecureBlock`. A header claiming 32 megapixels must not overflow a `usize` on the way to being
/// refused.
pub fn decoded_len(header: &Header) -> usize {
    header.pixels().saturating_mul(4).min(usize::MAX as u64) as usize
}

/// Join the `IDAT` payloads, then inflate them as one zlib stream into a buffer of `expect` bytes.
fn inflate(chunks: &Chunks<'_>, expect: usize) -> Result<Vec<u8>> {
    // One `IDAT` is the common case for a hand-written file and needs no join. More than one is the
    // common case for a real encoder, and the join is mandatory there.
    let joined: Vec<u8> = if chunks.idat.len() == 1 {
        chunks.idat[0].to_vec()
    } else {
        let mut v = Vec::with_capacity(chunks.idat_len);
        for part in &chunks.idat {
            v.extend_from_slice(part);
        }
        v
    };

    // The *bounded* helper, not the one-shot one. `decompress_to_vec_zlib` sizes its output from the
    // stream, so a crafted `IHDR`+stream that inflates to a gigabyte is allocated before the length
    // check that would have refused it. `_with_limit` caps the output at `expect` -- the exact byte
    // count the `IHDR` implies -- so the bound sits at the allocation instead of after it. That
    // ordering is the whole reason this is not a one-liner.
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&joined, expect).map_err(|e| {
        // A stream that filled the cap is a *length* problem and gets the length error, because
        // that is what a caller can act on; everything else is a malformed stream.
        if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput {
            PngError::UnexpectedDataLength {
                want: expect,
                got: e.output.len(),
            }
        } else {
            PngError::Inflate
        }
    })
}

/// Undo the per-scanline filter over `raw`, in place.
///
/// One pass, row by row: `Up`, `Average` and `Paeth` all read the row above, so rows are serially
/// dependent and this cannot be vectorised across rows or run in parallel without changing what the
/// filter means.
fn unfilter(header: &Header, stride: usize, raw: &mut [u8]) -> Result<()> {
    let bpp = header.bytes_per_pixel().max(1);
    let row_len = stride + 1;
    for y in 0..header.height as usize {
        let (this, rest) = raw.split_at_mut(y * row_len);
        let (filter, payload) = this.split_at_mut(1);
        png::unfilter_row(filter[0], payload, rest.get(..stride), bpp)?;
    }
    Ok(())
}

/// Expand the reconstructed source rows into RGBA.
///
/// | colour type | source | RGBA |
/// |---|---|---|
/// | 0 greyscale | 1 byte | `(g, g, g, 255)` |
/// | 2 truecolour | 3 bytes | `(r, g, b, 255)` |
/// | 3 palette | 1 index | `PLTE[index]`, alpha from `tRNS` or 255 |
/// | 4 grey+alpha | 2 bytes | `(g, g, g, a)` |
/// | 6 truecolour+alpha | 4 bytes | copied |
///
/// A per-pixel loop with no SIMD here, deliberately. The cache always *downscales* (§2.9.3), so this
/// output is consumed and discarded within microseconds by [`crate::scale`], and a source-width RGBA
/// buffer for a 1920x1080 photo is 8.3 MiB allocated to exist briefly. Scaling straight from the
/// source rows would avoid it, but it would put two pixel layouts in the scaler's inner loop -- and
/// the scaler is the hand-written integer code that has to stay readable to be auditable.
fn expand(
    header: &Header,
    stride: usize,
    raw: &[u8],
    palette: Option<&[u8]>,
    transparency: Option<&[u8]>,
    dst: &mut [u8],
) -> Result<()> {
    let w = header.width as usize;
    let h = header.height as usize;
    debug_assert_eq!(
        dst.len(),
        w * h * 4,
        "decoded_len and expand must agree on the output size"
    );
    match header.colour_type {
        // Greyscale: the value goes into all three channels, alpha opaque.
        0 => {
            for y in 0..h {
                let src = &raw[y * stride..y * stride + w];
                let out = &mut dst[y * w * 4..(y + 1) * w * 4];
                for (i, &g) in src.iter().enumerate() {
                    out[i * 4] = g;
                    out[i * 4 + 1] = g;
                    out[i * 4 + 2] = g;
                    out[i * 4 + 3] = 255;
                }
            }
        }
        // Truecolour: RGB in, alpha forced opaque.
        2 => {
            for y in 0..h {
                let src = &raw[y * stride..y * stride + w * 3];
                let out = &mut dst[y * w * 4..(y + 1) * w * 4];
                // `as_chunks` rather than `chunks_exact`: the slice length is already `w * 3`, so
                // the remainder it returns is provably empty and the compiler can drop the bounds
                // work in the loop.
                let (px, rest) = src.as_chunks::<3>();
                debug_assert!(rest.is_empty(), "the row length is exactly w * 3 by construction");
                for (i, px) in px.iter().enumerate() {
                    out[i * 4] = px[0];
                    out[i * 4 + 1] = px[1];
                    out[i * 4 + 2] = px[2];
                    out[i * 4 + 3] = 255;
                }
            }
        }
        // Palette: one index in, one RGBA out. `png::walk` has already refused a `PLTE` that is
        // missing, empty, or not a whole number of entries, so `entries` here is at least 1.
        3 => {
            let plte = palette.ok_or(PngError::MissingPalette)?;
            let entries = plte.len() / 3;
            for y in 0..h {
                let src = &raw[y * stride..y * stride + w];
                let out = &mut dst[y * w * 4..(y + 1) * w * 4];
                for (i, &idx) in src.iter().enumerate() {
                    let e = usize::from(idx);
                    if e >= entries {
                        // An index past the table is a corrupt image. Writing black for it would
                        // invent a colour the author never chose, so the file is reported instead.
                        return Err(PngError::PaletteIndexOutOfRange {
                            index: idx,
                            entries,
                        });
                    }
                    out[i * 4] = plte[e * 3];
                    out[i * 4 + 1] = plte[e * 3 + 1];
                    out[i * 4 + 2] = plte[e * 3 + 2];
                    // `tRNS` for a palette image is a run of alpha values *parallel to* the palette
                    // and shorter than it, so an index past its end is opaque by definition.
                    out[i * 4 + 3] = transparency.and_then(|t| t.get(e).copied()).unwrap_or(255);
                }
            }
        }
        // Greyscale + alpha.
        4 => {
            for y in 0..h {
                let src = &raw[y * stride..y * stride + w * 2];
                let out = &mut dst[y * w * 4..(y + 1) * w * 4];
                let (px, rest) = src.as_chunks::<2>();
                debug_assert!(rest.is_empty(), "the row length is exactly w * 2 by construction");
                for (i, px) in px.iter().enumerate() {
                    out[i * 4] = px[0];
                    out[i * 4 + 1] = px[0];
                    out[i * 4 + 2] = px[0];
                    out[i * 4 + 3] = px[1];
                }
            }
        }
        // Truecolour + alpha: already RGBA, one copy per row.
        6 => {
            for y in 0..h {
                let src = &raw[y * stride..y * stride + w * 4];
                let out = &mut dst[y * w * 4..(y + 1) * w * 4];
                out.copy_from_slice(src);
            }
        }
        other => return Err(PngError::UnsupportedColourType { colour_type: other }),
    }
    Ok(())
}
