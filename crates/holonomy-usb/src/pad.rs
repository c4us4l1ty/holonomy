//! Reading pad sectors off the USB, folding the container, and ratcheting spent sectors.
//!
//! # The two open modes, and what each may do
//!
//! * [`Access::ReadOnly`] — unlock and view. Zero writes to the bus, so a hardware write-blocker
//!   sees nothing to complain about. This is the mode the write-blocker requirement exists for.
//! * [`Access::ReadWrite`] — save, and *only* save. The ratchet overwrites the sectors that were
//!   just spent with fresh CSPRNG bytes, which is the only thing that restores pad freshness.
//!
//! # Where the pad comes from
//!
//! Zone B starts at LBA [`ZONE_B_LBA_START`] and holds [`ZONE_B_SECTORS`] 4 KiB sectors. A save
//! derives 32,768 logical indices from its monotonic cursor, maps each through the `FPE-22`
//! permutation in `holonomy-core`, and reads that LBA. Because the permutation is a bijection and
//! the cursor ranges are disjoint, no two sectors in one save overlap, and no two saves overlap
//! before the 54-save budget runs out.
//!
//! # Why the read order is shuffled
//!
//! A save issues 32,768 random reads. Sequential order would be faster and is deliberately not
//! used: the LBA sequence is the only observable that ties a save to a physical location pattern,
//! and reading in permutation order makes an on-bus trace of one save look like random access.

use std::io;

use holonomy_core::feistel::Feistel22;
use holonomy_core::layout::{PAD_SECTORS_PER_SAVE, ZONE_B_SECTORS};

use crate::io::{Access, AlignedBuf, DirectFile};

/// 4 KiB.
pub const SECTOR_BYTES: u64 = 4_096;

/// LBA where Zone B begins. Partition 1 (the 16 MiB launch stub) occupies LBA 0..32,767, so the
/// reservoir starts at 32,768 and the two never overlap.
pub const ZONE_B_LBA_START: u64 = 32_768;

/// Bytes of pad one save consumes: `32,768 × 4,096 = 134,217,728`.
pub const PAD_BYTES_PER_SAVE: u64 = PAD_SECTORS_PER_SAVE as u64 * SECTOR_BYTES;

const _: () = assert!(PAD_BYTES_PER_SAVE == 134_217_728);
const _: () = assert!(ZONE_B_LBA_START == 32_768);

/// A read plan: the physical LBAs one save will consume.
#[derive(Debug, Clone)]
pub struct PadPlan {
    lbas: Vec<u32>,
}

impl PadPlan {
    /// Build the plan for `cursor` (the monotonic save counter) under `k_perm`.
    pub fn for_save(k_perm: &[u8; 32], cursor: u64) -> Self {
        let feistel = Feistel22::new(k_perm, cursor);
        let mut lbas = Vec::with_capacity(PAD_SECTORS_PER_SAVE as usize);
        for j in 0..PAD_SECTORS_PER_SAVE {
            lbas.push(feistel.permute(Feistel22::logical_sector(cursor, j)));
        }
        Self { lbas }
    }

    pub fn len(&self) -> usize {
        self.lbas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lbas.is_empty()
    }

    pub fn lbas(&self) -> &[u32] {
        &self.lbas
    }

    /// Physical byte offset of sector `i`'s data on the raw device.
    pub fn byte_offset(&self, i: usize) -> u64 {
        (ZONE_B_LBA_START + self.lbas[i] as u64) * SECTOR_BYTES
    }

    /// Whether every sector in the plan is distinct — the per-save half of the non-overlap
    /// guarantee.
    pub fn is_collision_free(&self) -> bool {
        let mut sorted = self.lbas.clone();
        sorted.sort_unstable();
        sorted.dedup();
        sorted.len() == self.lbas.len() && self.lbas.iter().all(|&l| l < ZONE_B_SECTORS)
    }
}

/// Failures while reading or ratcheting the pad.
#[derive(Debug)]
pub enum PadIoError {
    /// The device returned an error. Often the first sign of a yank.
    Io(io::Error),
    /// The device is shorter than Zone B requires.
    DeviceTooSmall { needed: u64, found: u64 },
    /// A ratchet was attempted on a read-only handle.
    ReadOnly,
    /// The plan was internally inconsistent; a bug, not a hardware condition.
    BadPlan(&'static str),
}

impl std::fmt::Display for PadIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "USB I/O: {e}"),
            Self::DeviceTooSmall { needed, found } => write!(
                f,
                "device has {found} bytes, Zone B needs at least {needed}"
            ),
            Self::ReadOnly => f.write_str("ratchet attempted on a read-only handle"),
            Self::BadPlan(why) => write!(f, "bad pad plan: {why}"),
        }
    }
}

impl std::error::Error for PadIoError {}

impl From<io::Error> for PadIoError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// The pad side of a container session.
pub struct PadReader<'a> {
    device: &'a DirectFile,
    plan: PadPlan,
}

impl<'a> PadReader<'a> {
    /// Prepare to consume one save's pad.
    ///
    /// Verifies the device is at least `ZONE_B_LBA_START + ZONE_B_SECTORS` sectors long before
    /// issuing a single read, so a truncated image fails with a clear message instead of 32,768
    /// identical `EIO`s.
    pub fn new(
        device: &'a DirectFile,
        k_perm: &[u8; 32],
        cursor: u64,
    ) -> Result<Self, PadIoError> {
        let plan = PadPlan::for_save(k_perm, cursor);
        if !plan.is_collision_free() {
            return Err(PadIoError::BadPlan("plan contains a repeated or out-of-range sector"));
        }
        let needed = (ZONE_B_LBA_START + ZONE_B_SECTORS as u64) * SECTOR_BYTES;
        let found = device.len()?;
        if found < needed {
            return Err(PadIoError::DeviceTooSmall { needed, found });
        }
        Ok(Self { device, plan })
    }

    pub fn plan(&self) -> &PadPlan {
        &self.plan
    }

    /// Stream the whole pad into `out`, which must be exactly [`PAD_BYTES_PER_SAVE`] long.
    ///
    /// `on_beat` is invoked after every successful sector read so the caller can stamp the PHY
    /// heartbeat. It is a callback rather than an unconditional beat because the reaper must only
    /// be told "the PHY is fine" when a read actually succeeded — beating before the read is how a
    /// watchdog ends up never firing.
    pub fn read_into(
        &self,
        out: &mut AlignedBuf,
        mut on_beat: impl FnMut(),
    ) -> Result<(), PadIoError> {
        if out.len() as u64 != PAD_BYTES_PER_SAVE {
            return Err(PadIoError::BadPlan("output buffer is not exactly one save of pad"));
        }
        let mut sector = AlignedBuf::new(SECTOR_BYTES as usize);
        for i in 0..self.plan.len() {
            self.device.read_exact_at(self.plan.byte_offset(i), &mut sector)?;
            on_beat();
            let at = i * SECTOR_BYTES as usize;
            out.as_mut_slice()[at..at + SECTOR_BYTES as usize].copy_from_slice(sector.as_slice());
        }
        Ok(())
    }

    /// Overwrite each spent sector with fresh CSPRNG bytes.
    ///
    /// This is the ratchet. It requires `O_RDWR`; on a read-only handle it fails before touching
    /// anything, so a write-blocked session cannot half-ratchet.
    ///
    /// The writes are issued in ascending LBA order rather than permutation order. That looks
    /// inconsistent with the read order and is deliberate: reads are shuffled to hide the access
    /// pattern from the bus, while writes are a maintenance operation whose existence is already
    /// observable from the write itself, and ascending order is what lets the FTL coalesce.
    pub fn ratchet(&self) -> Result<u32, PadIoError> {
        if self.device.access() != Access::ReadWrite {
            return Err(PadIoError::ReadOnly);
        }
        let mut order: Vec<usize> = (0..self.plan.len()).collect();
        order.sort_by_key(|&i| self.plan.lbas[i]);

        let mut fresh = AlignedBuf::new(SECTOR_BYTES as usize);
        let mut written = 0u32;
        for i in order {
            fresh.as_mut_slice().copy_from_slice(&[0u8; SECTOR_BYTES as usize]);
            // CSPRNG, not a counter: a ratchet whose replacement bytes are predictable is not a
            // ratchet. See `holonomy_crypto::rng`.
            fill_random(fresh.as_mut_slice())?;
            self.device.write_exact_at(self.plan.byte_offset(i), &fresh)?;
            written += 1;
        }
        self.device.sync()?;
        Ok(written)
    }
}

fn fill_random(buf: &mut [u8]) -> Result<(), io::Error> {
    getrandom::getrandom(buf).map_err(|e| io::Error::other(format!("getrandom: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k() -> [u8; 32] {
        [0x5Au8; 32]
    }

    #[test]
    fn plan_has_exactly_one_save_of_sectors() {
        let p = PadPlan::for_save(&k(), 0);
        assert_eq!(p.len(), PAD_SECTORS_PER_SAVE as usize);
        assert!(p.is_collision_free());
    }

    #[test]
    fn plans_for_consecutive_saves_are_disjoint() {
        let mut seen = std::collections::HashSet::new();
        for cursor in 0..54u64 {
            let p = PadPlan::for_save(&k(), cursor);
            for &lba in p.lbas() {
                assert!(seen.insert(lba), "LBA {lba} reused at save {cursor}");
            }
        }
        assert_eq!(seen.len(), 54 * PAD_SECTORS_PER_SAVE as usize);
    }

    #[test]
    fn plan_sectors_stay_inside_zone_b() {
        for cursor in [0u64, 1, 53, 54, 200] {
            let p = PadPlan::for_save(&k(), cursor);
            assert!(p.lbas().iter().all(|&l| l < ZONE_B_SECTORS));
        }
    }

    #[test]
    fn offsets_are_sector_aligned_and_inside_the_device() {
        let p = PadPlan::for_save(&k(), 3);
        let needed = (ZONE_B_LBA_START + ZONE_B_SECTORS as u64) * SECTOR_BYTES;
        for i in 0..p.len() {
            let off = p.byte_offset(i);
            assert_eq!(off % SECTOR_BYTES, 0);
            assert!(off >= ZONE_B_LBA_START * SECTOR_BYTES);
            assert!(off + SECTOR_BYTES <= needed);
        }
    }

    #[test]
    fn pad_bytes_per_save_is_the_container_size() {
        // The 1:1 property: one save consumes precisely as many pad bytes as there are container
        // bytes, which is what makes the XOR a fold rather than a stream.
        assert_eq!(PAD_BYTES_PER_SAVE, holonomy_core::layout::CONTAINER_BYTES as u64);
    }

    #[test]
    fn short_device_is_rejected_before_any_read() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("usb-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("short-{}.bin", std::process::id()));
        std::fs::write(&path, vec![0u8; 1 << 20]).unwrap();

        let dev = DirectFile::open(&path, Access::ReadOnly, 4096).unwrap();
        let err = PadReader::new(&dev, &k(), 0).unwrap_err();
        match err {
            PadIoError::DeviceTooSmall { needed, found } => {
                assert!(needed > found);
                assert_eq!(found, 1 << 20);
            }
            other => panic!("expected DeviceTooSmall, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_only_handle_cannot_ratchet() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("usb-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("ro-{}.bin", std::process::id()));
        std::fs::write(&path, vec![0u8; 2 << 20]).unwrap();
        let dev = DirectFile::open(&path, Access::ReadOnly, 4096).unwrap();
        // Pass the size check by pointing at a small plan; the ratchet refusal is what is under
        // test, so build the reader over a device that is nominally too small by lying about
        // nothing — instead assert the guard directly.
        assert_eq!(dev.access(), Access::ReadOnly);
        let plan = PadPlan::for_save(&k(), 0);
        let reader = PadReader { device: &dev, plan };
        assert!(matches!(reader.ratchet(), Err(PadIoError::ReadOnly)));
        let _ = std::fs::remove_file(&path);
    }
}
