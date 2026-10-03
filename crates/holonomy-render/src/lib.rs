//! The SSE2 blitter, damage tracking, and the surface tree (text runs + icons + rects).
//!
//! One correction to the PRD's kernel, recorded here so it is not reintroduced: the
//! blend `(fg*a + bg*(255-a)) >> 8` is wrong by up to 1/255, because `255 * 255 = 65025`
//! overflows the 16-bit intermediate SSE2 gives you. Use a `u32` intermediate, or
//! `((fg*a) + (bg*(255-a)) + 127) / 255`. The PRD's scalar fallback has the same bug.
//!
//! Icons are hand-authored 1-bit masks in `.rodata`. No SVG runtime, no font parsing for
//! UI chrome.
//!
//! Lands in Phase 5. Gate: blend accuracy against an exact reference, damage-rect union
//! correctness, and a `HeadlessScanout` PPM fixture. See PROJECT.md §5 Phase 5 and
//! PRD §7.4.

pub use holonomy_jail::PHASE_0_PLACEHOLDER;
