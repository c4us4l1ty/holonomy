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
use core::ptr::{read_volatile, slice_from_raw_parts_mut, write_volatile, NonNull};
use core::sync::atomic::{compiler_fence, AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

/// Maximum simultaneously armed secret/slab/canary arenas (`2048` covers a full 4 MiB viewport of
/// 4 KiB slabs plus capability keys and zeroize canaries).
pub const MAX_ARENAS: usize = 2_048;

const SLOT_EMPTY: usize = 0;
const SLOT_INIT: usize = 1;
const SLOT_ARMED: usize = 2;
const SLOT_WIPING: usize = 3;
const SLOT_UNREGISTERING: usize = 4;
const SLOT_READING: usize = 5;

/// Opaque handle to one armed slot inside [`SecretArenas`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArenaSlot(usize);

impl ArenaSlot {
    /// Returns the slot's zero-based index (`0..MAX_ARENAS`).
    #[must_use]
    pub const fn index(self) -> usize {
        self.0
    }

    /// Reconstructs an [`ArenaSlot`] from a raw index when `index < MAX_ARENAS`.
    #[must_use]
    pub const fn from_index(index: usize) -> Option<Self> {
        if index < MAX_ARENAS {
            Some(Self(index))
        } else {
            None
        }
    }
}

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
    /// process terminates). No other access may overlap a wipe, and subsequent accesses must not
    /// invalidate the pointer's provenance. Slot coordination alone does not provide exclusivity.
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
                Err(SLOT_WIPING | SLOT_READING) => spin_loop(),
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
                st == SLOT_ARMED || st == SLOT_WIPING || st == SLOT_READING
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
            let acquired = loop {
                match slot.state.compare_exchange(
                    SLOT_ARMED,
                    SLOT_WIPING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break true,
                    Err(SLOT_READING) => spin_loop(),
                    Err(_) => break false,
                }
            };
            if acquired {
                let ptr = slot.address.load(Ordering::Relaxed) as *mut u8;
                let len = slot.length.load(Ordering::Relaxed);
                if !ptr.is_null() && len > 0 {
                    // SAFETY: `SLOT_WIPING` prevents `unregister` from returning until we
                    // restore `SLOT_ARMED`, so `[ptr, ptr + len)` stays live and exclusive.
                    unsafe { volatile_zero_range(ptr, len) };
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

/// Volatile-zeroes `[ptr, ptr + len)` using aligned `usize` stores for the interior and byte
/// stores for unaligned head/tail edges.
///
/// # Safety
///
/// `[ptr, ptr + len)` must be valid for writes and exclusively owned for the duration of this call.
unsafe fn volatile_zero_range(ptr: *mut u8, len: usize) {
    let word = core::mem::size_of::<usize>();
    let align_off = ptr.align_offset(word).min(len);
    for idx in 0..align_off {
        // SAFETY: `idx < align_off <= len`.
        unsafe { write_volatile(ptr.add(idx), 0u8) };
    }
    let rem = len - align_off;
    let words = rem / word;
    // SAFETY: `ptr.add(align_off)` is in `[ptr, ptr + len]` and `usize`-aligned if `words > 0`.
    let word_ptr = unsafe { ptr.add(align_off).cast::<usize>() };
    for idx in 0..words {
        // SAFETY: `idx < words`, within `[ptr, ptr + len)`.
        unsafe { write_volatile(word_ptr.add(idx), 0usize) };
    }
    let tail_start = align_off + words * word;
    for idx in tail_start..len {
        // SAFETY: `idx < len`, within `[ptr, ptr + len)`.
        unsafe { write_volatile(ptr.add(idx), 0u8) };
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
    ptr: NonNull<u8>,
    len: usize,
    armed: Option<(Arc<SecretArenas>, ArenaSlot)>,
}

// SAFETY: `ptr` owns a heap allocation of `len` bytes mutated only while holding `&mut Self`
// before arming, during `SecretArenas::wipe` (coordinated by atomic slot state), or in `Drop`
// after `unregister` completes; concurrent inspection uses `read_volatile`.
unsafe impl Send for CanarySlab {}
unsafe impl Sync for CanarySlab {}

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
        let raw = Box::into_raw(vec.into_boxed_slice()).cast::<u8>();
        let ptr = NonNull::new(raw).expect("Box::into_raw is non-null");
        Self {
            ptr,
            len,
            armed: None,
        }
    }

    fn arm(&mut self, arenas: &Arc<SecretArenas>) -> bool {
        if self.armed.is_some() || self.len == 0 {
            return false;
        }
        // SAFETY: `self.ptr` has raw-pointer provenance over `self.len` writable bytes unaffected
        // by moving `Self`, and `Drop` unregisters `slot` before zeroizing and freeing `self.ptr`.
        if let Some(slot) = unsafe { arenas.register(self.ptr.as_ptr(), self.len) } {
            self.armed = Some((Arc::clone(arenas), slot));
            true
        } else {
            false
        }
    }

    fn ghost_bytes(&self) -> usize {
        let slot = self
            .armed
            .as_ref()
            .map(|(arenas, handle)| &arenas.slots[handle.0]);
        if let Some(slot) = slot {
            loop {
                match slot.state.compare_exchange(
                    SLOT_ARMED,
                    SLOT_READING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(SLOT_WIPING | SLOT_READING) => spin_loop(),
                    Err(_) => unreachable!("a live canary remains registered"),
                }
            }
        }
        let ptr = self.ptr.as_ptr().cast_const();
        let len = self.len;
        let mut ghosts = 0usize;
        for offset in 0..len {
            // SAFETY: `self.ptr` owns `len` initialized bytes.
            let b = unsafe { read_volatile(ptr.add(offset)) };
            if b != 0 {
                ghosts += 1;
            }
        }
        if let Some(slot) = slot {
            slot.state.store(SLOT_ARMED, Ordering::Release);
        }
        ghosts
    }
}

impl Drop for CanarySlab {
    fn drop(&mut self) {
        if let Some((arenas, slot)) = self.armed.take() {
            arenas.unregister(slot);
        }
        if self.len > 0 {
            // SAFETY: `self.ptr` owns `self.len` writable bytes and `unregister` completed above.
            unsafe { volatile_zero_range(self.ptr.as_ptr(), self.len) };
            compiler_fence(Ordering::SeqCst);
        }
        let raw = slice_from_raw_parts_mut(self.ptr.as_ptr(), self.len);
        // SAFETY: `raw` reconstructs the exact `Box<[u8]>` turned into raw parts in `Self::new`.
        unsafe { drop(Box::from_raw(raw)) };
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

    /// How many canary buffers this set holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slabs.len()
    }

    /// Whether this canary set holds no buffers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slabs.is_empty()
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
            bytes_sampled = bytes_sampled.saturating_add(slab.len);
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
    // Timestamp in the upper bits, reason in the low three bits. One atomic publication keeps
    // concurrent producers from pairing one reason with another producer's timestamp.
    trip: AtomicU64,
    deadman_timeout_ns: u64,
    reaper_thread: OnceLock<Thread>,
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
            trip: AtomicU64::new(0),
            deadman_timeout_ns: timeout_ns,
            reaper_thread: OnceLock::new(),
        })
    }

    fn now_ns(&self) -> u64 {
        // Reserve MAX as the heartbeat state meaning an expiry is being published.
        u64::try_from(self.origin.elapsed().as_nanos())
            .unwrap_or(u64::MAX - 1)
            .min(u64::MAX - 1)
    }

    // Claim an expired heartbeat before publishing the trip. The CAS makes expiry and a
    // concurrent heartbeat mutually exclusive: a late beat cannot revive an expired timer.
    fn latch_expired(&self, last: u64) -> bool {
        if self
            .last_beat_ns
            .compare_exchange(last, u64::MAX, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let deadline = last.saturating_add(self.deadman_timeout_ns).max(1);
        self.fire_at(TripReason::DeadmanTimeout, deadline);
        true
    }

    /// Refreshes the dead-man heartbeat timestamp, unless the deadline has already expired.
    pub fn beat(&self) {
        loop {
            if self.trip.load(Ordering::Acquire) != 0 {
                return;
            }
            let last = self.last_beat_ns.load(Ordering::Acquire);
            if last == u64::MAX {
                spin_loop();
                continue;
            }
            let now = self.now_ns();
            if self.deadman_timeout_ns != u64::MAX
                && now.saturating_sub(last) > self.deadman_timeout_ns
            {
                if self.latch_expired(last) {
                    return;
                }
            } else if self
                .last_beat_ns
                .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    /// Fires an immediate tripwire (`Manual`, `Suspend`, or `UmbilicalDisconnect`).
    pub fn fire(&self, reason: TripReason) {
        self.fire_at(reason, self.now_ns().max(1));
    }

    fn fire_at(&self, reason: TripReason, at: u64) {
        // Three low bits hold the reason. A timestamp beyond roughly 73 years saturates.
        let packed = (at.clamp(1, u64::MAX >> 3) << 3) | u64::from(reason as u8);
        let _ = self
            .trip
            .compare_exchange(0, packed, Ordering::AcqRel, Ordering::Acquire);
        if let Some(watcher) = self.reaper_thread.get() {
            watcher.unpark();
        }
    }

    /// Evaluates whether the tripwire has fired at the current monotonic instant. Once a
    /// dead-man deadline expires, observing it permanently latches the trip.
    #[must_use]
    pub fn evaluate(&self) -> Option<(TripReason, u64)> {
        loop {
            let trip = self.trip.load(Ordering::Acquire);
            if let Some(reason) = TripReason::from_code((trip & 7) as u8) {
                return Some((reason, trip >> 3));
            }
            let last = self.last_beat_ns.load(Ordering::Acquire);
            if last == u64::MAX {
                // The winner of the expiry CAS is publishing the reason.
                spin_loop();
                continue;
            }
            if self.deadman_timeout_ns == u64::MAX
                || self.now_ns().saturating_sub(last) <= self.deadman_timeout_ns
            {
                return None;
            }
            if self.latch_expired(last) {
                continue;
            }
        }
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
    pending: Arc<AtomicBool>,
    permission: Option<Arc<AtomicBool>>,
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
        Self::spawn_inner(arenas, canaries, tripwire, poll_interval, false)
    }

    /// Starts a reaper which waits for the editor to acknowledge quiescence before wiping.
    ///
    /// This trades immediate wiping for memory safety: a stuck editor cannot acknowledge a trip,
    /// so this mode does not by itself satisfy the L-7 deadline.
    #[must_use]
    pub fn spawn_cooperative(
        arenas: Arc<SecretArenas>,
        canaries: Arc<CanarySet>,
        tripwire: Arc<Tripwire>,
        poll_interval: Duration,
    ) -> Self {
        Self::spawn_inner(arenas, canaries, tripwire, poll_interval, true)
    }

    fn spawn_inner(
        arenas: Arc<SecretArenas>,
        canaries: Arc<CanarySet>,
        tripwire: Arc<Tripwire>,
        poll_interval: Duration,
        cooperative: bool,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(AtomicBool::new(false));
        let permission = cooperative.then(|| Arc::new(AtomicBool::new(false)));
        let sched = Arc::new(AtomicU8::new(0));
        let stop_bg = Arc::clone(&stop);
        let done_bg = Arc::clone(&done);
        let pending_bg = Arc::clone(&pending);
        let permission_bg = permission.clone();
        let sched_bg = Arc::clone(&sched);

        let handle = thread::Builder::new()
            .name("holonomy-reaper".to_owned())
            .spawn(move || {
                let sched_class = promote_realtime();
                let _ = tripwire.reaper_thread.set(thread::current());
                sched_bg.store(sched_class as u8, Ordering::Release);

                while !stop_bg.load(Ordering::Acquire) {
                    if let Some((reason, tripped_at_ns)) = tripwire.evaluate() {
                        pending_bg.store(true, Ordering::Release);
                        if let Some(permission) = &permission_bg {
                            while !permission.load(Ordering::Acquire) {
                                if stop_bg.load(Ordering::Acquire) {
                                    return None;
                                }
                                thread::park();
                            }
                        }
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
                    thread::park_timeout(poll_interval);
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
            pending,
            permission,
            sched,
            handle: Some(handle),
        }
    }

    /// Whether the watcher has latched a trip, even if a later heartbeat arrives.
    #[must_use]
    pub fn has_tripped(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    /// Allows a cooperative reaper to proceed after all registered allocations are quiescent.
    ///
    /// # Safety
    ///
    /// All registered allocations and canaries must remain exclusively available to the reaper
    /// until `wait_outcome` returns. No thread may read, write, register, or free them meanwhile.
    pub unsafe fn acknowledge_quiescence(&self) {
        if let Some(permission) = &self.permission {
            permission.store(true, Ordering::Release);
            if let Some(handle) = &self.handle {
                handle.thread().unpark();
            }
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
            handle.thread().unpark();
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

    #[test]
    fn canary_sampling_can_overlap_a_reaper_wipe() {
        let arenas = SecretArenas::new();
        let mut canaries = CanarySet::new(16, 1024);
        assert_eq!(canaries.arm_all(&arenas), 16);
        let canaries = Arc::new(canaries);
        let sampled = Arc::clone(&canaries);
        let reader = thread::spawn(move || {
            for _ in 0..200 {
                assert_eq!(sampled.sample_ghosts().canaries_sampled, 16);
            }
        });
        let tripwire = Tripwire::new(None);
        let guard = ReaperGuard::spawn(
            Arc::clone(&arenas),
            Arc::clone(&canaries),
            Arc::clone(&tripwire),
            Duration::from_millis(1),
        );
        tripwire.fire(TripReason::Manual);
        let outcome = guard.wait_outcome().unwrap();
        reader.join().unwrap();
        assert_eq!(outcome.wipe.arenas_wiped, 16);
        assert_eq!(outcome.canaries.ghost_bytes, 0);
        assert_eq!(canaries.sample_ghosts().ghost_bytes, 0);
    }

    #[test]
    fn cooperative_reaper_waits_for_editor_quiescence() {
        let arenas = SecretArenas::new();
        let mut canaries = CanarySet::new(2, 128);
        assert_eq!(canaries.arm_all(&arenas), 2);
        let canaries = Arc::new(canaries);
        let mut editor_bytes = vec![42u8; 256];
        let tripwire = Tripwire::new(None);
        let guard = ReaperGuard::spawn_cooperative(
            Arc::clone(&arenas),
            Arc::clone(&canaries),
            Arc::clone(&tripwire),
            Duration::from_millis(1),
        );
        tripwire.fire(TripReason::Manual);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !guard.has_tripped() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(guard.has_tripped());
        assert!(!guard.has_completed());
        // Editing remains safe until the single owner explicitly yields the allocations.
        editor_bytes[0] = 17;
        assert_eq!(canaries.sample_ghosts().ghost_canaries, 2);
        // SAFETY: register a fresh pointer only after editing; the allocation remains live and
        // untouched until the reaper has joined and the slot is unregistered.
        let slot =
            unsafe { arenas.register(editor_bytes.as_mut_ptr(), editor_bytes.len()) }.unwrap();
        // SAFETY: this thread stops touching registered buffers and canaries until join returns.
        unsafe { guard.acknowledge_quiescence() };
        let result = guard.wait_outcome().unwrap();
        assert_eq!(result.canaries.ghost_bytes, 0);
        arenas.unregister(slot);
        assert!(editor_bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn deadman_expiry_survives_a_late_heartbeat_and_wakes_reaper() {
        let arenas = SecretArenas::new();
        let mut canaries = CanarySet::new(2, 128);
        assert_eq!(canaries.arm_all(&arenas), 2);
        let canaries = Arc::new(canaries);
        let tripwire = Tripwire::new(Some(Duration::from_millis(2)));
        tripwire.beat();
        // Park long enough to ensure the UI, rather than the worker, observes expiry first.
        let guard = ReaperGuard::spawn_cooperative(
            arenas,
            Arc::clone(&canaries),
            Arc::clone(&tripwire),
            Duration::from_secs(10),
        );
        thread::sleep(Duration::from_millis(10));
        assert_eq!(tripwire.evaluate().unwrap().0, TripReason::DeadmanTimeout);
        tripwire.beat();
        assert_eq!(tripwire.evaluate().unwrap().0, TripReason::DeadmanTimeout);
        // SAFETY: no other thread accesses the registered canaries until the join completes.
        unsafe { guard.acknowledge_quiescence() };
        let outcome = guard.wait_outcome().unwrap();
        assert_eq!(outcome.reason, TripReason::DeadmanTimeout);
        assert_eq!(outcome.canaries.ghost_bytes, 0);
    }

    #[test]
    fn racing_late_heartbeat_and_observer_cannot_clear_expiry() {
        for _ in 0..16 {
            let tripwire = Tripwire::new(Some(Duration::from_millis(1)));
            tripwire.beat();
            thread::sleep(Duration::from_millis(4));
            let other = Arc::clone(&tripwire);
            let heartbeat = thread::spawn(move || other.beat());
            assert_eq!(tripwire.evaluate().unwrap().0, TripReason::DeadmanTimeout);
            heartbeat.join().unwrap();
            assert_eq!(tripwire.evaluate().unwrap().0, TripReason::DeadmanTimeout);
        }
    }

    #[test]
    fn late_beat_alone_latches_expired_deadman() {
        let tripwire = Tripwire::new(Some(Duration::from_millis(2)));
        tripwire.beat();
        thread::sleep(Duration::from_millis(10));
        tripwire.beat();
        assert_eq!(tripwire.evaluate().unwrap().0, TripReason::DeadmanTimeout);
    }

    #[test]
    fn concurrent_trip_producers_publish_a_matching_reason_and_timestamp() {
        for _ in 0..128 {
            let tripwire = Tripwire::new(None);
            let start = Arc::new(std::sync::Barrier::new(3));
            let a = Arc::clone(&tripwire);
            let a_start = Arc::clone(&start);
            let first = thread::spawn(move || {
                a_start.wait();
                a.fire_at(TripReason::Manual, 11);
            });
            let b = Arc::clone(&tripwire);
            let b_start = Arc::clone(&start);
            let second = thread::spawn(move || {
                b_start.wait();
                b.fire_at(TripReason::Suspend, 22);
            });
            start.wait();
            first.join().unwrap();
            second.join().unwrap();
            assert!(matches!(
                tripwire.evaluate(),
                Some((TripReason::Manual, 11) | (TripReason::Suspend, 22))
            ));
        }
    }

    #[test]
    fn tripwire_fire_publishes_real_timestamp_to_concurrent_evaluator() {
        for _ in 0..64 {
            let tripwire = Tripwire::new(None);
            thread::sleep(Duration::from_micros(50));
            let min_expected = tripwire.now_ns();
            let worker_tripwire = Arc::clone(&tripwire);
            let evaluator = thread::spawn(move || loop {
                if let Some((reason, at)) = worker_tripwire.evaluate() {
                    return (reason, at);
                }
                spin_loop();
            });
            tripwire.fire(TripReason::Suspend);
            let (reason, at) = evaluator.join().expect("evaluator joined");
            assert_eq!(reason, TripReason::Suspend);
            assert!(
                at >= min_expected,
                "observed clamped timestamp {at} < {min_expected}"
            );
        }
    }

    #[test]
    fn idle_reaper_parks_without_polling_and_wakes_on_fire_or_drop() {
        let arenas = SecretArenas::new();
        let canaries = Arc::new(CanarySet::new(2, 256));
        let tripwire = Tripwire::new(None);
        let guard = ReaperGuard::spawn(
            Arc::clone(&arenas),
            Arc::clone(&canaries),
            Arc::clone(&tripwire),
            Duration::from_secs(3_600),
        );
        let started = Instant::now();
        drop(guard);
        assert!(started.elapsed() < Duration::from_secs(1));

        let tripwire = Tripwire::new(None);
        let guard = ReaperGuard::spawn(
            Arc::clone(&arenas),
            Arc::clone(&canaries),
            Arc::clone(&tripwire),
            Duration::from_secs(3_600),
        );
        let started = Instant::now();
        tripwire.fire(TripReason::Manual);
        let outcome = guard.wait_outcome().expect("tripwire fired");
        assert_eq!(outcome.reason, TripReason::Manual);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
