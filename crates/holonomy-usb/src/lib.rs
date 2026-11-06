//! The USB umbilical: device geometry, aligned block I/O, the pad plan and the ratchet.
//!
//! # What was salvaged from `holonomy-old/`
//!
//! Exactly one module, [`io`], and for one reason: it had already discovered and documented that
//! `O_DIRECT` requires the *buffer address* to be 4096-aligned as well as the file offset, which
//! is the kind of thing that costs an afternoon when met as `EINVAL` with no explanation. The
//! salvage is the two facts and a 60-line buffer type, not the 9,209-line container crate it came
//! from. [`io::DirectFile`] was rewritten around a plain `File` and a `pread`/`pwrite` loop rather
//! than copied, because the original was entangled with a `SecureBlock` type that no longer
//! exists.
//!
//! # What was not salvaged
//!
//! The old `layout.rs` (which sized a *file*, not a device), the old `aead.rs`, and the old
//! `ring.rs` are not used. They solved the previous generation's problem — a 192 KiB ring inside a
//! file — and v5.1.0's geometry is different enough that taking them would have imported
//! assumptions along with code.
//!
//! # The honest statement about the pad
//!
//! See [`holonomy_crypto::usb_pad`]. This crate reads and ratchets; it does not prove device
//! freshness, erase old flash translations, reconcile external snapshots, or establish an
//! information-theoretic one-time-pad guarantee.

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

/// Device geometry, derived from one measurement instead of a hand-summed table.
pub mod geometry;
/// Aligned, unbuffered block I/O — the one thing salvaged from `holonomy-old/`.
pub mod io;
/// The pad plan and the sector ratchet.
pub mod pad;

pub use geometry::{Geometry, GeometryError};
pub use io::{Access, AlignedBuf, DirectFile};
pub use pad::{PadIoError, PadPlan, PadReader, PAD_BYTES_PER_SAVE, ZONE_B_LBA_START};
