//! The `Scanout` trait: `DrmScanout` (real, default on Linux) and `HeadlessScanout`.
//!
//! PROJECT.md §2.7 verified that the DRM path is *more* exercisable here than expected:
//! allocate, `MAP_DUMB`, write, read back and destroy all succeed unprivileged on
//! `/dev/dri/card1` (i915). Only `SETCRTC` presentation is out of reach, because that
//! needs DRM master and therefore the VT.
//!
//! So `DrmScanout` is exercised for real on this machine; `HeadlessScanout` (anonymous
//! mmap + PPM dump) exists for deterministic pixel assertions, not as a fallback.
//! Hardware-only paths sit behind `--features hardware` so nothing depends on them.
//!
//! Lands in Phase 5. See PROJECT.md §2.7 and PRD §7.3.

pub use holonomy_jail::PHASE_0_PLACEHOLDER;
