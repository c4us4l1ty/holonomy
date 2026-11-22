//! Block 9's `L-7` gate: verified emergency wipe, zeroize canaries, and counted zero-allocation
//! scrubbing across a full 4 MiB `ResidentSet` and live `holonomy-seal` capability tokens.
//!
//! Three properties are measured and asserted in this single-test binary (single test because
//! `Counting` counters are process-global):
//!
//! 1. **Anti-decorative canary control:** leaving one canary slab unarmed out of 32 causes
//!    `CanarySet::sample_ghosts` to report `ghost_canaries == 1` and `ghost_bytes == 4096`. A
//!    probe that reported zero ghosts without reading memory fails here.
//! 2. **Counted zero-allocation wipe:** `SecretArenas::wipe` followed by `CanarySet::sample_ghosts`
//!    runs inside `claim_quiet` — zero heap allocations and zero deallocations on the emergency
//!    scrub path.
//! 3. **Reaper fire latency & completeness (`L-7 <= 50 ms`):** across 24 end-to-end trials covering
//!    all four `TripReason`s (`Manual`, `Suspend`, `UmbilicalDisconnect`, `DeadmanTimeout`), a
//!    full 4 MiB viewport of 1,008 resident plaintext slabs, live `UnwrapKey` + `DEMKey`
//!    capability tokens, and 32 × 4 KiB zeroize canaries are wiped and volatile-verified by the
//!    background reaper thread with `ghost_bytes == 0`, `dem.is_zeroized()`, `open_object`
//!    rejected post-wipe, every resident slab byte verified `0x00`, and worst-case trip-to-verified
//!    time `<= 50 ms`.

use core::ptr::read_volatile;
use std::sync::Arc;
use std::time::{Duration, Instant};

use holonomy_ledger::alloc::{claim_quiet, AllocStats, Counting};
use holonomy_ledger::timing::summarise;
use holonomy_seal::{
    create_capsule_for, open_object, seal_object, ArenaSlot, CanarySet, KdfParamsRecord,
    MasterKeys, ReaperGuard, SchedClass, SecretArenas, TripReason, Tripwire, CALIBRATION_P,
    CALIBRATION_T, DERIVE_SALT_BYTES, FENCE_MIN_KIB,
};
use holonomy_text::{ResidentSet, SegmentId, SegmentSource};

#[global_allocator]
static COUNTING: Counting = Counting;

const VIEWPORT_BUDGET: usize = 4 * 1024 * 1024;
const LEAF_BYTES: usize = 4_096;
const SLACK_BYTES: usize = 64;
const CANARY_COUNT: usize = 32;
const CANARY_BYTES: usize = 4_096;
const TRIALS: usize = 24;
const L7_BOUND: Duration = Duration::from_millis(50);

struct NonZeroProse;

impl SegmentSource for NonZeroProse {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        let base = b'a' + (segment.get() % 26) as u8;
        let mut buf = vec![base; LEAF_BYTES];
        buf[LEAF_BYTES - 1] = b'\n';
        Ok(buf)
    }
}

fn fill_full_viewport() -> ResidentSet<NonZeroProse> {
    let mut set =
        ResidentSet::new(NonZeroProse, VIEWPORT_BUDGET, SLACK_BYTES).expect("non-zero budget");
    let max_segments = VIEWPORT_BUDGET / (LEAF_BYTES + SLACK_BYTES);
    for id in 0..max_segments as u64 {
        set.fault_in(SegmentId::new(id)).expect("segment faults in");
        // Write into editing slack too so spare capacity carries non-zero bytes before wipe.
        let slab = set.fault_in_mut(SegmentId::new(id)).expect("resident");
        slab.insert(0, b"secret-prefix ")
            .expect("slack holds prefix");
        slab.delete(0, 7).expect("cut leaves rest");
    }
    set
}

#[test]
fn reaper_wipes_full_viewport_keys_and_canaries_under_fifty_ms_with_zero_ghosts() {
    // 1. Negative control: prove the canary probe catches an unarmed buffer, and prove the
    //    allocation counter is installed.
    let before_ctrl = AllocStats::snapshot();
    let ctrl_arenas = SecretArenas::new();
    let mut ctrl_canaries = CanarySet::new(CANARY_COUNT, CANARY_BYTES);
    assert_eq!(
        ctrl_canaries.arm_prefix(&ctrl_arenas, CANARY_COUNT - 1),
        CANARY_COUNT - 1
    );
    let after_ctrl_alloc = AllocStats::snapshot();
    assert!(
        after_ctrl_alloc.delta(&before_ctrl).allocations == 0
            || before_ctrl.delta(&after_ctrl_alloc).allocations > 0,
        "counting allocator must observe setup allocations"
    );
    assert!(
        before_ctrl.delta(&after_ctrl_alloc).allocations >= CANARY_COUNT as u64,
        "counting allocator must count canary allocations"
    );

    // Counted wipe: `wipe()` + `sample_ghosts()` must allocate zero bytes on the emergency path.
    let negative_report = claim_quiet(|| {
        let stats = ctrl_arenas.wipe();
        assert_eq!(stats.arenas_wiped, CANARY_COUNT - 1);
        ctrl_canaries.sample_ghosts()
    });
    println!(
        "HOLONOMY_WIPE_CONTROL sampled={} ghost_canaries={} ghost_bytes={} quiet_allocs=0",
        negative_report.canaries_sampled,
        negative_report.ghost_canaries,
        negative_report.ghost_bytes,
    );
    assert_eq!(
        negative_report.ghost_canaries, 1,
        "unarmed canary must be caught as a ghost"
    );
    assert_eq!(
        negative_report.ghost_bytes, CANARY_BYTES,
        "every non-zero byte of the unarmed canary must be detected"
    );
    drop(ctrl_canaries);

    // Derive session unwrap capability once under a production-fenced schedule so this binary runs
    // identically in debug and `--release` builds (OQ-6 refuses test schedules in release).
    let params = KdfParamsRecord {
        m_kib: FENCE_MIN_KIB,
        t: CALIBRATION_T,
        p: CALIBRATION_P,
    };
    let salt = [0x42u8; DERIVE_SALT_BYTES];
    let seed_keys =
        MasterKeys::derive_with(b"l7-reaper-fire-passphrase", &salt, None, &params).expect("kdf");
    let (_, seed_dem) = create_capsule_for(seed_keys.unwrap_key(), None).expect("capsule");

    // 2. Twenty-four end-to-end reaper fire trials across all four tripwire reasons.
    let reasons = [
        TripReason::Manual,
        TripReason::Suspend,
        TripReason::UmbilicalDisconnect,
        TripReason::DeadmanTimeout,
    ];
    let mut reaper_samples = Vec::with_capacity(TRIALS);
    let mut wall_samples = Vec::with_capacity(TRIALS);
    let mut total_ghosts = 0usize;
    let mut last_bytes_wiped = 0usize;
    let mut last_arenas_wiped = 0usize;
    let mut observed_sched = SchedClass::NormalFallback;

    for trial in 0..TRIALS {
        let reason = reasons[trial % reasons.len()];
        let arenas = SecretArenas::new();
        let mut resident = fill_full_viewport();
        let expected_slabs = resident.resident_segments();
        assert!(
            expected_slabs >= 1_000,
            "4 MiB viewport must hold >= 1000 4 KiB slabs, got {expected_slabs}"
        );

        let mut slab_slots: Vec<ArenaSlot> = Vec::with_capacity(expected_slabs);
        for (_, slab) in resident.resident_slabs_mut() {
            let (ptr, cap) = slab.allocation_range();
            // SAFETY: `slab` stays pinned inside `resident` until we explicitly unregister every
            // `slab_slots` handle below before `resident` drops.
            let slot = unsafe { arenas.register(ptr, cap) }.expect("arena capacity");
            slab_slots.push(slot);
        }

        let mut dem = seed_dem.duplicate();
        let sealed_probe = seal_object(&dem, b"l7:probe", b"canary-authenticated-payload");
        assert!(
            open_object(&dem, b"l7:probe", &sealed_probe).is_ok(),
            "live DEMKey must open sealed probe before wipe"
        );
        assert!(!dem.is_zeroized(), "live DEMKey must not start zeroized");
        assert!(dem.arm_reaper(&arenas), "DEMKey must arm in SecretArenas");

        let mut canaries = CanarySet::new(CANARY_COUNT, CANARY_BYTES);
        assert_eq!(canaries.arm_all(&arenas), CANARY_COUNT);
        let canaries = Arc::new(canaries);

        let deadman = if reason == TripReason::DeadmanTimeout {
            Some(Duration::from_millis(2))
        } else {
            None
        };
        let tripwire = Tripwire::new(deadman);
        tripwire.beat();
        let guard = ReaperGuard::spawn(
            Arc::clone(&arenas),
            Arc::clone(&canaries),
            Arc::clone(&tripwire),
            Duration::from_micros(400),
        );

        let wall_start = if reason == TripReason::DeadmanTimeout {
            // Wait until the 2 ms dead-man window expires, then time from deadline expiry.
            std::thread::sleep(Duration::from_millis(2));
            Instant::now()
        } else {
            let t0 = Instant::now();
            tripwire.fire(reason);
            t0
        };

        let outcome = guard.wait_outcome().expect("reaper must fire and return");
        let wall_elapsed = wall_start.elapsed();

        assert_eq!(outcome.reason, reason);
        observed_sched = outcome.sched_class;
        total_ghosts = total_ghosts.saturating_add(outcome.canaries.ghost_bytes);
        assert_eq!(outcome.canaries.ghost_canaries, 0, "trial {trial}: ghosts");
        assert_eq!(
            outcome.canaries.ghost_bytes, 0,
            "trial {trial}: ghost bytes"
        );
        assert!(dem.is_zeroized(), "trial {trial}: DEMKey must be zeroized");
        assert!(
            open_object(&dem, b"l7:probe", &sealed_probe).is_err(),
            "trial {trial}: wiped DEMKey must reject open_object"
        );

        // Volatile-verify every byte of every resident plaintext slab (`0..capacity`) is `0x00`.
        let mut surviving_slab_bytes = 0usize;
        for (_, slab) in resident.resident_slabs_mut() {
            let (ptr, cap) = slab.allocation_range();
            for offset in 0..cap {
                // SAFETY: `slab` owns `cap` initialized bytes.
                if unsafe { read_volatile(ptr.add(offset)) } != 0 {
                    surviving_slab_bytes += 1;
                }
            }
        }
        assert_eq!(
            surviving_slab_bytes, 0,
            "trial {trial}: surviving resident plaintext bytes after wipe"
        );

        for slot in slab_slots {
            arenas.unregister(slot);
        }
        last_bytes_wiped = outcome.wipe.bytes_wiped;
        last_arenas_wiped = outcome.wipe.arenas_wiped;
        reaper_samples.push(Duration::from_nanos(outcome.trip_to_verified_ns));
        wall_samples.push(wall_elapsed);
    }

    let reaper_dist = summarise(reaper_samples);
    let wall_dist = summarise(wall_samples);
    println!(
        "HOLONOMY_WIPE_GATE trials={} arenas={} bytes_wiped={} ghosts={} sched={:?} \
         reaper_p50_us={} reaper_p99_us={} reaper_max_us={} wall_p50_us={} wall_p99_us={} \
         wall_max_us={}",
        TRIALS,
        last_arenas_wiped,
        last_bytes_wiped,
        total_ghosts,
        observed_sched,
        reaper_dist.p50.as_micros(),
        reaper_dist.p99.as_micros(),
        reaper_dist.max.as_micros(),
        wall_dist.p50.as_micros(),
        wall_dist.p99.as_micros(),
        wall_dist.max.as_micros(),
    );

    assert_eq!(total_ghosts, 0);
    assert!(
        reaper_dist.max <= L7_BOUND,
        "L-7 violated: reaper trip-to-verified max {:?} exceeded {:?}",
        reaper_dist.max,
        L7_BOUND
    );
    assert!(
        wall_dist.p99 <= L7_BOUND,
        "L-7 violated: caller wall p99 {:?} exceeded {:?}",
        wall_dist.p99,
        L7_BOUND
    );
}
