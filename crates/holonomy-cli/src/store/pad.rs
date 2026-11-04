//! Where the pad comes from: a device, or the stand-in used when there is no device.
//!
//! # Why this is a trait and not a branch
//!
//! The store's first version called a free function that derived 4,096 bytes of sector material
//! from a sector index. That was labelled honestly as a stand-in, but a stand-in that is *called
//! directly* is a stand-in that ships: nothing forced the real path to exist, and nothing would have
//! failed if it never had. Making the pad a value the caller supplies means every write goes through
//! a [`PadSource`], `--device` selects the implementation that reads the stick, and the stand-in is
//! something the caller gets by not having a device — and is then told about, in the save report.
//!
//! # What a `PadSource` must guarantee
//!
//! 1. **Determinism.** `window(kappa, cursor)` returns the same bytes for the same inputs, for as
//!    long as the sectors it reads have not been ratcheted. A reader undoes what a writer folded by
//!    asking for the same window; nondeterminism here corrupts containers silently instead of
//!    failing.
//! 2. **Disjointness across cursors.** Saves `0..54` must consume mutually distinct sectors. A
//!    source that ignored the cursor would satisfy (1) and still be broken, and broken in the worst
//!    way: two containers whose folds partly cancel.
//! 3. **Nothing else.** A source is not trusted with the container, the keys, or the decision to
//!    write. It hands over [`WINDOW_BYTES`] bytes and that is all.
//!
//! # Why the pad is not keyed by the salt
//!
//! The salt lives in slot 1, which is inside the pad-covered region. A window that needed the salt
//! could only be computed by a reader who had already unfolded the container — circular, and the
//! result is a container nothing can open. The key is therefore `BLAKE2b("K_PERM" ‖ κ_USB)`: the
//! pad is a property of the stick, and the cursor selects which disjoint slice of it a save spends.

use holonomy_core::layout::{PAD_SECTORS_PER_SAVE, SECTOR_BYTES};
use holonomy_usb::io::{Access, AlignedBuf, DirectFile};
use holonomy_usb::pad::{PadPlan, PadReader, ZONE_B_LBA_START};

/// One save of pad: `32,768 × 4,096 = 134,217,728` bytes, which is also exactly one container.
pub const WINDOW_BYTES: usize = PAD_SECTORS_PER_SAVE as usize * SECTOR_BYTES;

const _: () = assert!(WINDOW_BYTES == 134_217_728);
const _: () = assert!(WINDOW_BYTES == holonomy_core::layout::CONTAINER_BYTES);

/// A pad window in the order [`crate::store::fold`] consumes it.
///
/// Position `i` is the material of the `i`-th sector the save read, *in read order*. The fold is
/// positional over the container, so a source that reordered sectors would put each 4 KiB block
/// somewhere else in the image — and the salt a reader recovers from slot 1 would not be the salt
/// the container was written under. That is the failure this type's documentation exists to prevent.
///
/// The backing store is an [`AlignedBuf`] rather than a `Vec` because that is what a direct read
/// needs, and because it wipes itself on drop: a `Vec<u8>` of pad bytes is one `realloc` away from
/// leaving a copy of the window in freed memory. This means one allocation, not two — a `Vec` would
/// have to be filled by copying out of the buffer, which is 256 MiB live at the peak against a
/// 180 MiB steady-state envelope (see `holonomy_core::budgets`).
pub struct PadWindow {
    bytes: AlignedBuf,
}

impl PadWindow {
    /// Wrap a buffer that is already one window long, in read order.
    fn of(bytes: AlignedBuf) -> Self {
        debug_assert_eq!(bytes.len(), WINDOW_BYTES, "a window is fixed size");
        Self { bytes }
    }

    /// The bytes, for [`crate::store::fold`].
    pub fn as_slice(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    /// How many bytes of pad this window carries. Always [`WINDOW_BYTES`].
    ///
    /// Named `byte_len` rather than `len` so it needs no `is_empty` partner: a pad window is never
    /// empty, and `AlignedBuf::new` refuses a zero length, so an `is_empty` that always returned
    /// `false` would be a method that exists only to silence a lint.
    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}

/// Where pad bytes come from.
///
/// The error type is `String` rather than an associated type so the trait stays object-safe: the CLI
/// chooses an implementation at run time, and a `Box<dyn PadSource>` cannot name a
/// per-implementation error. The text is what the operator sees either way.
pub trait PadSource {
    /// The window for `cursor`, keyed by `kappa_usb`.
    fn window(&mut self, kappa_usb: &[u8; 32], cursor: u64) -> Result<PadWindow, String>;

    /// Overwrite the sectors this window spent with fresh CSPRNG bytes, and report how many.
    ///
    /// After a real ratchet the same `(kappa_usb, cursor)` produces *different* bytes; that is what
    /// "fresh pad" means, and it is the only thing standing between a 54-save container and pad
    /// reuse. A source that cannot ratchet returns an error rather than a zero count, because a
    /// silent `Ok(0)` would let a caller report freshness it does not have.
    fn ratchet(&mut self, kappa_usb: &[u8; 32], cursor: u64) -> Result<u32, String>;

    /// Whether [`Self::ratchet`] can succeed, so a caller can tell the operator *before* writing.
    fn can_ratchet(&self) -> bool;

    /// A one-line description for the save report: which stick, or which stand-in.
    fn describe(&self) -> String;
}

/// The device: `holonomy-usb`'s [`PadReader`], reading Zone B of a real stick.
///
/// This is the production path. It holds the open [`DirectFile`] because a `PadReader` borrows the
/// device for the length of one save; a session keeps the handle for its lifetime and builds a
/// reader per save, which is what keeps the `O_RDONLY`-until-save rule enforceable rather than
/// aspirational.
///
/// It also holds the permutation key, derived once from `κ_USB`. Reading is 32,768 random sectors
/// per save, so deriving a BLAKE2b hash on every call would be noise; more to the point, holding it
/// makes it impossible for two calls in one session to disagree about the key.
pub struct DevicePad {
    device: DirectFile,
    k_perm: [u8; 32],
    /// LBA where Zone B begins. Always [`ZONE_B_LBA_START`] in production; the tests move it so the
    /// same code can be pointed at a small file instead of a 7.3 GiB one.
    zone_b_lba_start: u64,
}

impl DevicePad {
    /// Open the device at `path` for the stick whose root share is `kappa_usb`.
    ///
    /// `read_write` selects `O_RDWR`, which a save needs in order to ratchet. Unlock-and-view opens
    /// `O_RDONLY`, so a hardware write-blocker sees nothing to complain about.
    pub fn open(
        path: &std::path::Path,
        kappa_usb: &[u8; 32],
        read_write: bool,
    ) -> std::io::Result<Self> {
        let access = if read_write {
            Access::ReadWrite
        } else {
            Access::ReadOnly
        };
        let device = DirectFile::open(path, access, holonomy_core::layout::IO_ALIGN as u64)?;
        Ok(Self {
            device,
            k_perm: k_perm_key(kappa_usb),
            zone_b_lba_start: ZONE_B_LBA_START,
        })
    }

    /// Open with Zone B somewhere other than [`ZONE_B_LBA_START`], for tests.
    ///
    /// A real device needs `ZONE_B_LBA_START + ZONE_B_SECTORS` sectors before [`PadReader::new`] will
    /// accept it — 7.3 GiB — and a window then reads 32,768 sectors scattered across that whole
    /// codomain. Exercising the read loop against a file therefore needs the offsets relocated,
    /// which is all this does. `#[cfg(test)]`, so no released build can select it.
    #[cfg(test)]
    pub fn open_relocated(
        path: &std::path::Path,
        kappa_usb: &[u8; 32],
        zone_b_lba_start: u64,
    ) -> std::io::Result<Self> {
        let device = DirectFile::open(
            path,
            Access::ReadOnly,
            holonomy_core::layout::IO_ALIGN as u64,
        )?;
        Ok(Self {
            device,
            // Derived from the caller's share, not a placeholder: a handle whose key disagreed with
            // the one its window is checked against reads the wrong 32,768 sectors and returns an
            // unwritten file's zeros, which looks like working code until the assertions land.
            k_perm: k_perm_key(kappa_usb),
            zone_b_lba_start,
        })
    }

    /// Whether `O_DIRECT` survived, for the status line.
    ///
    /// On a filesystem that cannot do direct I/O (a tmpfs, or a loopback file on some kernels) this
    /// is `false` and reads go through the page cache. The bytes are the same; the guarantee that
    /// the kernel does not keep a copy of the pad is not, so the operator is told.
    pub fn is_direct(&self) -> bool {
        self.device.is_direct()
    }

    /// The read plan for one save.
    ///
    /// On a real device this goes through [`PadReader::new`], which verifies the plan is
    /// collision-free *and* that the device is long enough for all of Zone B before a single read is
    /// issued — so a truncated stick fails once, with a clear message, instead of 32,768 identical
    /// `EIO`s.
    ///
    /// A relocated handle (tests only) builds the plan directly. `PadReader::new` would refuse it for
    /// being smaller than 7.3 GiB, which is a property of the *device* check rather than of the plan,
    /// and the test's whole point is a small file.
    fn plan(&self, cursor: u64) -> Result<PadPlan, String> {
        if self.zone_b_lba_start == ZONE_B_LBA_START {
            let reader = PadReader::new(&self.device, &self.k_perm, cursor)
                .map_err(|e| format!("preparing the pad plan for save {cursor}: {e}"))?;
            return Ok(reader.plan().clone());
        }
        let plan = PadPlan::for_save(&self.k_perm, cursor);
        if !plan.is_collision_free() {
            return Err(format!(
                "the pad plan for save {cursor} is not collision-free"
            ));
        }
        Ok(plan)
    }

    /// Byte offset of plan entry `i`, honouring a relocated Zone B.
    fn offset_of(&self, plan: &PadPlan, i: usize) -> u64 {
        if self.zone_b_lba_start == ZONE_B_LBA_START {
            plan.byte_offset(i)
        } else {
            (self.zone_b_lba_start + plan.lbas()[i] as u64) * SECTOR_BYTES as u64
        }
    }
}

impl PadSource for DevicePad {
    fn window(&mut self, _kappa_usb: &[u8; 32], cursor: u64) -> Result<PadWindow, String> {
        let plan = self.plan(cursor)?;

        // One allocation, filled in plan order.
        //
        // `PadReader::read_into` would do this in one call, and it is what the production path would
        // use — but it bakes `ZONE_B_LBA_START` into its offsets, so a relocated handle cannot use it,
        // and two code paths for the same read is how the two come to disagree. The loop below is
        // the same read in the same order: `read_into` is itself a sector loop that copies in plan
        // order, so the bytes are identical by construction.
        //
        // `WINDOW_BYTES` is a multiple of `IO_ALIGN` (4,096), which `AlignedBuf::new` asserts.
        let mut bytes = AlignedBuf::new(WINDOW_BYTES);
        let mut sector = AlignedBuf::new(SECTOR_BYTES);
        for i in 0..plan.len() {
            self.device
                .read_exact_at(self.offset_of(&plan, i), &mut sector)
                .map_err(|e| {
                    format!(
                        "reading Zone B sector {} ({}) of save {cursor}: {e}",
                        i,
                        plan.lbas()[i]
                    )
                })?;
            let to = i * SECTOR_BYTES;
            bytes.as_mut_slice()[to..to + SECTOR_BYTES].copy_from_slice(sector.as_slice());
        }
        Ok(PadWindow::of(bytes))
    }

    fn ratchet(&mut self, _kappa_usb: &[u8; 32], cursor: u64) -> Result<u32, String> {
        if self.zone_b_lba_start != ZONE_B_LBA_START {
            return Err(
                "this handle has a relocated Zone B, so a ratchet would write to the wrong offsets; \
                 it is a test handle and cannot be used for a real save"
                    .to_string(),
            );
        }
        // Ratcheting writes, so it needs `PadReader` itself, not just a plan: the ascending-LBA
        // write order and the `fdatasync` live there. A relocated handle never reaches this line.
        let reader = PadReader::new(&self.device, &self.k_perm, cursor)
            .map_err(|e| format!("preparing the ratchet for save {cursor}: {e}"))?;
        reader
            .ratchet()
            .map_err(|e| format!("ratcheting the pad window for save {cursor}: {e}"))
    }

    fn can_ratchet(&self) -> bool {
        self.device.access() == Access::ReadWrite
    }

    fn describe(&self) -> String {
        format!(
            "device {} ({}{})",
            self.device.path(),
            if self.is_direct() {
                "O_DIRECT"
            } else {
                "buffered: this filesystem will not do direct I/O"
            },
            if self.can_ratchet() {
                ", read-write"
            } else {
                ", read-only"
            }
        )
    }
}

/// The stand-in: sector material derived from the permutation, for tests and for a file-backed demo.
///
/// It satisfies determinism and cursor-disjointness by construction — it goes through the same
/// [`crate::store::sector_index`] the device path goes through — and it **cannot ratchet**.
/// Returning `Ok(0)` or pretending to ratchet would let a save report a fresh pad it does not have,
/// so `can_ratchet` is `false` and `ratchet` is an error that says why.
///
/// What it is *not*: entropy. The real pad is CSPRNG bytes written to Zone B at provisioning time;
/// this derives its bytes from a hash of the sector index, so anyone holding `κ_USB` can compute the
/// whole window. A container written through this source is protected by the passphrase, the KEM
/// envelope and the pad *geometry*, but not by pad secrecy — which is exactly the half of the threat
/// model that needs a stick, and the reason `describe` says "stand-in" in the save report instead of
/// quietly reporting a device.
#[derive(Default)]
pub struct DerivedPad;

impl DerivedPad {
    /// A fresh stand-in.
    pub fn new() -> Self {
        Self
    }
}

impl PadSource for DerivedPad {
    fn window(&mut self, kappa_usb: &[u8; 32], cursor: u64) -> Result<PadWindow, String> {
        let mut bytes = AlignedBuf::new(WINDOW_BYTES);
        for j in 0..PAD_SECTORS_PER_SAVE as usize {
            let material = sector_material(crate::store::sector_index(kappa_usb, cursor, j));
            let to = j * SECTOR_BYTES;
            bytes.as_mut_slice()[to..to + SECTOR_BYTES].copy_from_slice(&material);
        }
        Ok(PadWindow::of(bytes))
    }

    fn ratchet(&mut self, _kappa_usb: &[u8; 32], _cursor: u64) -> Result<u32, String> {
        Err(
            "the stand-in pad cannot ratchet: it has no device to write fresh entropy to, so a given \
             cursor always yields the same sector material. That is why a container written without \
             --device is a demo and not a deployment"
                .to_string(),
        )
    }

    fn can_ratchet(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        "stand-in pad (no --device: sector material is derived from the permutation, not read, and \
         cannot ratchet)"
            .to_string()
    }
}

/// The permutation key `BLAKE2b("HOLONOMY_V51_K_PERM" ‖ 0x00 ‖ κ_USB)`, truncated to 32 bytes.
///
/// One definition, used by [`DevicePad`] and by [`crate::store::sector_index`], so the two cannot
/// disagree about which physical sector a save reads. `holonomy-usb`'s `PadPlan` takes exactly this
/// value; `the_store_and_the_usb_crate_choose_the_same_sectors` fails if they drift.
///
/// The salt is deliberately absent from the input: it lives inside the pad-covered region, so a key
/// that needed it could never be computed by a reader.
pub fn k_perm_key(kappa_usb: &[u8; 32]) -> [u8; 32] {
    use blake2::{Blake2b512, Digest};
    let mut h = Blake2b512::new();
    h.update(b"HOLONOMY_V51_K_PERM");
    h.update([0u8]);
    h.update(kappa_usb);
    let d = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d[..32]);
    out
}

/// 4,096 bytes of deterministic material for one sector index.
///
/// **This is not entropy.** It is a hash of the index, so it is reproducible by anyone who can
/// compute the permutation. Named for what it is, and reachable only through [`DerivedPad`].
pub fn sector_material(sector: u32) -> [u8; SECTOR_BYTES] {
    use blake2::{Blake2b512, Digest};
    let mut out = [0u8; SECTOR_BYTES];
    for (block, chunk) in out.chunks_mut(64).enumerate() {
        let mut h = Blake2b512::new();
        h.update(b"HOLONOMY_V51_STAND_IN_PAD");
        h.update(sector.to_le_bytes());
        h.update((block as u64).to_le_bytes());
        let d = h.finalize();
        let take = chunk.len();
        chunk.copy_from_slice(&d[..take]);
    }
    out
}

/// Where Zone B begins, re-exported because the device path and `--check` must agree on it and a
/// second literal is how the two would come to disagree.
pub const ZONE_B_LBA_START_SECTOR: u64 = ZONE_B_LBA_START;
