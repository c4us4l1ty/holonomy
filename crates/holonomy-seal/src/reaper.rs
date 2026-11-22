//! Emergency secret wipe, zeroize canaries, and the zero-I/O reaper (ROADMAP Block 9, `L-7`).
//!
//! Lineage: adapted from v5.1.0's audited `holonomy-runtime::{scrub, reaper}` zero-I/O Thread B,
//! upgraded with two v6 invariants:
//!
//! 1. **Concurrency-safe arena slots (`SecretArenas`):** normal eviction/drop and emergency wipe
//!    coordinate through a lock-free per-slot atomic state machine (`EMPTY` / `INIT` / `ARMED` /
//!    `WIPING` / `UNREGISTERING`), so `Drop` never frees a buffer mid-wipe and `wipe()` never
//!    allocates, takes a mutex, or makes a syscall.
//! 2. **Zeroize canaries (`CanarySet`):** non-zero patterned buffers allocated on the same heap
//!    path as resident plaintext and armed in the same `SecretArenas`. After `wipe()`, volatile
//!    read-back across every canary byte counts surviving "ghost" bytes — an unarmed or skipped
//!    buffer reports `ghost_bytes > 0`, making decorative wipe claims fail loudly.

use core::hint::spin_loop;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use zeroize::Zeroize;

/// Maximum simultaneously armed secret/slab/canary arenas (`2048` covers a full 4 MiB viewport of
/// 4 KiB slabs plus capability keys and zeroize canaries).
pub const MAX_ARENAS: usize = 2_048;

const SLOT_EMPTY: usize = 0;
const SLOT_INIT: usize = 1;
const SLOT_ARMED: usize = 2;
const SLOT_WIPING: usize = 3;
const SLOT_UNREGISTERING: usize = 4;

/// Opaque handle to one armed slot inside [`SecretArenas`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaSlot(usize);

struct Slot {
    state: AtomicUsize,
    address: AtomicUsize,
    length: AtomicUsize,
}

/// Counts returned by [`SecretArenas::wipe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WipeStats {
    /// Number of armed slots scrubbed.
    pub arenas_wiped: usize,
    /// Total bytes overwritten with volatile zeros.
    pub bytes_wiped: usize,
}

/// Lock-free fixed-capacity registry of live secret, plaintext, and canary byte ranges.
pub struct SecretArenas {
    slots: [Slot; MAX_ARENAS],
}

impl SecretArenas {
    /// Creates an empty registry on the heap without blowing small thread stacks.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            slots: [const {
                Slot {
                    state: AtomicUsize::new(SLOT_EMPTY),
                    address: AtomicUsize::new(0),
                    length: AtomicUsize::new(0),
                }
            }; MAX_ARENAS],
        })
    }

    /// Registers `[ptr, ptr + len)` for emergency volatile zeroing.
    ///
    /// # Safety
    ///
    /// `[ptr, ptr + len)` must point to a live, initialized, mutable allocation that stays at
    /// that address until [`Self::unregister`] returns for the yielded [`ArenaSlot`] (or the
    /// process terminates).
    pub unsafe fn register(&self, ptr: *mut u8, len: usize) -> Option<ArenaSlot> {
        if ptr.is_null() || len == 0 {
            return None;
        }
        for (idx, slot) in self.slots.iter().enumerate() {
            if slot
                .state
                .compare_exchange(SLOT_EMPTY, SLOT_INIT, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                slot.address.store(ptr as usize, Ordering::Relaxed);
                slot.length.store(len, Ordering::Relaxed);
                slot.state.store(SLOT_ARMED, Ordering::Release);
                return Some(ArenaSlot(idx));
            }
        }
        None
    }

    /// Disarms `handle`, waiting if an emergency [`Self::wipe`] is actively scrubbing this slot
    /// so the caller may safely deallocate immediately after return.
    pub fn unregister(&self, handle: ArenaSlot) {
        let Some(slot) = self.slots.get(handle.0) else {
            return;
        };
        loop {
            match slot.state.compare_exchange(
                SLOT_ARMED,
                SLOT_UNREGISTERING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    slot.address.store(0, Ordering::Relaxed);
                    slot.length.store(0, Ordering::Relaxed);
                    slot.state.store(SLOT_EMPTY, Ordering::Release);
                    return;
                }
                Err(SLOT_WIPING) => spin_loop(),
                Err(_) => return,
            }
        }
    }

    /// How many slots are currently armed.
    #[must_use]
    pub fn armed_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| {
                let st = s.state.load(Ordering::Acquire);
                st == SLOT_ARMED || st == SLOT_WIPING
            })
            .count()
    }

    /// Overwrites every armed byte range with volatile zeros and emits a `SeqCst` fence.
    ///
    /// Zero allocations, zero locks, zero syscalls. Slots remain `ARMED` afterward so post-wipe
    /// inspection or normal `Drop` unregistration remains deterministic.
    pub fn wipe(&self) -> WipeStats {
        let mut arenas_wiped = 0usize;
        let mut bytes_wiped = 0usize;
        for slot in &self.slots {
            if slot
                .state
                .compare_exchange(SLOT_ARMED, SLOT_WIPING, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let ptr = slot.address.load(Ordering::Relaxed) as *mut u8;
                let len = slot.length.load(Ordering::Relaxed);
                if !ptr.is_null() && len > 0 {
                    for offset in 0..len {
                        // SAFETY: `SLOT_WIPING` prevents `unregister` from returning until we
                        // restore `SLOT_ARMED`, so `[ptr, ptr + len)` stays live and exclusive.
                        unsafe { write_volatile(ptr.add(offset), 0u8) };
                    }
                    arenas_wiped += 1;
                    bytes_wiped = bytes_wiped.saturating_add(len);
                }
                slot.state.store(SLOT_ARMED, Ordering::Release);
            }
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        WipeStats {
            arenas_wiped,
            bytes_wiped,
        }
    }
}

/// Post-wipe verification counts produced by [`CanarySet::sample_ghosts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanaryReport {
    /// Total canary buffers inspected.
    pub canaries_sampled: usize,
    /// Total bytes read via `read_volatile`.
    pub bytes_sampled: usize,
    /// Canaries with at least one surviving non-zero byte.
    pub ghost_canaries: usize,
    /// Total surviving non-zero canary bytes (`0` is required by `L-7`).
    pub ghost_bytes: usize,
}

struct CanarySlab {
    bytes: Box<[u8]>,
    armed: Option<(Arc<SecretArenas>, ArenaSlot)>,
}

impl CanarySlab {
    fn new(seed: u64, len: usize) -> Self {
        let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
        let mut vec = vec![0u8; len];
        for byte in &mut vec {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            let raw = (z & 0xFF) as u8;
            // Never emit `0x00`: every canary byte starts non-zero so any unwiped byte is caught.
            *byte = if raw == 0 { 0xA5 } else { raw };
        }
        Self {
            bytes: vec.into_boxed_slice(),
            armed: None,
        }
    }

    fn arm(&mut self, arenas: &Arc<SecretArenas>) -> bool {
        if self.armed.is_some() || self.bytes.is_empty() {
            return false;
        }
        let ptr = self.bytes.as_mut_ptr();
        let len = self.bytes.len();
        // SAFETY: `self.bytes` is a fixed-size boxed slice whose address does not move, and `Drop`
        // unregisters the slot before zeroizing and freeing `self.bytes`.
        if let Some(slot) = unsafe { arenas.register(ptr, len) } {
            self.armed = Some((Arc::clone(arenas), slot));
            true
        } else {
            false
        }
    }

    fn ghost_bytes(&self) -> usize {
        let ptr = self.bytes.as_ptr();
        let len = self.bytes.len();
        let mut ghosts = 0usize;
        for offset in 0..len {
            // SAFETY: `self.bytes` owns `len` initialized bytes.
            let b = unsafe { read_volatile(ptr.add(offset)) };
            if b != 0 {
                ghosts += 1;
            }
        }
        ghosts
    }
}

impl Drop for CanarySlab {
    fn drop(&mut self) {
        if let Some((arenas, slot)) = self.armed.take() {
            arenas.unregister(slot);
        }
        self.bytes.zeroize();
    }
}

/// Inert non-zero patterned buffers riding the heap allocator alongside resident plaintext.
pub struct CanarySet {
    slabs: Vec<CanarySlab>,
}

impl CanarySet {
    /// Allocates `count` non-zero patterned canary buffers of `bytes_each` bytes.
    #[must_use]
    pub fn new(count: usize, bytes_each: usize) -> Self {
        let slabs = (0..count)
            .map(|idx| CanarySlab::new((idx as u64).wrapping_add(1), bytes_each))
            .collect();
        Self { slabs }
    }

    /// Arms every canary in `arenas`, returning how many were newly armed.
    pub fn arm_all(&mut self, arenas: &Arc<SecretArenas>) -> usize {
        let mut armed = 0usize;
        for slab in &mut self.slabs {
            if slab.arm(arenas) {
                armed += 1;
            }
        }
        armed
    }

    /// Arms only the first `limit` canaries, leaving any remainder unarmed for negative controls.
    pub fn arm_prefix(&mut self, arenas: &Arc<SecretArenas>, limit: usize) -> usize {
        let mut armed = 0usize;
        for slab in self.slabs.iter_mut().take(limit) {
            if slab.arm(arenas) {
                armed += 1;
            }
        }
        armed
    }

    /// Volatile-reads every canary buffer and counts surviving non-zero ghost bytes.
    ///
    /// Zero allocations, zero locks, zero syscalls — safe to call on the emergency reaper path.
    #[must_use]
    pub fn sample_ghosts(&self) -> CanaryReport {
        let mut bytes_sampled = 0usize;
        let mut ghost_canaries = 0usize;
        let mut ghost_bytes = 0usize;
        for slab in &self.slabs {
            bytes_sampled = bytes_sampled.saturating_add(slab.bytes.len());
            let ghosts = slab.ghost_bytes();
            if ghosts > 0 {
                ghost_canaries += 1;
                ghost_bytes = ghost_bytes.saturating_add(ghosts);
            }
        }
        CanaryReport {
            canaries_sampled: self.slabs.len(),
            bytes_sampled,
            ghost_canaries,
            ghost_bytes,
        }
    }
}

/// Why the reaper tripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TripReason {
    /// Explicit user or programmatic panic trigger.
    Manual = 1,
    /// OS suspend / sleep / lid-close notification.
    Suspend = 2,
    /// Optional USB deadbolt / umbilical yank notification.
    UmbilicalDisconnect = 3,
    /// Dead-man heartbeat expired.
    DeadmanTimeout = 4,
}

impl TripReason {
    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Manual),
            2 => Some(Self::Suspend),
            3 => Some(Self::UmbilicalDisconnect),
            4 => Some(Self::DeadmanTimeout),
            _ => None,
        }
    }
}

/// Scheduler class achieved by the reaper thread, reported honestly (never assumed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SchedClass {
    /// `SCHED_FIFO` real-time scheduling granted by the kernel.
    RealTime = 1,
    /// Unprivileged fallback (`EPERM` or non-Linux target): ordinary OS thread priority.
    NormalFallback = 2,
}

/// Lock-free tripwire shared between tripwire producers and the zero-I/O reaper thread.
pub struct Tripwire {
    origin: Instant,
    last_beat_ns: AtomicU64,
    tripped_at_ns: AtomicU64,
    explicit_reason: AtomicU8,
    deadman_timeout_ns: u64,
}

impl Tripwire {
    /// Creates an armed tripwire with `deadman_timeout` (`None` disables dead-man expiry).
    #[must_use]
    pub fn new(deadman_timeout: Option<Duration>) -> Arc<Self> {
        let timeout_ns = deadman_timeout.map_or(u64::MAX, |d| {
            u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
        });
        Arc::new(Self {
            origin: Instant::now(),
            last_beat_ns: AtomicU64::new(0),
            tripped_at_ns: AtomicU64::new(0),
            explicit_reason: AtomicU8::new(0),
            deadman_timeout_ns: timeout_ns,
        })
    }

    fn now_ns(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Refreshes the dead-man heartbeat timestamp.
    pub fn beat(&self) {
        self.last_beat_ns.store(self.now_ns(), Ordering::Release);
    }

    /// Fires an immediate tripwire (`Manual`, `Suspend`, or `UmbilicalDisconnect`).
    pub fn fire(&self, reason: TripReason) {
        let now = self.now_ns().max(1);
        if self
            .explicit_reason
            .compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.tripped_at_ns.store(now, Ordering::Release);
        }
    }

    /// Evaluates whether the tripwire has fired at the current monotonic instant.
    #[must_use]
    pub fn evaluate(&self) -> Option<(TripReason, u64)> {
        let code = self.explicit_reason.load(Ordering::Acquire);
        if let Some(reason) = TripReason::from_code(code) {
            let at = self.tripped_at_ns.load(Ordering::Acquire).max(1);
            return Some((reason, at));
        }
        if self.deadman_timeout_ns != u64::MAX {
            let now = self.now_ns();
            let last = self.last_beat_ns.load(Ordering::Acquire);
            if now.saturating_sub(last) > self.deadman_timeout_ns {
                let deadline = last.saturating_add(self.deadman_timeout_ns).max(1);
                return Some((TripReason::DeadmanTimeout, deadline));
            }
        }
        None
    }
}

/// Measured outcome of one reaper trip-to-verified-wipe execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaperOutcome {
    /// Tripwire condition that fired.
    pub reason: TripReason,
    /// Thread scheduler rung achieved on this host.
    pub sched_class: SchedClass,
    /// Arenas and bytes volatile-zeroed.
    pub wipe: WipeStats,
    /// Canary read-back report post-wipe.
    pub canaries: CanaryReport,
    /// Monotonic nanoseconds from tripwire instant to completion of `SecretArenas::wipe`.
    pub trip_to_wiped_ns: u64,
    /// Monotonic nanoseconds from tripwire instant to completion of canary ghost verification.
    pub trip_to_verified_ns: u64,
}

#[cfg(target_os = "linux")]
fn promote_realtime() -> SchedClass {
    // SAFETY: `sched_param` is plain integer data and `sched_setscheduler(0, ...)` targets only
    // the calling reaper thread; `EPERM` in unprivileged containers degrades cleanly.
    unsafe {
        let param = libc::sched_param { sched_priority: 1 };
        if libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) == 0 {
            SchedClass::RealTime
        } else {
            SchedClass::NormalFallback
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn promote_realtime() -> SchedClass {
    SchedClass::NormalFallback
}

/// Background zero-I/O reaper thread watching a [`Tripwire`].
pub struct ReaperGuard {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    sched: Arc<AtomicU8>,
    handle: Option<JoinHandle<Option<ReaperOutcome>>>,
}

impl ReaperGuard {
    /// Spawns the background reaper thread polling `tripwire` every `poll_interval`.
    #[must_use]
    pub fn spawn(
        arenas: Arc<SecretArenas>,
        canaries: Arc<CanarySet>,
        tripwire: Arc<Tripwire>,
        poll_interval: Duration,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let sched = Arc::new(AtomicU8::new(0));
        let stop_bg = Arc::clone(&stop);
        let done_bg = Arc::clone(&done);
        let sched_bg = Arc::clone(&sched);

        let handle = thread::Builder::new()
            .name("holonomy-reaper".to_owned())
            .spawn(move || {
                let sched_class = promote_realtime();
                sched_bg.store(sched_class as u8, Ordering::Release);

                while !stop_bg.load(Ordering::Acquire) {
                    if let Some((reason, tripped_at_ns)) = tripwire.evaluate() {
                        let wipe = arenas.wipe();
                        let wiped_ns = tripwire.now_ns();
                        let canary_report = canaries.sample_ghosts();
                        let verified_ns = tripwire.now_ns();
                        done_bg.store(true, Ordering::Release);
                        return Some(ReaperOutcome {
                            reason,
                            sched_class,
                            wipe,
                            canaries: canary_report,
                            trip_to_wiped_ns: wiped_ns.saturating_sub(tripped_at_ns),
                            trip_to_verified_ns: verified_ns.saturating_sub(tripped_at_ns),
                        });
                    }
                    thread::sleep(poll_interval);
                }
                None
            })
            .expect("reaper thread spawn must succeed");

        // Wait briefly for the reaper thread to enter its poll loop before returning.
        while sched.load(Ordering::Acquire) == 0 {
            thread::yield_now();
        }

        Self {
            stop,
            done,
            sched,
            handle: Some(handle),
        }
    }

    /// Whether the reaper has already fired and completed verification.
    #[must_use]
    pub fn has_completed(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Scheduler class achieved by the spawned reaper thread.
    #[must_use]
    pub fn sched_class(&self) -> SchedClass {
        if self.sched.load(Ordering::Acquire) == SchedClass::RealTime as u8 {
            SchedClass::RealTime
        } else {
            SchedClass::NormalFallback
        }
    }

    /// Joins the reaper thread after a tripwire has fired and returns its [`ReaperOutcome`].
    pub fn wait_outcome(mut self) -> Option<ReaperOutcome> {
        self.handle
            .take()
            .and_then(|handle| handle.join().expect("reaper thread must not panic"))
    }
}

impl Drop for ReaperGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unarmed_canary_reports_ghost_bytes_and_armed_canary_wipes_to_zero() {
        let arenas = SecretArenas::new();
        let mut canaries = CanarySet::new(4, 256);
        assert_eq!(canaries.arm_prefix(&arenas, 3), 3);

        let stats = arenas.wipe();
        assert_eq!(stats.arenas_wiped, 3);
        assert_eq!(stats.bytes_wiped, 3 * 256);

        let report = canaries.sample_ghosts();
        assert_eq!(report.canaries_sampled, 4);
        assert_eq!(report.bytes_sampled, 4 * 256);
        assert_eq!(report.ghost_canaries, 1);
        assert_eq!(report.ghost_bytes, 256);
    }

    #[test]
    fn reaper_thread_wipes_all_armed_canaries_on_manual_tripwire() {
        let arenas = SecretArenas::new();
        let mut canaries = CanarySet::new(8, 512);
        assert_eq!(canaries.arm_all(&arenas), 8);
        let canaries = Arc::new(canaries);
        let tripwire = Tripwire::new(None);
        let guard = ReaperGuard::spawn(
            Arc::clone(&arenas),
            Arc::clone(&canaries),
            Arc::clone(&tripwire),
            Duration::from_micros(250),
        );

        tripwire.fire(TripReason::Manual);
        let outcome = guard.wait_outcome().expect("tripwire fired");
        assert_eq!(outcome.reason, TripReason::Manual);
        assert_eq!(outcome.wipe.arenas_wiped, 8);
        assert_eq!(outcome.wipe.bytes_wiped, 8 * 512);
        assert_eq!(outcome.canaries.ghost_canaries, 0);
        assert_eq!(outcome.canaries.ghost_bytes, 0);
    }
}
