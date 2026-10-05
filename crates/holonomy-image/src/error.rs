//! Why a PNG could not be decoded, as a closed enum.
//!
//! PROJECT.md §3 makes this deliberate: "A sandboxed process gets an error type it can exhaustively
//! match, not a catch-all that swallows the fault." Every variant here names a *specific* refusal,
//! because the whole design of [`crate::png`] is that an unsupported file is reported rather than
//! guessed at -- and a catch-all would put those guesses back in.

use core::fmt;

/// A decoding failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PngError {
    /// The file does not start with the eight PNG signature bytes.
    ///
    /// `have`/`want` are lengths rather than the differing byte index, so a caller that misrouted a
    /// file to this decoder can say so without a second read.
    BadSignature {
        /// Bytes the file has.
        have: usize,
        /// Bytes the signature needs.
        want: usize,
    },
    /// A chunk's framing ran past the end of the file.
    TruncatedChunk {
        /// Byte offset the chunk started at.
        offset: usize,
        /// Bytes the chunk needs.
        need: usize,
        /// Bytes left in the file.
        have: usize,
    },
    /// A chunk claimed a length above [`crate::png::MAX_CHUNK`].
    ///
    /// Refused before the length is used to size anything.
    ChunkTooLarge {
        /// The length the chunk claimed.
        len: u32,
        /// The ceiling this reader applies.
        cap: u32,
    },
    /// A chunk's CRC did not match its contents.
    ///
    /// An error, not a warning: §2.9.5 keeps one decoder because PNG is the lossless format that
    /// suits documents, and a document image with a corrupt chunk is a corrupt document.
    BadCrc {
        /// The chunk's four type bytes, as text.
        chunk: String,
        /// Byte offset the chunk started at.
        offset: usize,
        /// The CRC the chunk carries.
        want: u32,
        /// The CRC computed over the chunk's type and payload.
        got: u32,
    },
    /// `IHDR` was not the first chunk.
    IhdrNotFirst {
        /// Byte offset it appeared at. Always 8 in a well-formed file.
        offset: usize,
    },
    /// `IHDR` appeared twice.
    DuplicateChunk {
        /// The chunk name.
        chunk: &'static str,
    },
    /// A chunk that must follow `IHDR` came first.
    ChunkBeforeHeader {
        /// The chunk name.
        chunk: &'static str,
    },
    /// The file has no `IHDR`.
    MissingIhdr,
    /// `IHDR`'s payload was shorter than its fixed 13 bytes.
    TruncatedIhdr {
        /// Bytes present.
        have: usize,
        /// Bytes required.
        want: usize,
    },
    /// `IHDR` claimed zero width or height.
    ZeroDimension,
    /// `IHDR` claimed more pixels than [`crate::png::MAX_PIXELS`].
    ///
    /// Checked *before* the caller sizes a buffer from the header, because every caller allocates
    /// from `Header` before a pixel arrives.
    TooManyPixels {
        /// Pixels claimed.
        pixels: u64,
        /// The ceiling.
        cap: u64,
    },
    /// The image is interlaced. Refused by design; see [`crate::png::Header`].
    Interlaced {
        /// The interlace method. Always 1 in practice.
        method: u8,
    },
    /// A bit depth other than 8.
    UnsupportedBitDepth {
        /// Bits per channel.
        depth: u8,
    },
    /// A colour type this decoder does not handle.
    UnsupportedColourType {
        /// The PNG colour type byte.
        colour_type: u8,
    },
    /// A compression method other than 0. PNG defines only 0.
    UnsupportedCompressionMethod {
        /// The method byte.
        method: u8,
    },
    /// A filter method other than 0. PNG defines only 0.
    UnsupportedFilterMethod {
        /// The method byte.
        method: u8,
    },
    /// A scanline carried a filter byte outside `0..=4`.
    UnknownFilter {
        /// The filter byte.
        filter: u8,
    },
    /// The file has no `IDAT`.
    NoImageData,
    /// A palette image has no `PLTE`.
    MissingPalette,
    /// `PLTE` is empty, not a whole number of 3-byte entries, or longer than 256 entries.
    BadPaletteLength {
        /// `PLTE`'s length in bytes.
        len: usize,
    },
    /// A palette index pointed past the end of `PLTE`.
    PaletteIndexOutOfRange {
        /// The offending index.
        index: u8,
        /// How many entries the palette has.
        entries: usize,
    },
    /// The inflated `IDAT` stream was not the byte count the `IHDR` implies.
    ///
    /// `want` is `height * (1 + stride)`, which is exact, so a short *or* long stream is caught.
    UnexpectedDataLength {
        /// Bytes the header implies.
        want: usize,
        /// Bytes the stream produced.
        got: usize,
    },
    /// `miniz_oxide` refused the zlib stream.
    Inflate,
    /// The destination is too small for the image.
    DestinationTooSmall {
        /// Bytes the image needs.
        want: usize,
        /// Bytes the destination has.
        have: usize,
    },
    /// The *source* buffer is shorter than the dimensions say it should be.
    ///
    /// A caller error rather than a file error: `resample_pixels` takes the dimensions and the bytes
    /// separately, so a half-filled buffer would otherwise be read past its end in the horizontal pass.
    /// An [`crate::scale::Rgba`] can never trip this -- its constructor allocates `bytes_for` -- so this
    /// only fires for a hand-built call, and it is cheap to be sure.
    SourceTooSmall {
        /// Bytes `width * height * 4`.
        want: usize,
        /// Bytes actually present.
        have: usize,
    },
}

impl fmt::Display for PngError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadSignature { have, want } => {
                write!(f, "not a PNG: {have} bytes, signature needs {want}")
            }
            Self::TruncatedChunk { offset, need, have } => write!(
                f,
                "chunk at {offset} needs {need} bytes, {have} left in the file"
            ),
            Self::ChunkTooLarge { len, cap } => {
                write!(f, "chunk length {len} exceeds the {cap} ceiling")
            }
            Self::BadCrc {
                chunk,
                offset,
                want,
                got,
            } => write!(
                f,
                "{chunk} chunk at {offset} computes CRC {got:#010x}, carries {want:#010x}"
            ),
            Self::IhdrNotFirst { offset } => {
                write!(f, "IHDR must be the first chunk, found it at {offset}")
            }
            Self::DuplicateChunk { chunk } => write!(f, "{chunk} appears twice"),
            Self::ChunkBeforeHeader { chunk } => write!(f, "{chunk} appears before IHDR"),
            Self::MissingIhdr => f.write_str("no IHDR chunk"),
            Self::TruncatedIhdr { have, want } => {
                write!(f, "IHDR payload is {have} bytes, needs {want}")
            }
            Self::ZeroDimension => f.write_str("IHDR declares a zero width or height"),
            Self::TooManyPixels { pixels, cap } => {
                write!(f, "{pixels} pixels exceeds the {cap} ceiling")
            }
            Self::Interlaced { method } => write!(
                f,
                "interlaced (method {method}); this decoder does not implement Adam7"
            ),
            Self::UnsupportedBitDepth { depth } => write!(f, "{depth} bits per channel, need 8"),
            Self::UnsupportedColourType { colour_type } => {
                write!(f, "colour type {colour_type} is not handled")
            }
            Self::UnsupportedCompressionMethod { method } => {
                write!(f, "compression method {method}, PNG defines only 0")
            }
            Self::UnsupportedFilterMethod { method } => {
                write!(f, "filter method {method}, PNG defines only 0")
            }
            Self::UnknownFilter { filter } => write!(f, "scanline filter {filter} is not 0..=4"),
            Self::NoImageData => f.write_str("no IDAT chunk"),
            Self::MissingPalette => f.write_str("palette image with no PLTE"),
            Self::BadPaletteLength { len } => {
                write!(
                    f,
                    "PLTE length {len} is not a whole number of 3-byte entries"
                )
            }
            Self::PaletteIndexOutOfRange { index, entries } => write!(
                f,
                "palette index {index} is past the {entries} entries PLTE declares"
            ),
            Self::UnexpectedDataLength { want, got } => {
                write!(f, "IDAT inflated to {got} bytes, the header implies {want}")
            }
            Self::Inflate => f.write_str("the zlib stream is malformed"),
            Self::SourceTooSmall { want, have } => {
                write!(f, "source holds {have} bytes, its dimensions need {want}")
            }
            Self::DestinationTooSmall { want, have } => {
                write!(f, "destination holds {have} bytes, the image needs {want}")
            }
        }
    }
}

impl std::error::Error for PngError {}

/// Shorthand for this crate's results.
pub type Result<T> = core::result::Result<T, PngError>;
