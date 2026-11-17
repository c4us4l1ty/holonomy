//! The resident slab's plaintext, editable in place without allocating.
//!
//! [`ResidentSet`] holds plaintext; this type is what makes holding it useful. A slab arrives from
//! the world-line as a `Vec<u8>` and is given **slack** beyond its length, so a keystroke writes
//! into capacity the allocation already has: `insert` and `delete` move bytes and nothing else.
//! That is GATES law L-2's keystroke half, and it is measured by a counting allocator in
//! `tests/keystroke_allocations.rs`, not asserted here.
//!
//! The boundary is deliberate and rigid: when an edit no longer fits, the buffer refuses with
//! [`EditOutcome::WouldSplit`] instead of growing. Growing would hide an allocation inside the
//! keystroke path and break the law quietly; refusing turns it into a structural event the editor
//! must handle (split the slab, seal a new segment) where allocations are allowed and counted.
//!
//! Two things this type does **not** do. It does not validate whole-buffer UTF-8 on every edit —
//! that would cost O(slab) per keystroke, which is exactly the "cost proportional to what changed"
//! failure the architecture exists to prevent; instead the buffer is validated once when it is
//! created and every edit checks only its own boundary bytes. And it does not own grapheme or
//! cluster logic: char boundaries are the cheapest correct unit here, and the Eye (Block 5) owns
//! cluster-aware caret movement above it.

use zeroize::Zeroizing;

/// What an in-place edit did, or why it could not be done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    /// The bytes were moved and nothing was allocated.
    InPlace,
    /// The edit would exceed the slab's capacity. Nothing was modified; the caller must split or
    /// merge slabs, where allocation is allowed and counted.
    WouldSplit {
        /// The length the slab would have needed.
        needed: usize,
        /// The capacity it has.
        capacity: usize,
    },
}

/// Why an edit was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextError {
    /// An offset beyond the buffer's last byte.
    OffsetOutOfRange {
        /// The requested offset.
        offset: usize,
        /// The buffer length it was checked against.
        len: usize,
    },
    /// A range that is inverted or beyond the buffer.
    RangeOutOfRange {
        /// The requested start.
        start: usize,
        /// The requested end (exclusive).
        end: usize,
        /// The buffer length it was checked against.
        len: usize,
    },
    /// An offset that falls inside a multi-byte character.
    NotACharBoundary {
        /// The offending offset.
        offset: usize,
    },
    /// Bytes that are not valid UTF-8, either on construction or insertion.
    InvalidUtf8,
}

impl std::fmt::Display for TextError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OffsetOutOfRange { offset, len } => {
                write!(formatter, "offset {offset} is outside a {len}-byte slab")
            }
            Self::RangeOutOfRange { start, end, len } => write!(
                formatter,
                "range {start}..{end} is invalid for a {len}-byte slab"
            ),
            Self::NotACharBoundary { offset } => {
                write!(formatter, "offset {offset} splits a character")
            }
            Self::InvalidUtf8 => formatter.write_str("the bytes are not valid UTF-8"),
        }
    }
}

impl std::error::Error for TextError {}

/// One slab's plaintext with spare capacity, editable in place.
pub struct SlabText {
    bytes: Zeroizing<Vec<u8>>,
}

impl SlabText {
    /// Takes ownership of a segment's plaintext and reserves `slack` bytes beyond it.
    ///
    /// This is slab-boundary work and is allowed to allocate: it runs when a segment faults in,
    /// not on a keystroke.
    ///
    /// # Errors
    ///
    /// [`TextError::InvalidUtf8`] if `plaintext` is not valid UTF-8.
    ///
    /// # Panics
    ///
    /// If `plaintext.len() + slack` exceeds the maximum allocation size — the same panic
    /// `Vec::reserve` gives, and reachable only by configuring an absurd slack.
    pub fn from_plaintext(plaintext: Vec<u8>, slack: usize) -> Result<Self, TextError> {
        if std::str::from_utf8(&plaintext).is_err() {
            return Err(TextError::InvalidUtf8);
        }
        let mut bytes = Zeroizing::new(plaintext);
        bytes.reserve(slack);
        Ok(Self { bytes })
    }

    /// Builds a slab from a `&str` with `slack` spare bytes. Convenience for callers and tests
    /// that already hold text rather than world-line bytes.
    #[must_use]
    pub fn from_text(text: &str, slack: usize) -> Self {
        let mut bytes = Vec::with_capacity(text.len() + slack);
        bytes.extend_from_slice(text.as_bytes());
        Self {
            bytes: Zeroizing::new(bytes),
        }
    }

    /// The plaintext.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    /// The plaintext as a `&str` (the buffer is UTF-8 by construction).
    #[must_use]
    pub fn as_str(&self) -> &str {
        // SAFETY-free justification: every constructor validates or receives UTF-8, and every
        // edit validates its own insertion and only cuts on char boundaries.
        std::str::from_utf8(self.bytes.as_slice()).expect("SlabText upholds UTF-8 by construction")
    }

    /// The number of bytes held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the slab holds no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// How many bytes fit without allocating.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.bytes.capacity()
    }

    /// How many more bytes can be inserted before [`EditOutcome::WouldSplit`].
    #[must_use]
    pub fn spare(&self) -> usize {
        self.capacity().saturating_sub(self.len())
    }

    /// Inserts `text` at byte offset `at`, in place, without allocating.
    ///
    /// # Errors
    ///
    /// [`TextError::OffsetOutOfRange`] past the end, [`TextError::NotACharBoundary`] inside a
    /// character, or [`TextError::InvalidUtf8`] for bytes that would corrupt the buffer.
    ///
    /// # Panics
    ///
    /// Does not panic: a full slab returns `Ok(EditOutcome::WouldSplit)` with the buffer untouched.
    pub fn insert(&mut self, at: usize, text: &[u8]) -> Result<EditOutcome, TextError> {
        let len = self.bytes.len();
        if at > len {
            return Err(TextError::OffsetOutOfRange { offset: at, len });
        }
        if !is_char_boundary(self.bytes.as_slice(), at) {
            return Err(TextError::NotACharBoundary { offset: at });
        }
        if std::str::from_utf8(text).is_err() {
            return Err(TextError::InvalidUtf8);
        }
        // Saturating, not wrapping: on any width this either fits or is far larger than the
        // capacity, which is exactly the refusal condition.
        let needed = len.saturating_add(text.len());
        if needed > self.capacity() {
            return Ok(EditOutcome::WouldSplit {
                needed,
                capacity: self.capacity(),
            });
        }
        // Within capacity: `resize` reserves zero additional bytes, `copy_within` shifts the
        // tail, and `copy_from_slice` writes the new bytes. No allocator call can occur here.
        self.bytes.resize(needed, 0);
        self.bytes.copy_within(at..len, at + text.len());
        self.bytes[at..at + text.len()].copy_from_slice(text);
        Ok(EditOutcome::InPlace)
    }

    /// Deletes `start..end`, in place, without allocating.
    ///
    /// Callers that end up with `is_empty()` should merge the slab away; that is structural work
    /// and belongs with the spine, not here.
    ///
    /// # Errors
    ///
    /// [`TextError::RangeOutOfRange`] for an inverted or out-of-bounds range, or
    /// [`TextError::NotACharBoundary`] for an edge inside a character.
    pub fn delete(&mut self, start: usize, end: usize) -> Result<(), TextError> {
        let len = self.bytes.len();
        if start > end || end > len {
            return Err(TextError::RangeOutOfRange { start, end, len });
        }
        if !is_char_boundary(self.bytes.as_slice(), start) {
            return Err(TextError::NotACharBoundary { offset: start });
        }
        if !is_char_boundary(self.bytes.as_slice(), end) {
            return Err(TextError::NotACharBoundary { offset: end });
        }
        self.bytes.copy_within(end..len, start);
        self.bytes.truncate(len - (end - start));
        Ok(())
    }
}

/// Whether `at` is a UTF-8 character boundary. Constant time: a continuation byte never starts a
/// character, and the end of the buffer always is a boundary.
fn is_char_boundary(bytes: &[u8], at: usize) -> bool {
    at == bytes.len() || bytes.get(at).is_some_and(|byte| *byte & 0xC0 != 0x80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_in_place_keeps_the_text_ordered() {
        let mut slab = SlabText::from_text("hello world", 16);
        assert_eq!(slab.insert(5, b",").expect("fits"), EditOutcome::InPlace);
        assert_eq!(slab.as_str(), "hello, world");
        assert_eq!(slab.len(), 12);

        slab.delete(5, 6).expect("in range");
        assert_eq!(slab.as_str(), "hello world");
    }

    #[test]
    fn insertion_at_both_ends_works() {
        let mut slab = SlabText::from_text("middle", 8);
        assert_eq!(slab.insert(0, b"a ").expect("fits"), EditOutcome::InPlace);
        assert_eq!(slab.insert(8, b" end").expect("fits"), EditOutcome::InPlace);
        assert_eq!(slab.as_str(), "a middle end");
    }

    #[test]
    fn deleting_everything_leaves_an_empty_slab_that_can_be_reused() {
        let mut slab = SlabText::from_text("all of it", 4);
        slab.delete(0, 9).expect("covers the slab");
        assert!(slab.is_empty());
        assert_eq!(slab.insert(0, b"new").expect("fits"), EditOutcome::InPlace);
        assert_eq!(slab.as_str(), "new");
    }

    #[test]
    fn a_full_slab_refuses_instead_of_allocating() {
        let mut slab = SlabText::from_text("abcd", 0);
        // `Vec` may round capacity up, so fill to the real capacity before expecting a refusal.
        let capacity = slab.capacity();
        let filler = vec![b'x'; capacity - slab.len()];
        assert_eq!(slab.insert(4, &filler).expect("fits"), EditOutcome::InPlace);
        assert_eq!(slab.len(), capacity);
        assert_eq!(slab.spare(), 0);

        let before = slab.as_str().to_owned();
        assert_eq!(
            slab.insert(0, b"!").expect("refusal is not an error"),
            EditOutcome::WouldSplit {
                needed: capacity + 1,
                capacity
            }
        );
        assert_eq!(slab.as_str(), before, "a refusal must not modify the slab");
    }

    #[test]
    fn utf8_boundaries_are_respected_on_both_edges() {
        let mut slab = SlabText::from_text("é", 8); // two bytes
        assert_eq!(
            slab.insert(1, b"x"),
            Err(TextError::NotACharBoundary { offset: 1 })
        );
        assert_eq!(
            slab.delete(1, 2),
            Err(TextError::NotACharBoundary { offset: 1 })
        );
        assert_eq!(slab.insert(2, b"!").expect("fits"), EditOutcome::InPlace);
        assert_eq!(slab.as_str(), "é!");
    }

    #[test]
    fn invalid_bytes_are_never_admitted() {
        let mut slab = SlabText::from_text("ok", 8);
        assert_eq!(slab.insert(2, &[0xFF]), Err(TextError::InvalidUtf8));
        assert_eq!(slab.as_str(), "ok");
        assert_eq!(
            SlabText::from_plaintext(vec![0xF0, 0x28], 0).map(|_| ()),
            Err(TextError::InvalidUtf8)
        );
    }

    #[test]
    fn out_of_range_edits_are_refused() {
        let mut slab = SlabText::from_text("abc", 8);
        assert_eq!(
            slab.insert(4, b"x"),
            Err(TextError::OffsetOutOfRange { offset: 4, len: 3 })
        );
        assert_eq!(
            slab.delete(2, 9),
            Err(TextError::RangeOutOfRange {
                start: 2,
                end: 9,
                len: 3
            })
        );
        assert_eq!(
            slab.delete(2, 1),
            Err(TextError::RangeOutOfRange {
                start: 2,
                end: 1,
                len: 3
            })
        );
    }

    #[test]
    fn random_edits_match_a_string_model() {
        use crate::test_support::XorShift;

        // Multi-byte characters on purpose: the boundary rules must hold for every width.
        let alphabet = ['a', 'b', ' ', 'é', '中', '🙂'];
        for seed in [3u64, 11, 777, 0x0000_ABCD] {
            let mut rng = XorShift::new(seed);
            let mut text = SlabText::from_text("", 512);
            let mut model = String::new();

            for _ in 0..500 {
                let boundaries: Vec<usize> = model
                    .char_indices()
                    .map(|(index, _)| index)
                    .chain(std::iter::once(model.len()))
                    .collect();

                if rng.below(2) == 0 || model.is_empty() {
                    let at = boundaries[rng.below(boundaries.len() as u64) as usize];
                    let character = alphabet[rng.below(alphabet.len() as u64) as usize];
                    let mut buffer = [0u8; 4];
                    let encoded = character.encode_utf8(&mut buffer).as_bytes().to_vec();
                    if text.spare() < encoded.len() {
                        continue;
                    }
                    assert_eq!(
                        text.insert(at, &encoded).expect("a boundary offset is valid"),
                        EditOutcome::InPlace
                    );
                    model.insert(at, character);
                } else {
                    let first = boundaries[rng.below(boundaries.len() as u64) as usize];
                    let second = boundaries[rng.below(boundaries.len() as u64) as usize];
                    let (start, end) = if first <= second {
                        (first, second)
                    } else {
                        (second, first)
                    };
                    text.delete(start, end).expect("boundary offsets are valid");
                    model.replace_range(start..end, "");
                }

                assert_eq!(text.as_str(), model, "seed {seed}");
                assert_eq!(text.len(), model.len(), "seed {seed}");
            }
        }
    }

    #[test]
    fn slack_is_reported_and_usable() {
        let slab = SlabText::from_text("abc", 5);
        assert!(slab.capacity() >= 8);
        assert_eq!(slab.spare(), slab.capacity() - 3);
    }
}
