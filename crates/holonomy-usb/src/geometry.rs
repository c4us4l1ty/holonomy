//! The 8 GB pendrive's partition arithmetic, as corrected.
//!
//! # The bug this file fixes
//!
//! `v5.0.0-OMEGA` §5.1 drew the reservoir like this:
//!
//! ```text
//!   Partition 1  : 16 MiB            LBA 0 .. 32,767
//!   Zone A       : 256 MiB × 4     = 1,024 MiB   RS(255,223) AONT key shares
//!   Zone B       : 6.80 GiB                       1:1 Vernam pad
//!   Zone C       : 150 MiB                        parity & ratchet log
//!   Zone D       : 4.0 MiB                        heartbeat ring
//! ```
//!
//! Those numbers do not fit on the drive. `16 + 1024 + 6.80·1024 + 150 + 4 = 8,155 MiB`, against a
//! "8 GB" stick that provides `8,000,000,000 / 1,048,576 = 7,629.4 MiB`. **The budget is over by
//! 526 MiB**, and the diagram's `7.98 GiB` reservoir is itself larger than the whole device.
//!
//! There is a second, independent unit error on the same line: it labels `LBA 0 .. 32,767` as
//! "16.00 MiB", which is only true for 512-byte LBAs, while every other figure in the table counts
//! 4 KiB sectors. [`SECTOR_BYTES`] makes the unit explicit and [`Geometry::from_device_len`]
//! derives every zone from one measurement rather than from a hand-summed table.
//!
//! # The correction
//!
//! Zone A shrinks from `256 MiB × 4` to **256 MiB total** (four 64 MiB RS shares). The root share
//! `κ_USB` is 64 bytes; a quarter of a gigabyte of `RS(255,223)`-coded redundancy for it is already
//! four orders of magnitude more than the coding theorem requires, and the extra 768 MiB is what
//! was pushing Zone B over the edge. With that change the layout fits with room to spare, and
//! Zone B keeps the documented `1,782,579` sectors, which is what preserves the 54-save budget.

use holonomy_core::layout::{PAD_SECTORS_PER_SAVE, SAVES_BEFORE_DOMAIN_REUSE, ZONE_B_SECTORS};

/// The nominal "8 GB" drive, in bytes. Vendors mean `10^9`, not `2^30`, which is the other reason
/// the old table overran: `8 GiB` of zones cannot fit on `8 GB`.
pub const DEVICE_BYTES: u64 = 8_000_000_000;

/// Logical block size used for every figure here. 4 KiB, stated once.
pub const SECTOR_BYTES: u64 = 4_096;

/// Partition 1: the read-only launch stub. 16 MiB.
pub const P1_BYTES: u64 = 16 * 1024 * 1024;

/// Zone A: `RS(255,223)` AONT shares. **256 MiB total** — see the module docs.
pub const ZONE_A_BYTES: u64 = 256 * 1024 * 1024;

/// Zone C: parity and the ratchet log. 150 MiB.
pub const ZONE_C_BYTES: u64 = 150 * 1024 * 1024;

/// Zone D: the 50 Hz heartbeat ring. 4 MiB.
pub const ZONE_D_BYTES: u64 = 4 * 1024 * 1024;

/// Fixed overhead before Zone B gets the rest.
pub const OVERHEAD_BYTES: u64 = P1_BYTES + ZONE_A_BYTES + ZONE_C_BYTES + ZONE_D_BYTES;

const _: () = assert!(P1_BYTES.is_multiple_of(SECTOR_BYTES));
const _: () = assert!(ZONE_A_BYTES.is_multiple_of(SECTOR_BYTES));
const _: () = assert!(OVERHEAD_BYTES < DEVICE_BYTES);

/// The derived layout of one device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Total usable bytes.
    pub device_bytes: u64,
    /// Sector count of the whole device.
    pub device_sectors: u64,
    /// Zone B's first LBA.
    pub zone_b_start: u64,
    /// Zone B's usable sector count.
    pub zone_b_sectors: u64,
}

impl Geometry {
    /// Derive the layout from a measured device length.
    ///
    /// Zones A, C and D are fixed sizes; Zone B absorbs whatever is left. A device too small for
    /// [`SAVES_BEFORE_DOMAIN_REUSE`] saves is rejected rather than silently giving a shorter pad
    /// budget — the number 54 is in the documentation, and a stick that cannot deliver it must not
    /// pretend to.
    pub fn from_device_len(len: u64) -> Result<Self, GeometryError> {
        if len < OVERHEAD_BYTES + MIN_ZONE_B_BYTES {
            return Err(GeometryError::TooSmall {
                found: len,
                needed: OVERHEAD_BYTES + MIN_ZONE_B_BYTES,
            });
        }
        let device_sectors = len / SECTOR_BYTES;
        let overhead_sectors = OVERHEAD_BYTES / SECTOR_BYTES;
        let zone_b_start = P1_BYTES / SECTOR_BYTES;
        let zone_b_sectors = device_sectors - overhead_sectors;

        if zone_b_sectors < ZONE_B_SECTORS as u64 {
            // The pad budget in the docs assumes at least this many sectors. Anything less and the
            // 54-save guarantee is quietly false, which is worse than refusing the stick.
            return Err(GeometryError::ZoneBTooSmall {
                found: zone_b_sectors,
                needed: ZONE_B_SECTORS as u64,
            });
        }

        Ok(Self {
            device_bytes: len,
            device_sectors,
            zone_b_start,
            zone_b_sectors,
        })
    }

    /// The layout of a nominal 8 GB stick, for tests and for provisioning docs.
    pub fn nominal() -> Self {
        Self::from_device_len(DEVICE_BYTES).expect("the nominal device must satisfy the budget")
    }

    /// How many saves this device's Zone B can serve before the domain wraps.
    pub fn saves_afforded(&self) -> u64 {
        self.zone_b_sectors / PAD_SECTORS_PER_SAVE as u64
    }

    /// Whether the documented 54-save budget holds on this device.
    pub fn meets_pad_budget(&self) -> bool {
        self.saves_afforded() >= SAVES_BEFORE_DOMAIN_REUSE
    }

    /// Zone B's size in bytes.
    pub fn zone_b_bytes(&self) -> u64 {
        self.zone_b_sectors * SECTOR_BYTES
    }
}

/// Minimum Zone B size: exactly enough for the documented budget.
pub const MIN_ZONE_B_BYTES: u64 = ZONE_B_SECTORS as u64 * SECTOR_BYTES;

/// Layout failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryError {
    /// The device cannot hold the fixed zones plus a full pad budget.
    TooSmall {
        /// Bytes the device reports.
        found: u64,
        /// Bytes required for the fixed zones plus 54 saves of pad.
        needed: u64,
    },
    /// The device has room for the fixed zones but not for 54 saves of pad.
    ZoneBTooSmall {
        /// Zone B sectors the device would have.
        found: u64,
        /// Zone B sectors the documented budget requires.
        needed: u64,
    },
}

impl std::fmt::Display for GeometryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooSmall { found, needed } => write!(
                f,
                "device is {found} bytes; the fixed zones plus a full pad budget need {needed}"
            ),
            Self::ZoneBTooSmall { found, needed } => write!(
                f,
                "Zone B would be {found} sectors, which serves {} saves; {needed} sectors are \
                 needed for the documented {SAVES_BEFORE_DOMAIN_REUSE}",
                found / PAD_SECTORS_PER_SAVE as u64
            ),
        }
    }
}

impl std::error::Error for GeometryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_budget_did_not_fit() {
        // Keep the finding executable: 16 + 1024 + 6.80 GiB + 150 + 4 MiB against an 8 GB stick.
        const MIB: u64 = 1024 * 1024;
        let old = 16 * MIB + 4 * 256 * MIB + 1_782_579 * 4096 + 150 * MIB + 4 * MIB;
        assert!(old > DEVICE_BYTES, "the old table must be shown to overflow");
        // And by how much: about 526 MiB.
        let over_mib = (old - DEVICE_BYTES) / MIB;
        assert!((520..535).contains(&over_mib), "over by {over_mib} MiB");
    }

    #[test]
    fn corrected_budget_fits_with_room() {
        let g = Geometry::nominal();
        assert!(g.device_bytes <= DEVICE_BYTES);
        assert!(g.zone_b_bytes() + OVERHEAD_BYTES <= g.device_bytes);
        assert!(g.meets_pad_budget(), "saves afforded: {}", g.saves_afforded());
        // Zone B is at least the documented 1,782,579 sectors.
        assert!(g.zone_b_sectors >= ZONE_B_SECTORS as u64);
    }

    #[test]
    fn zone_a_shrank_to_a_quarter_gigabyte() {
        let g = Geometry::nominal();
        // The whole point of the correction: 256 MiB total, not 4 × 256 MiB.
        assert_eq!(ZONE_A_BYTES, 256 * 1024 * 1024);
        // With the old 1 GiB, eight nominal sticks' worth of padding would be missing.
        let old_overhead = OVERHEAD_BYTES + 3 * ZONE_A_BYTES;
        assert!(old_overhead + MIN_ZONE_B_BYTES > g.device_bytes);
    }

    #[test]
    fn zone_b_starts_after_partition_one() {
        let g = Geometry::nominal();
        assert_eq!(g.zone_b_start, P1_BYTES / SECTOR_BYTES);
        assert_eq!(g.zone_b_start, 4_096); // 16 MiB in 4 KiB sectors
    }

    #[test]
    fn a_too_small_device_is_refused() {
        let err = Geometry::from_device_len(64 * 1024 * 1024).unwrap_err();
        assert!(matches!(err, GeometryError::TooSmall { .. }));
    }

    #[test]
    fn a_device_with_no_room_for_the_pad_budget_is_refused() {
        // Big enough for the fixed zones, not big enough for 54 saves.
        let len = OVERHEAD_BYTES + MIN_ZONE_B_BYTES - SECTOR_BYTES;
        let err = Geometry::from_device_len(len).unwrap_err();
        assert!(matches!(
            err,
            GeometryError::TooSmall { .. } | GeometryError::ZoneBTooSmall { .. }
        ));
        assert!(err.to_string().contains("54") || err.to_string().contains("pad budget"));
    }

    #[test]
    fn sector_arithmetic_is_exact() {
        let g = Geometry::nominal();
        assert_eq!(g.device_sectors, DEVICE_BYTES / SECTOR_BYTES);
        assert_eq!(g.device_sectors, 1_953_125);
        assert_eq!(
            g.zone_b_sectors + OVERHEAD_BYTES / SECTOR_BYTES,
            g.device_sectors
        );
    }
}
