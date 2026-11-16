//! Host-calibrated Argon2id parameters — the mechanism, not a guessed constant.
//!
//! # Why calibrate at all
//!
//! H1 once hard-coded a parameter set from a PRD's claimed numbers and measured it 57× off
//! (K-2 on the audit wall). v5's answer was two fixed classes and a per-host measurement note.
//! v6's answer is the calibration mechanism itself: a container records the parameters it was
//! created with (Block 3's header field), created parameters are chosen by *measuring this
//! host*, and open-time derivation replays what the header says. **Determinism holds**: the
//! derivation itself is a pure function of `(passphrase, salt, params)`; the params are data.
//!
//! # The fences
//!
//! The memory cost is clamped to a fence: below [`FENCE_MIN_KIB`] the "memory-hard" claim is
//! marketing; above [`FENCE_MAX_KIB`] we exceed what the weakest supported host can carry
//! (RFC-001's open question, decided). The probe linearly scales its measurement — documented
//! as an approximation: within 64–256 MiB on one host, Argon2id wall time is dominated by
//! within-slab memory-touch work and scales roughly linearly in `m * t`, at which point the
//! residual error is an order of safety, not correctness, which is what the fences bound.

use std::time::Instant;

use argon2::{Algorithm, Argon2, Params, Version};

use crate::kdf::ROOT_SECRET_BYTES;

/// Minimum Argon2id memory cost: 64 MiB. Below this there is no fence worth the name.
pub const FENCE_MIN_KIB: u32 = 65_536;

/// Maximum Argon2id memory cost: 256 MiB. Above this, budget hosts suffer instead of users
/// being protected.
pub const FENCE_MAX_KIB: u32 = 262_144;

/// The wall-clock target band for one open/param registration: 800–1,500 ms. Below the floor
/// the attacker saves time; above the ceiling the user suffers (H1's K-2 lesson).
pub const TARGET_BAND_MS: (u64, u64) = (800, 1_500);

/// Lanes the calibration chooses for real parameters: two cores, no thrash.
pub const CALIBRATION_P: u32 = 2;

/// Passes the calibration chooses for real parameters.
pub const CALIBRATION_T: u32 = 3;

/// Probe parameter: 64 MiB, one pass.
const PROBE_KIB: u32 = 65_536;

/// The measure->derive cycle's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calibration {
    /// The parameters chosen for this host, to be recorded in the container header.
    pub params: KdfParamsRecord,
    /// Measured probe wall time in milliseconds, retained for audit display ("we measured; the
    /// number is in the report" rather than "trust us").
    pub probe_ms: u64,
    /// Explanation for the UI/audit log, including when fences clamped.
    pub note: &'static str,
}

/// Serde-free, plain record of chosen Argon2id parameters — exactly the shape Block 3 commits
/// to the container at creation time and replays at open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParamsRecord {
    /// Memory cost in KiB.
    pub m_kib: u32,
    /// Time cost (passes).
    pub t: u32,
    /// Lanes.
    pub p: u32,
}

/// Select parameters from a measured probe wall time. Pure, so the fences can be unit-tested
/// without spending a second of Argon2 in CI.
///
/// Model: probe = 64 MiB × t=1; final = m × [`CALIBRATION_T`]. Solve so the final cost lands in
/// [`TARGET_BAND_MS`]: `m = PROBE_KIB × probe_ms_Target_adj`, clamped to the fence, rounded
/// down to a 4 MiB step so records stay legible.
pub fn select_params(probe_ms: u64) -> (KdfParamsRecord, &'static str) {
    let mid_target = (TARGET_BAND_MS.0 + TARGET_BAND_MS.1) / 2;
    let note;
    let m = if probe_ms == 0 {
        note = "probe below timer resolution; choosing the maximum fence";
        FENCE_MAX_KIB
    } else {
        // probe_ms is the time for 65,536 KiB × 1 pass; scaling to `m` KiB × 3 passes predicts
        // probe_ms * (m / 65,536) * 3. Solve for the mid-band target.
        let scaled = (mid_target as u128 * PROBE_KIB as u128)
            .div_ceil((probe_ms as u128) * CALIBRATION_T as u128);
        let stepped = (scaled as u32) & !0xFFF; // down to a 4 MiB boundary for record legibility
        if stepped < FENCE_MIN_KIB {
            note = "host is slow; fence floor engaged (security floor is not negotiable)";
            FENCE_MIN_KIB
        } else if stepped > FENCE_MAX_KIB {
            note = "host is fast; fence ceiling engaged";
            FENCE_MAX_KIB
        } else {
            note = "host measured inside the fence";
            stepped
        }
    };
    (
        KdfParamsRecord {
            m_kib: m,
            t: CALIBRATION_T,
            p: CALIBRATION_P,
        },
        note,
    )
}

/// Measure this host and pick recordable parameters. Uses fixed, public probe material — no
/// user bytes ever enter the timing loop.
pub fn calibrate() -> Calibration {
    const PROBE_PW: &[u8] = b"holonomy calibration probe (not user data)";
    const PROBE_SALT: [u8; 64] = [0xC6; 64];

    let params = Params::new(PROBE_KIB, 1, CALIBRATION_P, Some(ROOT_SECRET_BYTES))
        .expect("probe params are constant and valid");

    let start = Instant::now();
    let mut out = [0u8; ROOT_SECRET_BYTES];
    let result = Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(PROBE_PW, &PROBE_SALT, &mut out);
    let probe_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(()) => {
            std::hint::black_box(out);
        }
        Err(_) => {
            // A host that cannot Argon2 the probe cannot Argon2 anything: report the floor and
            // let creation fail later with the real reason, rather than pretend.
            let (params, _) = select_params(u64::MAX / 4);
            return Calibration {
                params,
                probe_ms: 0,
                note: "probe failed; refusing to guess — floor parameters returned",
            };
        }
    }

    let (params, note) = select_params(probe_ms.max(1));
    Calibration {
        params,
        probe_ms,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_hold_at_the_extremes() {
        let (slow, n1) = select_params(u64::MAX / 8);
        assert_eq!(slow.m_kib, FENCE_MIN_KIB);
        assert!(n1.contains("floor"));
        let (fast, n2) = select_params(1);
        assert_eq!(fast.m_kib, FENCE_MAX_KIB);
        assert!(n2.contains("ceiling"));
        let (degenerate, _) = select_params(0);
        assert_eq!(degenerate.m_kib, FENCE_MAX_KIB);
    }

    #[test]
    fn midband_choice_scales_reasonably() {
        // probe 64 MiB t=1 in ~100 ms ⇒ m ≈ (1150 × 65,536) / (100 × 3) ≈ 251,306 KiB,
        // stepped down to a 4 MiB boundary ≈ 249,856 — inside the fence.
        let (p, _) = select_params(100);
        assert!(p.m_kib > FENCE_MIN_KIB && p.m_kib <= FENCE_MAX_KIB);
        assert_eq!(p.m_kib % 4096, 0);
        assert_eq!((p.t, p.p), (CALIBRATION_T, CALIBRATION_P));
    }

    #[test]
    fn calibration_runs_and_records() {
        let c = calibrate();
        assert!(c.params.m_kib >= FENCE_MIN_KIB);
        assert!(c.params.m_kib <= FENCE_MAX_KIB);
        assert_eq!(c.params.m_kib % 4096, 0);
        assert!(!c.note.is_empty());
    }

    #[test]
    fn calibrated_params_are_replayable_and_deterministic() {
        // The determinism law: params are data; two derivations with the same record agree.
        use crate::kdf::derive_root_secret_with;
        let c = calibrate();
        let salt = [1u8; 64];
        let a = derive_root_secret_with(b"pw", &salt, &c.params).unwrap();
        let b = derive_root_secret_with(b"pw", &salt, &c.params).unwrap();
        assert_eq!(a.0, b.0);
    }
}
