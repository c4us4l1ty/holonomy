//! Phase 9C: content-addressed image assets, and where they live in the payload.
//!
//! # Why a catalog in the payload and not a container section
//!
//! The container format is **frozen**. `FORMAT_VERSION` does not move, `MasterFrame` does not gain
//! a field, and there is no container-level section table. So an image cannot be a new top-level
//! thing: it has to be *in* something, and the only thing that traverses chunks 1..N is the document
//! payload. Hence a catalog appended to the payload's tail, which is why this module is in
//! `holonomy-text` -- the payload is the text engine's serialisation, not the container's.
//!
//! # The address is the content
//!
//! [`AssetId`] is `BLAKE2b-256` of the PNG's **encoded** bytes. That choice has three consequences,
//! all of them wanted:
//!
//! * Inserting the same image twice produces the same id, so the catalog deduplicates for free and
//!   two references to one picture cost one blob.
//! * The id is verifiable. Anything holding an `AssetId` can recompute it over the bytes and learn
//!   whether it has the asset it thinks it has, with no side table to trust.
//! * The encoded bytes, not the decoded pixels, are hashed. Decoded pixels depend on the decoder and
//!   its version, so hashing them would make an id mean "these pixels under this decoder" rather
//!   than "this file", and an id stored in a document would stop meaning anything to a later build.
//!
//! Hashing the encoded bytes also means the catalog can be written **without decoding anything**,
//! which is the property that keeps this crate free of a decoder dependency. See
//! [`Asset::dimensions`]: the only thing read out of the PNG is its `IHDR`, at a fixed offset.
//!
//! # No floats, no curves
//!
//! Zero-Bézier Invariant (§2.2) applies with full force here: the dimensions are integers taken from
//! the file, and nothing in this module evaluates anything. The scaler that turns these bytes into
//! page-column-width pixels lives in `holonomy-image`, and it is integer fixed point for the same
//! reason.

use core::fmt;
use zeroize::Zeroizing;

/// The codepoint that anchors an image in the text.
///
/// **This is how an image's position is stored.** Not a byte offset in some parallel structure, and
/// not a line number: a single character in the document's own bytes. Three things follow, and all
/// three are the reason it is done this way:
///
/// 1. **Edits move it for free.** The rope, the span map, the table map, undo and redo all already
///    track byte offsets through every edit. A new offset-shaped structure would need touching at
///    each of those five sites; a character in the text needs nothing, because it *is* the text.
/// 2. **It survives save and load without a new payload section.** The frozen payload layout has
///    room for the catalog and for tables and math, and the catalog's per-entry shape is fixed at
///    `Blake2b 32B | w u16 | h u16 | len | PNG`. There is nowhere in that shape to put an anchor,
///    and the layout is frozen, so an anchor cannot go there.
/// 3. **It is how every other word processor does it**, which is not an argument on its own but is
///    evidence that the approach ages well.
///
/// U+FFFC OBJECT REPLACEMENT CHARACTER is the Unicode codepoint reserved for exactly this. It has no
/// glyph in any packed face, so `Painter` must intercept it before the atlas lookup -- the same
/// arrangement as [`holonomy_assets::box_drawing`], whose runes are procedural for the same reason.
/// An uncaught U+FFFC would land in `PaintStats::missing`.
///
/// ## Which asset goes where
///
/// The *N*-th U+FFFC in document order is served by the *N*-th catalog entry. So the catalog is an
/// ordered list, and the pairing needs no field anywhere: both sides are positional, and both are
/// covered by the payload's AEAD tag, so they cannot drift apart on disk without the tag failing.
///
/// The cost is honest and worth stating: images cannot be reordered without rewriting the text, and a
/// document with an image whose asset was removed loses that image (rendered as the missing marker)
/// rather than the text around it.
pub const ANCHOR: char = '\u{fffc}';

/// The anchor's UTF-8 encoding, which is 3 bytes: `EF BF BC`.
pub const ANCHOR_BYTES: [u8; 3] = [0xEF, 0xBF, 0xBC];

/// Bytes one [`AssetId`] occupies in the catalog.
pub const ID_LEN: usize = 32;

/// A content address: `BLAKE2b-256` over a PNG's encoded bytes.
///
/// A newtype rather than a bare `[u8; 32]` so that an `AssetId` and a pixel buffer cannot be swapped
/// by accident at a call site -- the compiler refuses, where a `[u8; 32]` would compile and corrupt.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetId([u8; ID_LEN]);

impl AssetId {
    /// The address of `png`.
    ///
    /// `BLAKE2b` at 256 bits, which is the digest `holonomy-crypto` already links for the root key
    /// (`Blake2b512`, a different output length of the same construction). Reusing it means the
    /// crate gains no new transitive dependency and no new binary cost.
    #[must_use]
    pub fn of(png: &[u8]) -> Self {
        use blake2::Digest as _;
        let mut h = blake2::Blake2b256::new();
        h.update(png);
        let mut out = [0u8; ID_LEN];
        out.copy_from_slice(&h.finalize());
        Self(out)
    }

    /// The raw digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; ID_LEN] {
        &self.0
    }

    /// Build from a digest that was stored rather than computed.
    ///
    /// Named `from_stored` rather than `from_bytes` on purpose: this is the load path, and the only
    /// thing that makes a stored digest trustworthy is [`AssetCatalog::decode`] checking it against
    /// the bytes that came with it. There is no constructor from raw bytes on the encode side, so
    /// the two cannot be confused.
    #[must_use]
    pub const fn from_stored(bytes: [u8; ID_LEN]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for AssetId {
    /// The first 8 hex digits, which is enough to tell two assets apart in a log line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AssetId({:02x}{:02x}{:02x}{:02x}…)",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

impl fmt::Display for AssetId {
    /// All 64 hex digits. `Display` is the whole-value form because a truncated id that gets copied
    /// out of a log line is worse than a long one; [`Debug`](Self::fmt) is the short form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// One image in the catalog.
pub struct Asset {
    /// Content address of `png`.
    pub id: AssetId,
    /// Width in pixels, from the PNG's `IHDR`.
    pub width: u16,
    /// Height in pixels, from the PNG's `IHDR`.
    pub height: u16,
    /// The PNG file exactly as it was given, still encoded.
    ///
    /// `Zeroizing`, because this is plaintext image data sitting in the heap and the container's
    /// whole claim is that plaintext is not left lying around. It is zeroed on drop -- including on
    /// the error paths of [`AssetCatalog::decode`], which is why `Zeroizing` is used rather than a
    /// `Vec` with a manual `clear` on the happy path and a comment on the others.
    pub png: Zeroizing<Vec<u8>>,
}

impl Asset {
    /// Bytes of PNG for `png`, with its dimensions read from `IHDR`.
    ///
    /// # Why the dimensions are read here rather than trusted from the caller
    ///
    /// `width`/`height` go into the catalog header and are later used to size a `SecureBlock` and
    /// to compute an image's height in the line layout. A caller-supplied number in either place is
    /// a way to make the layout disagree with the pixels, and the disagreement would be invisible:
    /// the image would draw, just at the wrong size, or not draw at all. So they are read from the
    /// file, and the file is refused if its `IHDR` is not where the format says it must be.
    ///
    /// Only the `IHDR` is parsed -- no inflate, no filters, no interlace handling -- because the
    /// full decoder lives in `holonomy-image` and pulling it in here would make the *text* crate
    /// depend on `miniz_oxide` for four integers. The width/height limits are the format's own:
    /// PNG is a u32 in `IHDR`, and a document's page column is 640 px (§2.9.3), so nothing above
    /// `u16::MAX` can be displayed anyway and is refused here rather than silently truncated.
    pub fn new(png: &[u8]) -> Result<Self, AssetError> {
        let (width, height) = ihdr_dimensions(png)?;
        Ok(Self {
            id: AssetId::of(png),
            width,
            height,
            png: Zeroizing::new(png.to_vec()),
        })
    }

    /// Encoded size in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.png.len()
    }

    /// Whether this asset holds no bytes, which construction refuses.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.png.is_empty()
    }
}

impl fmt::Debug for Asset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Deliberately not derived: `png` is up to 8 MiB of bytes and a `{:?}` on it would flood
        // whatever log this went to.
        f.debug_struct("Asset")
            .field("id", &self.id)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("png_len", &self.png.len())
            .finish()
    }
}

/// Read `(width, height)` out of a PNG's `IHDR`.
///
/// The layout is fixed by the format: an 8-byte signature, then `IHDR` as a 4-byte big-endian
/// length, the 4-byte type, and 8 bytes of width and height big-endian. This reads exactly those
/// bytes and refuses anything else, which includes refusing a file that is not a PNG at all.
fn ihdr_dimensions(png: &[u8]) -> Result<(u16, u16), AssetError> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if png.len() < 24 {
        return Err(AssetError::Truncated {
            have: png.len(),
            need: 24,
        });
    }
    if png[..8] != SIGNATURE {
        return Err(AssetError::NotPng);
    }
    if &png[12..16] != b"IHDR" {
        return Err(AssetError::NotPng);
    }
    let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    if width == 0 || height == 0 {
        return Err(AssetError::ZeroDimension { width, height });
    }
    if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
        return Err(AssetError::DimensionTooLarge { width, height });
    }
    Ok((width as u16, height as u16))
}

/// Why an asset was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetError {
    /// Fewer than 24 bytes, so there is not even an `IHDR`.
    Truncated {
        /// Bytes offered.
        have: usize,
        /// Bytes needed.
        need: usize,
    },
    /// The signature is wrong or the first chunk is not `IHDR`.
    NotPng,
    /// A dimension is zero.
    ZeroDimension {
        /// Width as declared.
        width: u32,
        /// Height as declared.
        height: u32,
    },
    /// A dimension exceeds `u16::MAX`, which is the catalog's field width.
    DimensionTooLarge {
        /// Width as declared.
        width: u32,
        /// Height as declared.
        height: u32,
    },
    /// A catalog index is past the end of the catalog.
    NoSuchIndex {
        /// The index asked for.
        index: usize,
        /// Entries present.
        len: usize,
    },
    /// The catalog is truncated: a declared length runs past the end of the buffer.
    TruncatedCatalog {
        /// Bytes the catalog claimed to need.
        need: usize,
        /// Bytes actually available.
        have: usize,
    },
    /// A stored id does not match the bytes stored with it.
    ///
    /// This is the whole point of a content address, and it is checked on load rather than trusted:
    /// the payload is AEAD-protected, so this cannot fire from an attacker who has the key. It fires
    /// from a *bug* -- a writer that hashed the wrong buffer, or a spliced payload -- and it fires at
    /// the boundary where a wrong id would otherwise become a wrong image forever after.
    IdMismatch {
        /// The id the catalog header declared.
        stored: AssetId,
        /// The id the bytes actually hash to.
        computed: AssetId,
    },
    /// The catalog's declared entry count cannot fit in the bytes that are left.
    CountTooLarge {
        /// Entries declared.
        count: u32,
        /// Bytes available for them.
        available: usize,
    },
    /// The header's dimensions disagree with the PNG's own `IHDR`.
    ///
    /// Separate from [`AssetError::IdMismatch`] because it is a different failure with a different
    /// cause and a different fix: the id matched, so the bytes are the bytes the id names, and the
    /// *header* is the thing that is wrong. Reporting it as an id mismatch would send whoever is
    /// debugging this to re-hash bytes that are provably correct.
    DimensionMismatch {
        /// Which entry of the catalog, by index.
        index: usize,
        /// Dimensions the header declared, as `(width, height)`.
        declared: (u16, u16),
        /// Dimensions the PNG's `IHDR` holds.
        actual: (u16, u16),
    },
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { have, need } => {
                write!(f, "only {have} bytes: a PNG header needs {need}")
            }
            Self::NotPng => write!(f, "not a PNG: the signature or the IHDR type is wrong"),
            Self::ZeroDimension { width, height } => {
                write!(f, "a PNG of {width}x{height} has no pixels")
            }
            Self::DimensionTooLarge { width, height } => write!(
                f,
                "a PNG of {width}x{height} does not fit the catalog's u16 dimensions"
            ),
            Self::NoSuchIndex { index, len } => {
                write!(f, "catalog entry {index} requested of {len}")
            }
            Self::TruncatedCatalog { need, have } => {
                write!(f, "the catalog needs {need} bytes and only {have} are left")
            }
            Self::IdMismatch { stored, computed } => write!(
                f,
                "the catalog header says {stored} but these bytes are {computed}"
            ),
            Self::CountTooLarge { count, available } => write!(
                f,
                "{count} assets cannot fit in the {available} bytes left in the payload"
            ),
            Self::DimensionMismatch {
                index,
                declared,
                actual,
            } => write!(
                f,
                "catalog entry {index} declares {declared:?} but its PNG is {actual:?}"
            ),
        }
    }
}

impl std::error::Error for AssetError {}

/// Bytes a catalog of `count` entries needs before any of their data.
///
/// 4 for the count, then `40 + len` per entry: 32 for the id, 2 + 2 for the dimensions, 4 for the
/// length. Used by the decoder to refuse a count it cannot possibly satisfy before allocating.
pub const fn catalog_fixed_bytes(count: u32) -> usize {
    4 + count as usize * (ID_LEN + 2 + 2 + 4)
}

/// The document's images, one per anchor, in document order.
///
/// # Ordering is the contract, and it needs maintaining
///
/// Entry `i` serves the *i*-th [`ANCHOR`] in document order. **That pairing is positional, so it has
/// to be repaired after every edit, and the first version of this file did not repair it.** It claimed
/// -- in a doc comment, at length, and wrongly -- that a catalog needed none of the five
/// `apply_insert`/`apply_delete` sites `TableMap` needs, on the grounds that a content address is not
/// an interval. A content address is not; the *ordinal* is.
///
/// Undoing an insertion of an image deletes its anchor and nothing else, so the document was left with
/// one anchor and two assets: anchor 0 still pointed at the first entry, and the second entry was
/// unreachable. That is invisible whenever the two images are the same bytes -- which is what the
/// test fixture is -- and a **wrong picture** the moment they are not. `tests/asset_payload.rs` cannot
/// catch it either: both the text and the catalog are individually valid and mutually inconsistent, so
/// the AEAD tag is valid and the payload decodes.
///
/// So the catalog is maintained the way [`TableMap`](crate::TableMap) is: [`insert_at`](Self::insert_at)
/// puts an asset at a chosen ordinal, [`remove_range`](Self::remove_range) drops a run of them and
/// hands the removed assets back so undo can restore them, and [`Editor`](crate::Editor) calls both at
/// the same five sites its tables are called from.
///
/// # Why the alternative was rejected
///
/// The obvious fix is to put the address *in the text* next to the anchor -- U+FFFC followed by 64 hex
/// digits -- which makes the pairing content-addressed and needs no maintenance at all. It costs 64
/// bytes of document per image, which for a document with a thousand images is 64 KB of pure overhead
/// inside an 8 MiB payload, and it makes the document's text carry a serialised pointer to a structure
/// that lives in the same document. Ordinals cost nothing and are what the frozen layout implies.
#[derive(Default)]
pub struct AssetCatalog {
    entries: Vec<Asset>,
}

impl AssetCatalog {
    /// An empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The assets, in document order.
    #[must_use]
    pub fn entries(&self) -> &[Asset] {
        &self.entries
    }

    /// How many assets the catalog holds. This is also how many anchors the document should have.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the catalog is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Put `png` at ordinal `at`, making it the asset for the *`at`-th anchor.
    ///
    /// **Not** `push`. A caller inserting an image in the middle of a document has an anchor at some
    /// ordinal `k`, and appending the asset would serve it the *last* picture in the document -- see
    /// this type's ordering contract for how that went wrong once.
    ///
    /// `at` beyond the end is the same as the end, which is what makes "insert at the caret, which is
    /// past every anchor" work without the caller having to measure. Out-of-order inserts are
    /// impossible: `at > len` is clamped to `len`, so a caller's arithmetic mistake shortens the
    /// document's image list rather than leaving a hole nothing serves.
    ///
    /// A duplicate is **not** deduplicated, and the reason is the ordering: entry `i` serves anchor
    /// `i`, so collapsing two entries into one would leave the second anchor pointing at the first
    /// anchor's slot. The *addresses* still collide, which is what lets a caller ask "do I already have
    /// this picture" without a side table.
    pub fn insert_at(&mut self, at: usize, png: &[u8]) -> Result<AssetId, AssetError> {
        let asset = Asset::new(png)?;
        let id = asset.id;
        let at = at.min(self.entries.len());
        self.entries.insert(at, asset);
        Ok(id)
    }

    /// Drop `count` assets starting at ordinal `first`, and hand them back.
    ///
    /// Returning the removed assets is what makes undo of a deletion possible. [`TableMap`] solves the
    /// same problem with a whole-map shadow (`Editor::undo_tables`), which works for tables because a
    /// table is 28 bytes; an asset is a PNG, so shadowing the map would mean holding every image twice.
    /// Handing back exactly what was removed costs one `Vec` per delete that removed an anchor, and
    /// `Vec::new()` does not allocate for the overwhelmingly common delete that removed none.
    pub fn remove_range(&mut self, first: usize, count: usize) -> Vec<Asset> {
        if count == 0 || first >= self.entries.len() {
            return Vec::new();
        }
        let last = first.saturating_add(count).min(self.entries.len());
        self.entries.drain(first..last).collect()
    }

    /// Put the first `take` of `assets` back at ordinal `at`, and return whatever did not fit.
    ///
    /// The inverse of [`remove_range`](Self::remove_range), used by undo and by redo.
    ///
    /// Three things about the signature, each forced by something:
    ///
    /// * **`Vec<Asset>` by value, and `Asset` is not `Clone`.** An `Asset` owns a PNG in a
    ///   `Zeroizing<Vec<u8>>`, so a `Clone` derive would be a way to duplicate plaintext image bytes
    ///   without the copy being scrubbed -- and it would be used, because "just clone the assets back"
    ///   is the obvious thing to write at a restore site. Restoring by moving is both cheaper and the
    ///   only version whose scrub story is checkable.
    /// * **`take`,** because the stash is a `Vec` and the range may hold fewer anchors than the stash has
    ///   assets. Taking all of it would install assets no anchor serves.
    /// * **Returns the remainder**, so a caller that cannot explain the difference can keep it rather
    ///   than drop it. Dropping a PNG is legal; dropping one *because a count was wrong* is how the
    ///   wrong-picture bug starts.
    ///
    /// The installed assets are **inserted** at `at`, shifting everything from `at` on up by `take` --
    /// and that shift is the point, not an accident.
    ///
    /// An earlier version of this replaced the range instead, on the reasoning that "assets at or after
    /// the restored range belong to later anchors and must stay where they are". That reasoning is
    /// wrong, and the arithmetic shows it. Deleting anchor 0 of `[a0, a1]` leaves `[a1]` at ordinal 0,
    /// correctly serving the anchor that is now first. Undoing must get back to `[a0, a1]`, so `A0`
    /// goes **in front of** `A1` and `A1` moves back up to ordinal 1. Replacing would have destroyed
    /// `A1`:
    ///
    /// ```text
    /// delete anchor 0:  catalog [A0, A1] -> [A1]      (remove_range(0, 1))
    /// undo:             catalog [A1]     -> [A0, A1]   (insert_at(0, A0))
    /// ```
    pub fn restore_at(&mut self, at: usize, assets: Vec<Asset>, take: usize) -> Vec<Asset> {
        if take == 0 || assets.is_empty() {
            return assets;
        }
        let at = at.min(self.entries.len());
        let take = take.min(assets.len());
        let mut rest = assets;
        // `drain(..take)` hands back the taken prefix and leaves the remainder in place; nothing is
        // copied and nothing is cloned.
        let taken: Vec<Asset> = rest.drain(..take).collect();
        for (i, a) in taken.into_iter().enumerate() {
            self.entries.insert(at + i, a);
        }
        rest
    }

    /// How many anchors there are at or before byte `offset` of `text`.
    ///
    /// The ordinal an [`Editor::insert_image`](crate::Editor::insert_image) at `offset` will create,
    /// and the ordinal [`remove_anchors_in`](Self::remove_anchors_in) needs to know about. Counting
    /// bytes rather than characters is right because the catalog pairs with *byte* offsets, which is
    /// what every other offset in the editor is.
    #[must_use]
    pub fn ordinal_at(text: &[u8], offset: usize) -> usize {
        let at = offset.min(text.len());
        scan_anchors(&text[..at]).len()
    }

    /// How many anchors are in `bytes`.
    ///
    /// The half of a delete's work that needs no document at all: the bytes being deleted are already
    /// in the editor's `delete_scratch`, so counting the anchors among them is arithmetic over a buffer
    /// that exists.
    #[must_use]
    pub fn count_in(bytes: &[u8]) -> usize {
        scan_anchors(bytes).len()
    }

    /// Remove `count` assets starting at ordinal `first`.
    ///
    /// The two halves a delete needs, separated so neither forces the other to materialise the
    /// document: `first` comes from the caller's own anchor list, `count` from the deleted bytes. The
    /// first version took a `text: &[u8]` and the range and did both from the whole document, which
    /// meant a full-document `Vec` **per delete** -- 1,000 allocations over a 1,000-keystroke burst,
    /// which `no_alloc.rs` reported immediately. The keystroke path has to stay allocation-free whatever
    /// the document contains.
    pub fn remove_n(&mut self, first: usize, count: usize) -> Vec<Asset> {
        self.remove_range(first, count)
    }

    /// The anchors inside byte range `offset..offset + len` of `text`, as their ordinals.
    ///
    /// What a *re-insert* needs: the assets go back at these ordinals. Kept separate from
    /// [`remove_anchors_in`](Self::remove_anchors_in) because undo needs the count and redo needs the
    /// positions, and a function returning both would have every caller destructure away half of it.
    #[must_use]
    pub fn anchors_in(text: &[u8], offset: u32, len: u32) -> (usize, usize) {
        let lo = (offset as usize).min(text.len());
        let hi = lo.saturating_add(len as usize).min(text.len());
        (Self::ordinal_at(text, lo), Self::ordinal_at(text, hi))
    }

    /// Asset `index`, by position.
    pub fn get(&self, index: usize) -> Result<&Asset, AssetError> {
        self.entries.get(index).ok_or(AssetError::NoSuchIndex {
            index,
            len: self.entries.len(),
        })
    }

    /// Total encoded bytes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        4 + self
            .entries
            .iter()
            .map(|a| ID_LEN + 2 + 2 + 4 + a.len())
            .sum::<usize>()
    }

    /// Append the catalog to `out`.
    ///
    /// The layout is the frozen one, field for field:
    ///
    /// ```text
    /// [count u32 LE][Asset: Blake2b 32B | w u16 | h u16 | len u32 | PNG]*
    /// ```
    ///
    /// Every width is little-endian to match `MasterFrame::encode`, the only other hand-rolled
    /// encoder in this codebase, and every length is explicit rather than derived -- which is what
    /// lets the decoder walk entries without trusting the id.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(
            &u32::try_from(self.entries.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        for a in &self.entries {
            out.extend_from_slice(a.id.as_bytes());
            out.extend_from_slice(&a.width.to_le_bytes());
            out.extend_from_slice(&a.height.to_le_bytes());
            // `to_le_bytes` on a `usize` is 8 bytes on this target and the field is 4, so the
            // conversion is explicit and saturating: an asset larger than 4 GiB cannot exist inside
            // the container's 8 MiB payload cap, and truncating the field would desynchronise every
            // entry after it.
            out.extend_from_slice(&u32::try_from(a.len()).unwrap_or(u32::MAX).to_le_bytes());
            out.extend_from_slice(&a.png);
        }
    }

    /// Read a catalog from the whole of `input`.
    ///
    /// The catalog is the payload's **last** section, so it reads to end of input. That is what lets
    /// the payload carry no catalog offset: there is nothing after it to be confused with.
    ///
    /// `input` must be exactly the catalog -- the caller slices it off. Every length in it is
    /// checked against what is actually there, and every stored id is recomputed from the bytes
    /// beside it. A catalog that does not decode is refused whole; there is no partial result,
    /// because half a catalog means half the images are wrong and the failure would surface as a
    /// missing picture three screens away.
    pub fn decode(input: &[u8]) -> Result<Self, AssetError> {
        if input.len() < 4 {
            return Err(AssetError::TruncatedCatalog {
                need: 4,
                have: input.len(),
            });
        }
        let count = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
        let mut cursor = 4usize;
        // Refuse an impossible count before allocating for it. `catalog_fixed_bytes` is the size of
        // a catalog of empty assets, so anything below that is unsatisfiable by construction.
        let fixed = catalog_fixed_bytes(count);
        if fixed > input.len() {
            return Err(AssetError::CountTooLarge {
                count,
                available: input.len(),
            });
        }
        let mut entries = Vec::with_capacity(count as usize);
        for index in 0..count as usize {
            let header_end = cursor + ID_LEN + 2 + 2 + 4;
            let Some(header) = input.get(cursor..header_end) else {
                return Err(AssetError::TruncatedCatalog {
                    need: header_end,
                    have: input.len(),
                });
            };
            let mut id = [0u8; ID_LEN];
            id.copy_from_slice(&header[..ID_LEN]);
            let width = u16::from_le_bytes([header[ID_LEN], header[ID_LEN + 1]]);
            let height = u16::from_le_bytes([header[ID_LEN + 2], header[ID_LEN + 3]]);
            let len_at = ID_LEN + 4;
            let len = u32::from_le_bytes([
                header[len_at],
                header[len_at + 1],
                header[len_at + 2],
                header[len_at + 3],
            ]) as usize;
            cursor = header_end;
            let Some(png) = input.get(cursor..cursor.saturating_add(len)) else {
                return Err(AssetError::TruncatedCatalog {
                    need: cursor.saturating_add(len),
                    have: input.len(),
                });
            };
            cursor += len;
            let stored = AssetId::from_stored(id);
            let computed = AssetId::of(png);
            if stored != computed {
                return Err(AssetError::IdMismatch { stored, computed });
            }
            // The dimensions in the header are re-derived from the bytes rather than taken on
            // trust. They are stored so a reader can size a `SecureBlock` without a second walk, and
            // they are checked because a wrong one is a layout bug that would otherwise only show up
            // as an image drawn at a size its pixels do not match -- and as a cache entry whose
            // `SizeMismatch` blames the decoder for a number the encoder wrote.
            let actual = ihdr_dimensions(png)?;
            if actual != (width, height) {
                return Err(AssetError::DimensionMismatch {
                    index,
                    declared: (width, height),
                    actual,
                });
            }
            entries.push(Asset {
                id: stored,
                width,
                height,
                png: Zeroizing::new(png.to_vec()),
            });
        }
        Ok(Self { entries })
    }
}

impl fmt::Debug for AssetCatalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssetCatalog")
            .field("len", &self.entries.len())
            .field("encoded_len", &self.encoded_len())
            .finish()
    }
}

/// How many [`ANCHOR`] characters the text holds, and where they are.
///
/// Both halves in one pass because every caller needs both or neither: a caller that wants "the
/// anchors on this page" needs the offsets, and a caller that wants "is this offset an image" needs
/// the count.
#[must_use]
pub fn scan_anchors(text: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + ANCHOR_BYTES.len() <= text.len() {
        if text[i..i + 3] == ANCHOR_BYTES {
            out.push(u32::try_from(i).unwrap_or(u32::MAX));
            i += ANCHOR_BYTES.len();
        } else {
            i += 1;
        }
    }
    out
}
