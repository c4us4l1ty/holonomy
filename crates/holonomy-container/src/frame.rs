//! The master frame: chunk 0's plaintext.
//!
//! FR-4 / PRD §5.4 call for "Master Header and Block Allocation Map": block count, document
//! title, KDF parameters. It lives inside chunk 0, which is AEAD-encrypted, so none of this
//! is readable from the file.
//!
//! # A circularity trap that this layout is shaped around
//!
//! The frame records the KDF parameters, and it is tempting to then *derive with* those
//! parameters. That cannot work: the frame is encrypted under `K_enc`, and `K_enc` comes
//! out of the derivation, so reading the parameters would require having already derived
//! the key. The parameters are recorded for a future migration (a newer build must be able
//! to see that a container was written with different costs) and are deliberately **not**
//! an input to [`derive`](crate) or anything in `holonomy-crypto`.
//!
//! The salt does not have this problem, which is exactly why it sits in the clear at
//! fixed offset 0 and the parameters do not. See [`crate::layout`].
//!
//! # Version and magic are allowed in here
//!
//! FR-2.1.2 forbids version flags and plaintext section identifiers *in the container*. A
//! four-byte magic inside authenticated ciphertext is not that: it is indistinguishable
//! from noise to anyone without `K_enc`, and without a version byte a future format change
//! has no way to detect what it is reading.

/// Fixed part of the frame, before the variable-length title.
pub const FRAME_HEADER_LEN: usize = 46;

/// Magic at the start of the frame plaintext. Inside the ciphertext, so invisible on disk.
pub const FRAME_MAGIC: [u8; 4] = *b"HWF1";

/// Format version understood by this build.
pub const FORMAT_VERSION: u16 = 1;

/// Longest document title, in UTF-8 bytes. Bounded so the frame cannot be used to smuggle
/// arbitrary data into chunk 0's plaintext.
pub const MAX_TITLE_LEN: usize = 512;

/// The decryption succeeded but the plaintext is not a frame we understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The magic did not match, so this is not our plaintext.
    BadMagic,
    /// The version is newer than this build understands.
    FutureVersion {
        /// The version found.
        found: u16,
        /// The newest understood.
        supported: u16,
    },
    /// A declared length does not fit inside the slot.
    Truncated {
        /// Bytes the frame wanted.
        wanted: usize,
        /// Bytes available.
        available: usize,
    },
    /// A reserved field was not zero, which means something is writing a format we do not
    /// have. Rejecting rather than ignoring keeps a future writer from being silently
    /// misread by an older one.
    ReservedNotZero {
        /// Byte offset of the field.
        at: usize,
    },
    /// The title was longer than [`MAX_TITLE_LEN`].
    TitleTooLong {
        /// Length found.
        len: usize,
    },
    /// The title was not valid UTF-8.
    TitleNotUtf8,
    /// Phase 13: a chunk authenticated, but decrypted to fewer bytes than the frame's `content_len` says
    /// that chunk holds.
    ///
    /// **Distinct from `AuthenticationFailed` and from `ChunkCountMismatch`, and the distinction is the
    /// point.** Authentication passed, so the key is right and the bytes are intact; the frame's own length
    /// arithmetic is what disagrees. Reporting it as an AEAD failure would tell a user with a perfectly good
    /// passphrase that they typed it wrong, which is the one answer a word processor must never give
    /// wrongly.
    ContentMismatch {
        /// The chunk that came up short.
        index: u64,
        /// Bytes the chunk held.
        got: u64,
        /// Bytes the frame said it holds.
        want: u64,
    },
    /// The frame's `chunk_count` disagreed with the caller's.
    ChunkCountMismatch {
        /// What the frame says.
        frame: u64,
        /// What the caller expected.
        caller: u64,
    },
    /// `content_len` and `chunk_count` were mutually inconsistent.
    ContentLengthMismatch {
        /// What the frame says.
        content_len: u64,
        /// What the chunk count implies.
        implied: u64,
    },
}

impl core::fmt::Display for FrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadMagic => f.write_str("not a master frame"),
            Self::FutureVersion { found, supported } => {
                write!(
                    f,
                    "format version {found} is newer than supported {supported}"
                )
            }
            Self::Truncated { wanted, available } => {
                write!(
                    f,
                    "frame wants {wanted} bytes but only {available} are present"
                )
            }
            Self::ReservedNotZero { at } => write!(f, "reserved byte {at} was not zero"),
            Self::TitleTooLong { len } => write!(f, "title of {len} bytes exceeds the cap"),
            Self::TitleNotUtf8 => f.write_str("title was not valid UTF-8"),
            Self::ChunkCountMismatch { frame, caller } => {
                write!(f, "frame says {frame} chunks, caller expects {caller}")
            }
            Self::ContentMismatch { index, got, want } => write!(
                f,
                "chunk {index} authenticated but held {got} bytes where the frame says {want}"
            ),
            Self::ContentLengthMismatch {
                content_len,
                implied,
            } => {
                write!(
                    f,
                    "content_len {content_len} disagrees with chunk count {implied}"
                )
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Chunk 0's plaintext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterFrame {
    /// Content plaintext length in bytes, across chunks 1..N.
    pub content_len: u64,
    /// Total chunks including this master frame, so 1 for an empty document.
    pub chunk_count: u64,
    /// Document title, shown in the UI once unlocked.
    pub title: String,
    /// VDF iteration count used when this container was written. Informational only -- see
    /// the module docs.
    pub vdf_iterations: u64,
    /// Argon2id memory cost, informational only.
    pub argon2_m_kib: u32,
    /// Argon2id time cost, informational only.
    pub argon2_t: u32,
    /// Argon2id parallelism, informational only.
    pub argon2_p: u32,
}

impl MasterFrame {
    /// A frame for a document of `content_len` bytes in `chunk_count` chunks.
    pub fn new(content_len: u64, chunk_count: u64, title: &str) -> Self {
        Self {
            content_len,
            chunk_count,
            title: title.chars().take(MAX_TITLE_LEN).collect(),
            vdf_iterations: 0,
            argon2_m_kib: 0,
            argon2_t: 0,
            argon2_p: 0,
        }
    }

    /// Record the derivation parameters that produced this container.
    ///
    /// Purely a record. Nothing reads it back to derive anything.
    pub fn with_kdf_params(mut self, vdf_iterations: u64, m_kib: u32, t: u32, p: u32) -> Self {
        self.vdf_iterations = vdf_iterations;
        self.argon2_m_kib = m_kib;
        self.argon2_t = t;
        self.argon2_p = p;
        self
    }

    /// Bytes the serialised frame occupies, which must be ≤ [`FRAME_HEADER_LEN`] +
    /// [`MAX_TITLE_LEN`].
    pub fn encoded_len(&self) -> usize {
        FRAME_HEADER_LEN + self.title.len()
    }

    /// Serialise into a buffer, returning the number of bytes written.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, FrameError> {
        let title = self.title.as_bytes();
        if title.len() > MAX_TITLE_LEN {
            return Err(FrameError::TitleTooLong { len: title.len() });
        }
        let need = self.encoded_len();
        if out.len() < need {
            return Err(FrameError::Truncated {
                wanted: need,
                available: out.len(),
            });
        }
        let mut b = [0u8; FRAME_HEADER_LEN];
        b[0..4].copy_from_slice(&FRAME_MAGIC);
        b[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        // b[6..8] stays zero: reserved.
        b[8..16].copy_from_slice(&self.content_len.to_le_bytes());
        b[16..20].copy_from_slice(&(self.chunk_count as u32).to_le_bytes());
        b[20..22].copy_from_slice(&(title.len() as u16).to_le_bytes());
        // b[22..26] stays zero: reserved.
        b[26..34].copy_from_slice(&self.vdf_iterations.to_le_bytes());
        b[34..38].copy_from_slice(&self.argon2_m_kib.to_le_bytes());
        b[38..42].copy_from_slice(&self.argon2_t.to_le_bytes());
        b[42..46].copy_from_slice(&self.argon2_p.to_le_bytes());
        out[..FRAME_HEADER_LEN].copy_from_slice(&b);
        out[FRAME_HEADER_LEN..need].copy_from_slice(title);
        Ok(need)
    }

    /// Parse from a decrypted chunk-0 plaintext.
    ///
    /// `expected_chunks` is checked against the frame's own count so a container whose
    /// header was swapped for a valid frame from another container is rejected. This is
    /// belt-and-braces: the AEAD already binds the chunk, so an attacker cannot substitute
    /// one without the key. It catches bugs and truncations, not attackers.
    pub fn decode(inp: &[u8], expected_chunks: Option<u64>) -> Result<Self, FrameError> {
        if inp.len() < FRAME_HEADER_LEN {
            return Err(FrameError::Truncated {
                wanted: FRAME_HEADER_LEN,
                available: inp.len(),
            });
        }
        if inp[0..4] != FRAME_MAGIC {
            return Err(FrameError::BadMagic);
        }
        let version = u16::from_le_bytes([inp[4], inp[5]]);
        if version > FORMAT_VERSION {
            return Err(FrameError::FutureVersion {
                found: version,
                supported: FORMAT_VERSION,
            });
        }
        // Report the first non-zero byte, not the start of the field: a diagnostic that
        // points at 22 when byte 25 is the one that changed sends the reader to the wrong
        // place to look.
        for range in [6..8, 22..26] {
            if let Some(at) = range.clone().find(|&i| inp[i] != 0) {
                return Err(FrameError::ReservedNotZero { at });
            }
        }

        let content_len = u64::from_le_bytes(inp[8..16].try_into().expect("8 bytes"));
        let chunk_count = u32::from_le_bytes(inp[16..20].try_into().expect("4 bytes")) as u64;
        let title_len = u16::from_le_bytes([inp[20], inp[21]]) as usize;
        if title_len > MAX_TITLE_LEN {
            return Err(FrameError::TitleTooLong { len: title_len });
        }
        let need = FRAME_HEADER_LEN
            .checked_add(title_len)
            .ok_or(FrameError::Truncated {
                wanted: usize::MAX,
                available: inp.len(),
            })?;
        if inp.len() < need {
            return Err(FrameError::Truncated {
                wanted: need,
                available: inp.len(),
            });
        }

        // chunk_count includes the master frame, so content occupies chunk_count - 1 slots.
        let implied = content_len.div_ceil(crate::layout::CHUNK_PLAINTEXT as u64) + 1;
        if implied != chunk_count {
            return Err(FrameError::ContentLengthMismatch {
                content_len,
                implied: chunk_count,
            });
        }
        if let Some(expected) = expected_chunks {
            if expected != chunk_count {
                return Err(FrameError::ChunkCountMismatch {
                    frame: chunk_count,
                    caller: expected,
                });
            }
        }

        let title = std::str::from_utf8(&inp[FRAME_HEADER_LEN..need])
            .map_err(|_| FrameError::TitleNotUtf8)?
            .to_string();

        Ok(Self {
            content_len,
            chunk_count,
            title,
            vdf_iterations: u64::from_le_bytes(inp[26..34].try_into().expect("8 bytes")),
            argon2_m_kib: u32::from_le_bytes(inp[34..38].try_into().expect("4 bytes")),
            argon2_t: u32::from_le_bytes(inp[38..42].try_into().expect("4 bytes")),
            argon2_p: u32::from_le_bytes(inp[42..46].try_into().expect("4 bytes")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{CHUNK_PLAINTEXT, CHUNK_SLOT};

    fn round_trip(f: &MasterFrame) -> MasterFrame {
        let mut buf = vec![0u8; CHUNK_SLOT as usize];
        let n = f.encode(&mut buf).expect("encode");
        MasterFrame::decode(&buf[..n], Some(f.chunk_count)).expect("decode")
    }

    #[test]
    fn round_trips() {
        let f = MasterFrame::new(1000, 2, "Notes");
        assert_eq!(round_trip(&f), f);
    }

    #[test]
    fn round_trips_with_kdf_params_and_a_unicode_title() {
        let f = MasterFrame::new(5, 2, "\u{6f22}\u{5b57} \u{2014} caf\u{e9} \u{1f600}")
            .with_kdf_params(1_500_000, 131_072, 2, 2);
        let back = round_trip(&f);
        assert_eq!(back, f);
        assert_eq!(back.vdf_iterations, 1_500_000);
        assert_eq!(back.argon2_m_kib, 131_072);
    }

    #[test]
    fn empty_document_is_one_chunk() {
        let f = MasterFrame::new(0, 1, "");
        assert_eq!(f.encoded_len(), FRAME_HEADER_LEN);
        assert_eq!(round_trip(&f), f);
    }

    #[test]
    fn exact_chunk_fit() {
        let n = CHUNK_PLAINTEXT as u64;
        let f = MasterFrame::new(n, 2, "exact");
        assert_eq!(round_trip(&f), f);
        // One byte more needs a third chunk.
        let over = MasterFrame::new(n + 1, 3, "over");
        assert_eq!(round_trip(&over), over);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut buf = vec![0u8; FRAME_HEADER_LEN];
        MasterFrame::new(0, 1, "").encode(&mut buf).expect("encode");
        buf[0] = b'X';
        assert_eq!(MasterFrame::decode(&buf, None), Err(FrameError::BadMagic));
    }

    #[test]
    fn a_future_version_is_refused_rather_than_guessed_at() {
        let mut buf = vec![0u8; FRAME_HEADER_LEN];
        MasterFrame::new(0, 1, "").encode(&mut buf).expect("encode");
        buf[4..6].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        assert_eq!(
            MasterFrame::decode(&buf, None),
            Err(FrameError::FutureVersion {
                found: FORMAT_VERSION + 1,
                supported: FORMAT_VERSION
            })
        );
    }

    #[test]
    fn nonzero_reserved_bytes_are_refused() {
        for at in [6usize, 22, 25] {
            let mut buf = vec![0u8; FRAME_HEADER_LEN];
            MasterFrame::new(0, 1, "").encode(&mut buf).expect("encode");
            buf[at] = 1;
            assert_eq!(
                MasterFrame::decode(&buf, None),
                Err(FrameError::ReservedNotZero { at }),
                "byte {at} should have been rejected"
            );
        }
    }

    #[test]
    fn a_truncated_buffer_is_refused() {
        let f = MasterFrame::new(0, 1, "a title");
        let mut buf = vec![0u8; f.encoded_len()];
        f.encode(&mut buf).expect("encode");
        assert!(matches!(
            MasterFrame::decode(&buf[..FRAME_HEADER_LEN - 1], None),
            Err(FrameError::Truncated { .. })
        ));
        assert!(matches!(
            MasterFrame::decode(&buf[..FRAME_HEADER_LEN], None),
            Err(FrameError::Truncated { .. })
        ));
    }

    /// A frame whose declared counts disagree with each other is rejected. This is the
    /// check that stops a truncated or spliced header from steering the reader into
    /// reading chunks that are not there.
    #[test]
    fn inconsistent_counts_are_refused() {
        let mut buf = vec![0u8; FRAME_HEADER_LEN];
        MasterFrame::new(0, 1, "").encode(&mut buf).expect("encode");
        buf[8..16].copy_from_slice(&9999u64.to_le_bytes()); // content_len without the chunks
        assert_eq!(
            MasterFrame::decode(&buf, None),
            Err(FrameError::ContentLengthMismatch {
                content_len: 9999,
                implied: 1
            })
        );
    }

    #[test]
    fn a_frame_from_a_different_container_is_refused() {
        let f = MasterFrame::new(10, 2, "x");
        let mut buf = vec![0u8; f.encoded_len()];
        f.encode(&mut buf).expect("encode");
        assert_eq!(
            MasterFrame::decode(&buf, Some(5)),
            Err(FrameError::ChunkCountMismatch {
                frame: 2,
                caller: 5
            })
        );
    }

    #[test]
    fn an_over_long_title_is_refused_on_encode() {
        let title = "a".repeat(MAX_TITLE_LEN + 1);
        let f = MasterFrame::new(0, 1, "");
        let mut buf = vec![0u8; CHUNK_SLOT as usize];
        let mut f = f;
        f.title = title;
        assert_eq!(
            f.encode(&mut buf),
            Err(FrameError::TitleTooLong {
                len: MAX_TITLE_LEN + 1
            })
        );
    }

    /// A title that decodes to the cap exactly is accepted; one byte over is not. Pinned
    /// because the boundary is the sort of thing an off-by-one lets through.
    #[test]
    fn title_cap_boundary() {
        let at_cap = MasterFrame::new(0, 1, &"a".repeat(MAX_TITLE_LEN));
        let mut buf = vec![0u8; CHUNK_SLOT as usize];
        let n = at_cap.encode(&mut buf).expect("at the cap is fine");
        assert_eq!(
            MasterFrame::decode(&buf[..n], None)
                .expect("decode")
                .title
                .len(),
            MAX_TITLE_LEN
        );

        // `new` truncates by chars, so build the over-cap case by hand.
        let mut over = at_cap.clone();
        over.title.push('b');
        assert_eq!(
            over.encode(&mut buf),
            Err(FrameError::TitleTooLong {
                len: MAX_TITLE_LEN + 1
            })
        );
    }

    #[test]
    fn a_non_utf8_title_is_refused() {
        let mut buf = vec![0u8; FRAME_HEADER_LEN + 4];
        MasterFrame::new(0, 1, "").encode(&mut buf).expect("encode");
        buf[20..22].copy_from_slice(&4u16.to_le_bytes());
        buf[FRAME_HEADER_LEN..FRAME_HEADER_LEN + 4].copy_from_slice(&[0xFF, 0xFE, 0xFD, 0xFC]);
        assert_eq!(
            MasterFrame::decode(&buf, None),
            Err(FrameError::TitleNotUtf8)
        );
    }

    /// Encoding into a too-small buffer must say so rather than panicking or truncating.
    #[test]
    fn encoding_into_a_small_buffer_is_refused() {
        let f = MasterFrame::new(0, 1, "title");
        let mut buf = vec![0u8; 10];
        assert_eq!(
            f.encode(&mut buf),
            Err(FrameError::Truncated {
                wanted: f.encoded_len(),
                available: 10
            })
        );
    }
}
