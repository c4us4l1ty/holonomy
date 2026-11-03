//! The Iceberg cache: decoded rasters, and only the ones the viewport can show.
//!
//! # Why decoded rasters are page-column-width, not native
//!
//! PROJECT.md §2.9.3 measured this and it dictates the design. A 1920x1080 RGBA image is 8.29 MiB,
//! which is **exactly one** image against an 8.0 MiB decoded budget; a document with two photos on
//! facing pages breaches the budget at the instant the second decodes, which is the OOM the
//! requirement is trying to prevent. At 640x360 -- the page column width of §5's chrome -- one
//! raster is 0.88 MiB and **nine** fit.
//!
//! So the container holds the original encrypted bytes and the cache holds only what the viewport can
//! show, downscaled at decode time. Three things follow, and all three are intended:
//!
//! * the ±1-page policy holds ~9 images, so it is a policy rather than a formality;
//! * the scaler runs on *every* image, because a 1080p source is always downscaled -- the only
//!   honest way to test a scaler;
//! * a native-resolution decode never happens, so the 8.3 MiB buffer it would need never exists.
//!
//! # Every raster is a `SecureBlock`, and unlocked
//!
//! Each entry's pixels live in a [`SecureBlock`] under [`LockPolicy::Unlocked`]. That policy is the
//! whole reason this type can honour §2.9.3's 8.0 MiB on this host: `RLIMIT_MEMLOCK` is 8 MiB and
//! document text already needs 6.82 MiB of it, so a page-locked raster would fit **one** alongside a
//! full document. See [`LockPolicy`] for the full trade -- rasters keep their guard pages,
//! `MADV_DONTDUMP` and `MADV_DONTFORK` and registry membership; only the swap guarantee is given up,
//! and a raster is derived from the encrypted container so swap disclosure costs a re-decodable
//! cache entry rather than the document.
//!
//! # Eviction is synchronous and observable, never `Drop`
//!
//! [`IcebergCache::evict`] calls [`SecureBlock::zeroize_and_release`] and *then* drops the entry, so
//! the pixels are zero before the memory is released and before the next frame is painted. Relying on
//! `Drop` would be correct for confidentiality but useless for the gate: RSS would fall whenever the
//! allocator got round to it, and the requirement is that it falls **before the next frame** and
//! **observably**, so the gate can assert it.
//!
//! # The ±1-page window, and what "page" means here
//!
//! [`IcebergCache::set_window`] takes the set of page indices whose images may stay resident --
//! typically `first_page - 1 ..= last_page + 1`. Entries outside it are evicted, *synchronously*.
//! The cache does not itself know the viewport's page layout; the session decides which pages are
//! near and says so. That keeps the eviction *rule* (which is what the gate tests) separate from the
//! page geometry (which is the session's).

use holonomy_secure::{LockPolicy, SecureBlock};

/// Default decoded-image budget, §2.9.3: 8.0 MiB.
///
/// 8,388,608 bytes. At the page-column width of §5's chrome (640 px) this is nine 640x360 RGBA
/// rasters with 0.9 MiB to spare, which is what makes the ±1-page policy hold. A document whose
/// images are *narrower* than the column can hold proportionally more, so this is a ceiling on
/// pixels-in-RAM rather than a count of images.
pub const DEFAULT_BUDGET: usize = 8 * 1024 * 1024;

/// One decoded raster in the cache.
#[derive(Debug)]
pub struct Entry {
    /// The pixels, `width * height * 4` bytes, in a scrub-on-drop [`SecureBlock`].
    ///
    /// Unlocked by policy; see the module docs.
    pub block: SecureBlock,
    /// Raster width in pixels.
    pub width: u32,
    /// Raster height in pixels.
    pub height: u32,
    /// Which page this raster belongs to, for the ±1 rule.
    pub page: u32,
    /// Which image on that page, so a page with several images addresses them separately.
    pub index: u32,
    /// The asset's content address: `BLAKE2b-256` over the asset's PNG bytes.
    ///
    /// **A raw `[u8; 32]`, not `holonomy_text::AssetId`, and that is deliberate.** A render tree says
    /// "draw this asset" by content address; the cache says "I have a raster". The two are joined by
    /// [`get_by_id`](IcebergCache::get_by_id), which is a linear scan over ≤ ~9 entries and is free
    /// next to the blit it precedes. If this field were an `AssetId`, `holonomy-image` would depend on
    /// `holonomy-text` and pull the whole gap-rope in behind it, so that §2.9.1's 60 KiB decoder budget
    /// would be measured against a crate that also carries a text engine. The digest is a
    /// `[u8; 32]` and that is all either side needs of it.
    pub id: [u8; 32],
    /// Bytes of *source* (native) image that produced this raster, for `PaintStats`.
    ///
    /// The scaler's work is proportional to this, not to the raster, so it is what a "resampled"
    /// counter should be weighed against.
    pub source_pixels: u64,
}

impl Entry {
    /// The raster's pixels as a slice.
    pub fn pixels(&self) -> &[u8] {
        self.block.as_slice()
    }

    /// Bytes this entry costs against the cache budget: exactly its `SecureBlock` length.
    ///
    /// `block.len()`, not `width * height * 4`. They are equal today because the entry is allocated
    /// at exactly the raster size, but the *accounting* should follow the allocation that will
    /// actually be freed, and if the two ever diverge the budget should follow the allocation.
    pub fn bytes(&self) -> usize {
        self.block.len()
    }
}

/// Why a raster could not be admitted.
///
/// `Clone` but not `Copy`: [`CacheError::Allocation`] carries the underlying
/// [`SecureBlockError`](holonomy_secure::SecureBlockError)'s message as a `String` because the two
/// possible causes -- `MmapFailed` and `RegistryFull` -- have one response between them, and matching
/// on the variant to render it would be a second place to keep in step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    /// Admitting this raster would exceed the budget, and the caller did not ask for eviction to
    /// make room.
    ///
    /// Separate from eviction *not happening*: eviction is normal and silent, this is a refusal.
    Budget {
        /// Bytes resident before the request.
        have: usize,
        /// Bytes the raster needs.
        want: usize,
        /// The budget.
        cap: usize,
    },
    /// The pixel buffer's length is not `width * height * 4`.
    ///
    /// Its own variant rather than folded into [`CacheError::Budget`]: a size *mismatch* is a caller
    /// bug, while a budget refusal is a policy decision, and lumping them would make a bug report as
    /// "out of memory".
    SizeMismatch {
        /// Bytes `width * height * 4`.
        want: usize,
        /// Bytes the buffer actually holds.
        have: usize,
    },
    /// A `SecureBlock` could not be allocated -- `mmap` or the guard registry failed.
    ///
    /// Carries the underlying error's `Display` rather than the type, because the two causes
    /// (`MmapFailed`, `RegistryFull`) are both "this host cannot give us that mapping" and the
    /// caller has one response to either.
    Allocation(String),
    /// The caller asked for a zero-area raster.
    ZeroSized,
}

impl Default for IcebergCache {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Display for CacheError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Budget { have, want, cap } => write!(
                f,
                "raster needs {want} B with {have} B resident against a {cap} B budget"
            ),
            Self::Allocation(why) => write!(f, "could not allocate a SecureBlock: {why}"),
            Self::ZeroSized => f.write_str("a raster must have a non-zero width and height"),
            Self::SizeMismatch { want, have } => {
                write!(f, "buffer is {have} B, the dimensions need {want} B")
            }
        }
    }
}

impl std::error::Error for CacheError {}

/// The decoded-image cache.
///
/// Holds page-column-width rasters for the pages near the viewport and nothing else. Not `Sync` and
/// not `Clone` on purpose: it owns `SecureBlock`s whose scrub-on-drop is the confidentiality
/// guarantee, so duplicating or sharing one would let a raster outlive the accounting the gate reads.
pub struct IcebergCache {
    /// Live entries, in insertion order. A `Vec`, not a `HashMap`: the key space is small (≤ ~9), a
    /// linear scan over it is nothing next to a decode, and insertion order is what makes eviction
    /// deterministic for the gate.
    entries: Vec<Entry>,
    /// Total bytes resident, as the sum of `Entry::bytes`.
    resident: usize,
    /// The ceiling on `resident`.
    budget: usize,
}

impl IcebergCache {
    /// A cache with the default 8.0 MiB budget.
    pub fn new() -> Self {
        Self::with_budget(DEFAULT_BUDGET)
    }

    /// A cache with an explicit byte budget.
    pub fn with_budget(budget: usize) -> Self {
        Self {
            entries: Vec::new(),
            resident: 0,
            budget,
        }
    }

    /// Total bytes resident. The number the gate asserts stays at or below [`DEFAULT_BUDGET`].
    pub fn resident_bytes(&self) -> usize {
        self.resident
    }

    /// The configured budget.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// How many rasters are live.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no raster is live.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry for `(page, index)`, if resident.
    ///
    /// `index` distinguishes several images on one page; a page with no images has no entries at all
    /// rather than an empty one, so "does this page have images" and "are they here" are different
    /// questions.
    pub fn get(&self, page: u32, index: u32) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.page == page && e.index == index)
    }

    /// The entry for the asset `id`, if resident.
    ///
    /// The lookup a painter makes for [`Node::Image`](holonomy_render::Node::Image)'s content address.
    /// A `Vec` scan rather than a `HashMap` because the key space is the number of rasters that fit in
    /// the budget -- nine at §2.9.3's numbers -- and a hash table would cost more in code and in
    /// binary than nine `u32` comparisons cost per image per frame.
    ///
    /// # Why the key is the address and not `(page, index)`
    ///
    /// Both address the same raster. The address is what the render tree carries, because a node must
    /// be able to say *which picture* without knowing where it landed -- and the same asset pasted
    /// twice is two anchors and one address, so an address-keyed lookup is what makes both anchors
    /// find the same pixels instead of the second showing a blank.
    pub fn get_by_id(&self, id: &[u8; 32]) -> Option<&Entry> {
        self.entries.iter().find(|e| &e.id == id)
    }

    /// Whether a raster for `id` is resident, without borrowing it.
    pub fn holds(&self, id: &[u8; 32]) -> bool {
        self.get_by_id(id).is_some()
    }

    /// Admit a raster for `(page, index)`, evicting whatever the ±1 rule allows first.
    ///
    /// Returns the entry's byte length on success. Eviction is *not* implicit here: the caller runs
    /// [`set_window`] first, so "which pages are near" is decided once per frame in one place rather
    /// than being re-derived inside the insert path.
    ///
    /// # Why `LockPolicy::Unlocked`
    ///
    /// The pixels go into an unlocked `SecureBlock`. Guards, `MADV_DONTDUMP` and `MADV_DONTFORK` and
    /// registry membership all still apply; the swap guarantee is traded away because
    /// `RLIMIT_MEMLOCK` is spent by the document's text and a locked raster would fit only one
    /// beside a full document, defeating the policy this cache exists to implement. The raster is
    /// derived from the encrypted container, so the loss is re-decodable.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        id: [u8; 32],
        page: u32,
        index: u32,
        width: u32,
        height: u32,
        source_pixels: u64,
        decoded: &[u8],
    ) -> Result<usize, CacheError> {
        if width == 0 || height == 0 {
            return Err(CacheError::ZeroSized);
        }
        // A raster already admitted for this address is *the same pixels*: the address is over the
        // encoded PNG, so there is nothing to recompute and nothing to add. Re-admitting would evict a
        // neighbour to make room for a byte-identical copy, and `set_window` on the next scroll would
        // then scrub it. Scrolling back over an image inside the ±1 window is the common case, so this
        // is the difference between a scroll that decodes and one that does not.
        if let Some(existing) = self.get_by_id(&id) {
            return Ok(existing.bytes());
        }
        let want = decoded.len();
        // A raster whose pixel buffer is not `width * height * 4` would make the painter's
        // `width`-indexed reads run off the end of the block, so it is refused here rather than
        // discovered as a guard-page fault during a paint.
        let expect = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if want != expect {
            return Err(CacheError::SizeMismatch {
                want: expect,
                have: want,
            });
        }

        // Evict until the new raster fits, scrubbing each victim as it goes.
        while self.resident + want > self.budget && !self.entries.is_empty() {
            // Evict the oldest: insertion order is the array order, so index 0 is the oldest. Not
            // LRU -- for a ±1-page window over a linear scroll, oldest-inserted is the one furthest
            // behind, and an LRU would need per-frame touch bookkeeping to be *correct* rather than
            // merely more elaborate.
            let mut victim = self.entries.remove(0);
            self.resident -= victim.bytes();
            // Synchronous, observable scrub. See the module docs on why this is not `Drop`.
            victim.block.zeroize_and_release();
        }
        if self.resident + want > self.budget {
            // Even an empty cache cannot hold it: the budget is smaller than one raster. Report
            // rather than admit and overflow -- the gate asserts the budget is never exceeded, and
            // "exceeded because the caller asked for something too big" is still exceeded.
            return Err(CacheError::Budget {
                have: self.resident,
                want,
                cap: self.budget,
            });
        }

        let mut block = SecureBlock::allocate_with(want, LockPolicy::Unlocked)
            .map_err(|e| CacheError::Allocation(e.to_string()))?;
        block.as_mut_slice().copy_from_slice(decoded);
        self.resident += want;
        self.entries.push(Entry {
            block,
            width,
            height,
            page,
            index,
            id,
            source_pixels,
        });
        Ok(want)
    }

    /// Evict every entry not covered by `keep`, scrubbing each synchronously.
    ///
    /// `keep` is the ±1-page set: the caller passes the pages near the viewport.
    ///
    /// # Why the evicted rasters are *returned* rather than dropped
    ///
    /// §2.9.3 requires eviction to be "synchronous, and observably, so the gate can assert it". A
    /// scrub followed by an immediate `drop` satisfies the first half and destroys the second: once the
    /// `SecureBlock` is unmapped there is nothing left to read, so a gate can only measure the *absence*
    /// of pixels and infer the scrub happened. Deleting the `zeroize_and_release()` call from
    /// [`set_window`] left every test in this file passing.
    ///
    /// So the victims are handed back **already scrubbed**, and the caller releases them by dropping
    /// them. `pixels()` on a returned entry is `read_at_eviction_time`, and it is all zeroes. That
    /// makes the scrub a structural property of the return value rather than a promise in a comment:
    /// `every_evicted_raster_is_scrubbed_to_zero_before_it_is_released` reads those bytes, and
    /// `every_evicted_raster_is_scrubbed_to_zero_before_it_is_released`'s mutation -- removing the
    /// scrub call -- makes it fail.
    ///
    /// The cost is that the caller now holds a `Vec<Entry>` for the frame. That is zero entries in the
    /// common case, and dropping it is one `munmap` per raster either way.
    pub fn set_window(&mut self, keep: &[u32]) -> Evicted {
        let mut out = Evicted::default();
        let mut i = 0usize;
        while i < self.entries.len() {
            if keep.contains(&self.entries[i].page) {
                i += 1;
                continue;
            }
            let mut victim = self.entries.remove(i);
            let bytes = victim.bytes();
            // Scrub *before* the memory is released and before the next frame is painted.
            victim.block.zeroize_and_release();
            self.resident -= bytes;
            out.bytes += bytes;
            out.count += 1;
            out.rasters.push(victim);
        }
        out
    }

    /// Evict everything, scrubbing synchronously.
    pub fn clear(&mut self) -> Evicted {
        self.set_window(&[])
    }
}

/// What one eviction removed, handed back so the scrub is observable.
///
/// See [`IcebergCache::set_window`] for why the rasters come back rather than being dropped.
#[derive(Debug, Default)]
pub struct Evicted {
    /// How many rasters were evicted.
    pub count: u32,
    /// Total bytes they held, for a frame's record of the eviction.
    pub bytes: usize,
    /// The evicted rasters, **already scrubbed to zero**. Drop this to release the memory.
    pub rasters: Vec<Entry>,
}

impl Evicted {
    /// Whether nothing was evicted.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}
