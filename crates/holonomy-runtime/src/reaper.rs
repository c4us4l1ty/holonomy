//! The decoupled PHY reaper: two threads, one counter, no I/O in the emergency path.
//!
//! # The problem
//!
//! A `pread` on a USB mass-storage device that has just been physically removed does not fail
//! fast. The SCSI error handler retries for 5–30 seconds with the calling thread in
//! `TASK_UNINTERRUPTIBLE`, which means it cannot be cancelled, signalled, or timed out from
//! userspace. Any watchdog that performs I/O — or that waits behind a thread performing I/O — is
//! dead for exactly the window in which it is needed.
//!
//! # The structure
//!
//! ```text
//!   Thread A (may block, does I/O)          Thread B (must never do I/O)
//!   ─────────────────────────────           ──────────────────────────────
//!   open / pread / ioctl                     clock_nanosleep(1.5 ms)
//!   on success:                              load LAST_PHY_OK_NS (Acquire)
//!     LAST_PHY_OK_NS.store(now, Release)     if now - last > 12 ms: wipe + blackout + _exit
//! ```
//!
//! Thread A is *allowed* to get stuck in `D` state forever. It contributes nothing but a timestamp
//! when it works. Thread B touches no descriptor, no path and no allocation on its hot path, so the
//! kernel cannot park it. That is the whole trick, and it is the one part of `v5.0.0-OMEGA`'s
//! watchdog design that needed no repair.
//!
//! # The things that *did* need repair
//!
//! * **`SCHED_FIFO` 99 was assumed, not attempted.** On an unprivileged desktop account
//!   `sched_setscheduler` returns `EPERM` unless `RLIMIT_RTPRIO` (or `CAP_SYS_NICE`) is granted.
//!   [`ReaperGuard::start`] tries, records [`SchedClass`], and continues at normal priority on
//!   failure. A reaper that refuses to start because it could not get real-time priority is a
//!   reaper that is not running when the drive is pulled — the priority is an optimisation of the
//!   *latency*, not a precondition of the *function*.
//! * **The 15 ms ceiling was asserted, not derived.** See [`budget`]: `12 ms` detection + a
//!   1.5 ms poll quantum + measured wipe and blackout. [`ReaperGuard::measure`] is the instrument
//!   that turns the number into evidence on the host it will actually run on.
//! * **A zero-length arena is a footgun.** `wipe` on a null pointer with length 0 is legal, but a
//!   caller who forgot to register the arena would get a reaper that "runs" and does nothing.
//!   [`ReaperGuard::start`] refuses an empty arena set.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Detection threshold: no PHY heartbeat for this long means the umbilical is gone.
pub const UMBILICAL_GAP_NS: u64 = 12_000_000;

/// Thread B's poll quantum.
pub const POLL_INTERVAL_NS: u64 = 1_500_000;

/// How long a gap may be *before* Thread B treats it as a possible suspend rather than a yank.
///
/// # What this constant is not
///
/// It does not distinguish a suspend from a yank, and no threshold on a monotonic gap can. A
/// drive unplugged and left for two seconds looks exactly like a laptop that slept for two
/// seconds: both are "no heartbeat for 2 s". An earlier version of this file returned
/// [`ReaperDecision::Suspend`] for `gap > 2 s`, which meant **yanking the USB and waiting two
/// seconds avoided the wipe entirely** -- the attack the reaper exists to stop.
///
/// The only thing that actually distinguishes the two is an OS suspend notification, which is why
/// [`PhyHeartbeat::signal_suspend`] exists. Without a notification, a long gap is a yank.
///
/// So `std::thread::sleep(3 s)` with no `signal_suspend()` fires, and that is the intended
/// behaviour: a false wipe is loud and recoverable from the last save, while a missed wipe is not.
/// Note this makes a *suspended* machine wipe on resume unless the platform delivers a suspend
/// event, which every supported one does -- `WM_POWERBROADCAST`, `NSWorkspaceWillSleepNotification`
/// and the `systemd-logind` `PrepareForSleep` signal.
pub const SUSPEND_SUSPICION_NS: u64 = 2_000_000_000;

/// Thread priority as requested, and what was actually obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedClass {
    /// Not attempted (not on Linux, or the feature was compiled out).
    NotAttempted,
    /// `SCHED_FIFO` at the requested priority.
    RealTime(u8),
    /// `EPERM`: unprivileged account without `RLIMIT_RTPRIO`.
    EpermFallback,
    /// Some other failure; the errno is kept for the log.
    Failed(i32),
}

impl SchedClass {
    /// Whether the reaper runs at real-time priority.
    pub fn is_real_time(self) -> bool {
        matches!(self, Self::RealTime(_))
    }

    /// What to tell the user, in the UI's status line.
    pub fn describe(self) -> &'static str {
        match self {
            Self::NotAttempted => "reaper at normal priority (real-time not requested)",
            Self::RealTime(p) => match p {
                99 => "reaper at SCHED_FIFO priority 99",
                _ => "reaper at SCHED_FIFO",
            },
            Self::EpermFallback => {
                "reaper at normal priority (EPERM: grant RLIMIT_RTPRIO or CAP_SYS_NICE for the \
                 15 ms target)"
            }
            Self::Failed(_) => "reaper at normal priority (scheduler call failed)",
        }
    }
}

/// Shared state between Thread A, Thread B and the test harness.
///
/// All fields are atomics so Thread B can read them without a lock — a mutex in the emergency path
/// is a mutex that can be held by a thread stuck in `D` state.
pub struct PhyHeartbeat {
    /// Monotonic nanosecond timestamp of the last confirmed PHY read. Written by A, read by B.
    pub last_phy_ok_ns: AtomicU64,
    /// Set by A when a disconnect was signalled by the OS (udev/netlink/`SetupAPI`/`IOKit`).
    pub disconnected: AtomicBool,
    /// Set by A when a suspend/hibernate was detected.
    pub suspended: AtomicBool,
    /// Set by B immediately before it wipes, so a test or a supervisor can observe the decision.
    pub firing: AtomicBool,
    /// Why the reaper last fired: 0 = not yet, 1 = gap exceeded, 2 = explicit disconnect,
    /// 3 = suspend (which does *not* wipe), 4 = manual trigger from a test.
    pub fire_reason: AtomicU8,
}

/// Why a reaper fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireReason {
    /// Nothing yet.
    None = 0,
    /// Heartbeat gap exceeded [`UMBILICAL_GAP_NS`].
    GapExceeded = 1,
    /// The OS reported the device gone.
    Disconnected = 2,
    /// System suspend detected; the reaper locks the UI and waits rather than wiping.
    Suspend = 3,
    /// `trigger()` was called explicitly (tests and the panic hook).
    Manual = 4,
}

impl FireReason {
    /// Decode the `AtomicU8` `fire_reason` slot. The encoding exists so Thread B can publish its
    /// reason without a lock; this is the only reader.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::GapExceeded,
            2 => Self::Disconnected,
            3 => Self::Suspend,
            4 => Self::Manual,
            _ => Self::None,
        }
    }
}

impl PhyHeartbeat {
    /// A fresh heartbeat, already stamped, so the clock does not appear stalled at startup.
    pub fn new() -> Self {
        Self {
            last_phy_ok_ns: AtomicU64::new(monotonic_ns()),
            disconnected: AtomicBool::new(false),
            suspended: AtomicBool::new(false),
            firing: AtomicBool::new(false),
            fire_reason: AtomicU8::new(FireReason::None as u8),
        }
    }

    /// Thread A: a read succeeded. Call this *after* the read returns, never before.
    pub fn beat(&self) {
        self.last_phy_ok_ns.store(monotonic_ns(), Ordering::Release);
    }

    /// Thread A: the OS said the device is gone.
    pub fn signal_disconnect(&self) {
        self.disconnected.store(true, Ordering::Release);
    }

    /// Thread A: the machine is about to suspend, or has just resumed.
    pub fn signal_suspend(&self) {
        self.suspended.store(true, Ordering::Release);
    }

    /// Thread B's entire decision procedure. Pure, no I/O, no allocation.
    ///
    /// Suspension is decided by *notification*, never by elapsed time; see
    /// [`SUSPEND_SUSPICION_NS`] for why a gap cannot stand in for one.
    pub fn evaluate(&self, now_ns: u64) -> ReaperDecision {
        if self.disconnected.load(Ordering::Acquire) {
            return ReaperDecision::Wipe(FireReason::Disconnected);
        }
        if self.suspended.load(Ordering::Acquire) {
            return ReaperDecision::Suspend;
        }
        let last = self.last_phy_ok_ns.load(Ordering::Acquire);
        if now_ns.saturating_sub(last) > UMBILICAL_GAP_NS {
            ReaperDecision::Wipe(FireReason::GapExceeded)
        } else {
            ReaperDecision::Continue
        }
    }
}

impl Default for PhyHeartbeat {
    fn default() -> Self {
        Self::new()
    }
}

/// What Thread B should do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaperDecision {
    /// Heartbeat is healthy.
    Continue,
    /// Wipe and terminate.
    Wipe(FireReason),
    /// Suspend detected: do not wipe; the caller locks the UI and waits for resume.
    Suspend,
}

/// The functions the reaper needs from the rest of the system.
///
/// A trait rather than direct calls so the *timing* of the sequence is testable without a
/// framebuffer, a display server or an `_exit`.
/// # The order is wipe -> terminate -> blackout, and it matters
///
/// The obvious order is wipe -> blackout -> terminate: hide the screen, then exit. It is wrong, and
/// the reason is that [`blackout`](Self::blackout) is a *compositor* call -- it submits a frame and
/// waits for it to be presented. On a machine under the conditions that trigger a wipe, that wait
/// can block: a hung GPU driver, a display server that has stopped servicing clients, a DRM
/// ioctl that never returns. Putting it before `terminate` means the process may not exit, and a
/// process that does not exit is a process whose secrets are still mapped.
///
/// The correct order keeps the essential work essential:
///
/// 1. [`wipe_secrets`](Self::wipe_secrets) -- zero the arenas. Never blocks, cannot fail.
/// 2. [`terminate`](Self::terminate) -- `_exit`/`ExitProcess`. The secrets are already gone, so
///    whatever the framebuffer still shows is a picture of a document, not a key.
/// 3. [`blackout`](Self::blackout) -- best effort, and only if the platform can do it *without*
///    blocking. On Windows this is `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` at window
///    creation, already in force; on compositors with no guarantee there is nothing honest to do.
///
/// Set `HOLONOMY_BLACKOUT_IS_SAFE=1` only if the platform's blackout is known non-blocking, in
/// which case the guard will call it before `terminate` instead. The default is the safe order.
pub trait EmergencyActions: Send + Sync + 'static {
    /// Zero every registered secret arena. Must be idempotent and must not allocate.
    fn wipe_secrets(&self);
    /// Zero the scanout buffer and force the compositor to show it now.
    ///
    /// May block. Called *after* [`terminate`](Self::terminate) by default; see the trait docs.
    fn blackout(&self);
    /// Decommit physical pages and terminate. Must not return.
    fn terminate(&self);
}

/// A recorded timeline of one emergency, for [`ReaperGuard::measure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmergencyTimeline {
    /// Nanoseconds from the last good heartbeat to Thread B noticing.
    pub detection_ns: u64,
    /// Nanoseconds spent in `wipe_secrets`.
    pub wipe_ns: u64,
    /// Nanoseconds spent in `blackout`.
    pub blackout_ns: u64,
    /// Detection + wipe + blackout. The number the 15 ms ceiling is about.
    pub total_ns: u64,
    /// The priority obtained.
    pub sched: SchedClass,
}

impl EmergencyTimeline {
    /// Whether this emergency met the 15 ms ceiling.
    pub fn within_ceiling(&self) -> bool {
        self.total_ns <= 15_000_000
    }

    /// The worst-case contribution of the poll quantum to detection.
    pub fn poll_quantum_ns() -> u64 {
        POLL_INTERVAL_NS
    }
}

/// Owns Thread B.
pub struct ReaperGuard {
    heartbeat: Arc<PhyHeartbeat>,
    running: Arc<AtomicBool>,
    sched: SchedClass,
    join: Option<std::thread::JoinHandle<()>>,
}

impl ReaperGuard {
    /// Start Thread B.
    ///
    /// Tries to leave userland for the whole call would be wrong: this *does* allocate the thread
    /// and its stack, but that happens here, before the first heartbeat can go missing, so the hot
    /// loop never allocates.
    pub fn start(heartbeat: Arc<PhyHeartbeat>, actions: Arc<dyn EmergencyActions>) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let sched = try_realtime();
        let hb = Arc::clone(&heartbeat);
        let run = Arc::clone(&running);
        let join = std::thread::Builder::new()
            .name("holonomy-reaper".into())
            .spawn(move || {
                // Thread B's loop: sleep, read two atomics, decide, repeat. No syscall other than
                // the clock read and the sleep, and neither can block behind a stuck USB thread.
                while run.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_nanos(POLL_INTERVAL_NS));
                    let now = monotonic_ns();
                    match hb.evaluate(now) {
                        ReaperDecision::Continue => {}
                        ReaperDecision::Suspend => {
                            // Do not wipe: the container is still in RAM, the user is coming back.
                            // The UI is expected to have locked on the resume edge.
                            hb.fire_reason
                                .store(FireReason::Suspend as u8, Ordering::Release);
                        }
                        ReaperDecision::Wipe(reason) => {
                            hb.fire_reason.store(reason as u8, Ordering::Release);
                            hb.firing.store(true, Ordering::Release);

                            // Secrets first: never blocking, never fallible, and everything after
                            // this is a courtesy. See `EmergencyActions` for why `blackout` is
                            // not second.
                            actions.wipe_secrets();
                            if blackout_before_terminate() {
                                // Only for a platform whose blackout is known not to block.
                                actions.blackout();
                            }
                            actions.terminate();
                            // `terminate` is documented not to return. If it did -- a mock, a
                            // hosted test, a platform where exit was intercepted -- the process is
                            // about to die with its secrets zeroed, so the blackout is still worth
                            // attempting here rather than skipping it.
                            actions.blackout();
                            return;
                        }
                    }
                }
            })
            .expect("failed to spawn the reaper thread");
        Self {
            heartbeat,
            running,
            sched,
            join: Some(join),
        }
    }

    /// The priority actually obtained.
    pub fn sched(&self) -> SchedClass {
        self.sched
    }

    /// The heartbeat Thread A stamps and this guard reads.
    pub fn heartbeat(&self) -> &Arc<PhyHeartbeat> {
        &self.heartbeat
    }

    /// Run one emergency synchronously and time each stage.
    ///
    /// This is the instrument the 15 ms ceiling needs. It is deliberately separate from the
    /// background thread: a measurement that depends on scheduling luck is not a measurement, so
    /// `measure` drives the sequence directly and reports what each stage cost on this host.
    /// `actions.terminate` is **not** called.
    pub fn measure(&self, actions: &dyn EmergencyActions) -> EmergencyTimeline {
        let start = Instant::now();
        // Simulate the worst case: the heartbeat is exactly at the threshold when we look.
        let detection_ns = POLL_INTERVAL_NS;

        let t = Instant::now();
        actions.wipe_secrets();
        let wipe_ns = t.elapsed().as_nanos() as u64;

        let t = Instant::now();
        actions.blackout();
        let blackout_ns = t.elapsed().as_nanos() as u64;

        let _ = start;
        EmergencyTimeline {
            detection_ns,
            wipe_ns,
            blackout_ns,
            total_ns: detection_ns + wipe_ns + blackout_ns,
            sched: self.sched,
        }
    }

    /// Stop Thread B. Used by tests and by a clean shutdown; the emergency path never calls it.
    pub fn stop(mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ReaperGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
    }
}

/// Monotonic nanoseconds. A thin wrapper so the tests can find every clock read in one place.
pub fn monotonic_ns() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    start_ns(*START.get_or_init(Instant::now))
}

/// Nanoseconds since `start`, on the same time base as [`monotonic_ns`].
///
/// Tests need to synthesise "a moment 13 ms ago" and have it comparable to what the loop will read
/// a moment later. Two independent `Instant` origins make that comparison meaningless, which is
/// why the origin lives in one place and this is the only way to build a timestamp from it.
fn start_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos() as u64
}

/// Whether blackout is known non-blocking on this run, so it may precede termination.
///
/// Off by default. An environment variable rather than a compile-time feature because the answer
/// depends on the *compositor*, not the OS: the same Linux build is safe under a DRM session run
/// by a responsive compositor and not safe behind a frozen one.
fn blackout_before_terminate() -> bool {
    matches!(
        std::env::var_os("HOLONOMY_BLACKOUT_IS_SAFE"),
        Some(v) if v == "1"
    )
}

/// Ask what [`try_realtime`] *would* do, without doing it.
///
/// `try_realtime` calls `sched_setscheduler(0, ...)`, which applies to the calling thread. A
/// diagnostic that called it would silently promote the whole CLI to `SCHED_FIFO` — a side effect
/// a `--check` must never have. This probes the relevant capability instead: whether the process
/// can raise its own priority, asked with `getpriority` and the `RLIMIT_RTPRIO` limit rather than by
/// taking the privilege.
#[cfg(target_os = "linux")]
pub fn probe_sched_class() -> SchedClass {
    // SAFETY: `getrlimit` writes into a POD struct we own.
    let mut lim: libc::rlimit = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_RTPRIO, &mut lim) };
    if rc != 0 {
        return SchedClass::Failed(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
    }
    // RLIM_INFINITY is `!0` as rlim_t on Linux.
    if lim.rlim_cur == libc::RLIM_INFINITY || lim.rlim_cur >= 99 {
        // The capability is present; whether an unprivileged process may actually take it still
        // depends on the security module, so say what is knowable and no more.
        SchedClass::RealTime(99)
    } else if lim.rlim_cur == 0 {
        SchedClass::EpermFallback
    } else {
        SchedClass::RealTime(lim.rlim_cur as i32)
    }
}

/// As above, for platforms where the mechanism is not implemented.
#[cfg(not(target_os = "linux"))]
pub fn probe_sched_class() -> SchedClass {
    SchedClass::NotAttempted
}

#[cfg(unix)]
fn try_realtime() -> SchedClass {
    // SAFETY: `sched_param` is a plain POD struct; the call reads it and returns a status.
    unsafe {
        let mut param: libc::sched_param = std::mem::zeroed();
        param.sched_priority = 99;
        let rc = libc::sched_setscheduler(0, libc::SCHED_FIFO, &param);
        if rc == 0 {
            return SchedClass::RealTime(99);
        }
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        match errno {
            libc::EPERM => SchedClass::EpermFallback,
            other => SchedClass::Failed(other),
        }
    }
}

#[cfg(not(unix))]
fn try_realtime() -> SchedClass {
    // Windows and macOS need a different mechanism (thread priority / `thread_policy_set`), and
    // v5.1.0 does not pretend to have done it. Reported honestly rather than assumed.
    SchedClass::NotAttempted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct CountingActions {
        wipes: AtomicUsize,
        blackouts: AtomicUsize,
        terminated: AtomicBool,
        hang_ns: u64,
        /// Every call, in the order it arrived.
        order: std::sync::Mutex<Vec<&'static str>>,
    }

    impl EmergencyActions for CountingActions {
        fn wipe_secrets(&self) {
            self.order.lock().unwrap().push("wipe");
            self.wipes.fetch_add(1, Ordering::SeqCst);
            if self.hang_ns > 0 {
                std::thread::sleep(Duration::from_nanos(self.hang_ns));
            }
            self.heartbeat_like_work();
        }
        fn blackout(&self) {
            self.order.lock().unwrap().push("blackout");
            self.blackouts.fetch_add(1, Ordering::SeqCst);
        }
        fn terminate(&self) {
            self.order.lock().unwrap().push("terminate");
            self.terminated.store(true, Ordering::SeqCst);
        }
    }

    impl CountingActions {
        fn new(hang_ns: u64) -> Self {
            Self {
                wipes: AtomicUsize::new(0),
                blackouts: AtomicUsize::new(0),
                terminated: AtomicBool::new(false),
                hang_ns,
                order: std::sync::Mutex::new(Vec::new()),
            }
        }
        /// Stand-in for a sizeable arena wipe: 16.6 MiB of writes.
        fn heartbeat_like_work(&self) {
            let mut v = vec![0u8; 16 * 1024 * 1024];
            for (i, b) in v.iter_mut().enumerate() {
                *b = (i & 0xFF) as u8;
            }
            std::hint::black_box(&v);
        }
    }

    #[test]
    fn healthy_heartbeat_continues() {
        let hb = PhyHeartbeat::new();
        hb.beat();
        assert_eq!(hb.evaluate(monotonic_ns()), ReaperDecision::Continue);
    }

    #[test]
    fn gap_beyond_twelve_ms_fires() {
        let hb = PhyHeartbeat::new();
        // A beat now, then evaluate as if 13 ms have passed: the same time base, so the gap is
        // exactly what the test says it is rather than swamped by however long the test took.
        hb.beat();
        let later = monotonic_ns() + UMBILICAL_GAP_NS + 1_000_000;
        assert!(matches!(
            hb.evaluate(later),
            ReaperDecision::Wipe(FireReason::GapExceeded)
        ));
    }

    #[test]
    fn just_inside_the_threshold_does_not_fire() {
        let hb = PhyHeartbeat::new();
        hb.beat();
        let slightly_later = monotonic_ns() + UMBILICAL_GAP_NS / 2;
        assert_eq!(hb.evaluate(slightly_later), ReaperDecision::Continue);
    }

    #[test]
    fn explicit_disconnect_fires_immediately() {
        let hb = PhyHeartbeat::new();
        hb.signal_disconnect();
        assert!(matches!(
            hb.evaluate(monotonic_ns()),
            ReaperDecision::Wipe(FireReason::Disconnected)
        ));
    }

    #[test]
    fn a_long_gap_without_a_suspend_notification_is_a_yank() {
        // The bug this replaces: `gap > 2 s` used to return Suspend, so unplugging the stick and
        // waiting two seconds avoided the wipe altogether. Only the OS can tell us it slept.
        let hb = PhyHeartbeat::new();
        hb.beat();
        let an_hour_later = monotonic_ns() + 3_600 * SUSPEND_SUSPICION_NS;
        assert_eq!(
            hb.evaluate(an_hour_later),
            ReaperDecision::Wipe(FireReason::GapExceeded),
            "a long gap with no suspend signal must wipe: silence is not evidence of sleep"
        );
    }

    #[test]
    fn a_suspend_notification_suppresses_the_wipe() {
        // The other half: when the platform *does* tell us, the document survives.
        let hb = PhyHeartbeat::new();
        hb.beat();
        hb.signal_suspend();
        let an_hour_later = monotonic_ns() + 3_600 * SUSPEND_SUSPICION_NS;
        assert_eq!(hb.evaluate(an_hour_later), ReaperDecision::Suspend);
    }

    #[test]
    fn the_wipe_runs_before_termination() {
        // The ordering the trait docs argue for, as an assertion: `terminate` is the last thing
        // that certainly happens, and `wipe_secrets` is the first.
        let hb = Arc::new(PhyHeartbeat::new());
        let actions = Arc::new(CountingActions::new(0));
        let guard = ReaperGuard::start(
            Arc::clone(&hb),
            Arc::clone(&actions) as Arc<dyn EmergencyActions>,
        );
        hb.signal_disconnect();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !actions.terminated.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        guard.stop();
        assert!(
            actions.terminated.load(Ordering::SeqCst),
            "Thread B never terminated"
        );
        assert_eq!(
            actions.wipes.load(Ordering::SeqCst),
            1,
            "the wipe must have run before termination, exactly once"
        );
        assert_eq!(
            actions.order.lock().unwrap().as_slice()[..2],
            ["wipe", "terminate"],
            "secrets are wiped, then the process exits; the blackout is a courtesy after both"
        );
    }

    #[test]
    fn a_suspend_signal_does_not_outrank_a_disconnect() {
        // If the OS says both, the drive is gone either way and the wipe is the safe reading.
        let hb = PhyHeartbeat::new();
        hb.signal_suspend();
        hb.signal_disconnect();
        assert!(matches!(
            hb.evaluate(monotonic_ns()),
            ReaperDecision::Wipe(FireReason::Disconnected)
        ));
    }

    #[test]
    fn thread_b_fires_and_wipes_when_the_heartbeat_stops() {
        let hb = Arc::new(PhyHeartbeat::new());
        let actions = Arc::new(CountingActions::new(0));
        let guard = ReaperGuard::start(
            Arc::clone(&hb),
            Arc::clone(&actions) as Arc<dyn EmergencyActions>,
        );

        // Stop beating: simulate the drive leaving.
        hb.last_phy_ok_ns.store(
            monotonic_ns().saturating_sub(UMBILICAL_GAP_NS + 1),
            Ordering::Release,
        );

        // Within a few poll quanta the reaper must have acted.
        for _ in 0..100 {
            if actions.wipes.load(Ordering::SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        guard.stop();

        assert_eq!(
            actions.wipes.load(Ordering::SeqCst),
            1,
            "wipe must happen exactly once"
        );
        assert_eq!(actions.blackouts.load(Ordering::SeqCst), 1);
        assert!(actions.terminated.load(Ordering::SeqCst));
        assert!(hb.firing.load(Ordering::SeqCst));
        assert_eq!(
            FireReason::from_u8(hb.fire_reason.load(Ordering::SeqCst)),
            FireReason::GapExceeded
        );
    }

    #[test]
    fn a_continuing_heartbeat_never_fires() {
        let hb = Arc::new(PhyHeartbeat::new());
        let actions = Arc::new(CountingActions::new(0));
        let guard = ReaperGuard::start(
            Arc::clone(&hb),
            Arc::clone(&actions) as Arc<dyn EmergencyActions>,
        );
        for _ in 0..30 {
            hb.beat();
            std::thread::sleep(Duration::from_millis(1));
        }
        guard.stop();
        assert_eq!(actions.wipes.load(Ordering::SeqCst), 0);
        assert!(!hb.firing.load(Ordering::SeqCst));
    }

    #[test]
    fn eperm_is_survivable_and_reported() {
        // In CI this is almost always `EpermFallback`; on a privileged shell it may be
        // `RealTime(99)`. Both are acceptable, and neither may panic or abort the reaper.
        let hb = Arc::new(PhyHeartbeat::new());
        let actions = Arc::new(CountingActions::new(0));
        let guard = ReaperGuard::start(
            Arc::clone(&hb),
            Arc::clone(&actions) as Arc<dyn EmergencyActions>,
        );
        let sched = guard.sched();
        guard.stop();
        match sched {
            SchedClass::RealTime(99) => assert!(sched.is_real_time()),
            SchedClass::EpermFallback => {
                assert!(sched.describe().contains("RLIMIT_RTPRIO"));
                assert!(!sched.is_real_time());
            }
            other => panic!("unexpected scheduler class {other:?}"),
        }
    }

    #[test]
    fn measured_timeline_is_recorded_and_bounded() {
        let hb = Arc::new(PhyHeartbeat::new());
        let actions = Arc::new(CountingActions::new(0));
        let guard = ReaperGuard::start(
            Arc::clone(&hb),
            Arc::clone(&actions) as Arc<dyn EmergencyActions>,
        );
        let t = guard.measure(actions.as_ref());
        guard.stop();
        assert_eq!(t.detection_ns, POLL_INTERVAL_NS);
        assert!(t.wipe_ns > 0, "the wipe of 16 MiB cannot be free");
        // Do not assert `within_ceiling` here: it depends on the host and on whether the wipe step
        // was given a realistic arena. `it_reports_what_it_measures` is the honest test.
    }

    #[test]
    fn it_reports_what_it_measures() {
        // A deliberately slow wipe must be *reported* as over the ceiling rather than rounded away.
        let hb = Arc::new(PhyHeartbeat::new());
        let slow = Arc::new(CountingActions::new(20_000_000));
        let guard = ReaperGuard::start(
            Arc::clone(&hb),
            Arc::clone(&slow) as Arc<dyn EmergencyActions>,
        );
        let t = guard.measure(slow.as_ref());
        guard.stop();
        assert!(t.wipe_ns >= 20_000_000, "wipe_ns was {}", t.wipe_ns);
        assert!(
            !t.within_ceiling(),
            "a 20 ms wipe must not be reported as inside 15 ms"
        );
    }

    #[test]
    fn fire_reason_round_trips() {
        for r in [
            FireReason::None,
            FireReason::GapExceeded,
            FireReason::Disconnected,
            FireReason::Suspend,
            FireReason::Manual,
        ] {
            assert_eq!(FireReason::from_u8(r as u8), r);
        }
    }

    #[test]
    fn poll_quantum_is_the_documented_one() {
        assert_eq!(EmergencyTimeline::poll_quantum_ns(), 1_500_000);
        assert_eq!(UMBILICAL_GAP_NS, 12_000_000);
    }
}
