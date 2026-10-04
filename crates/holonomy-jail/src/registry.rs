//! The registry of live guarded mappings, so a tripwire can scrub them all.
//!
//! # Why the registry lives in the jail and not in `holonomy-secure`
//!
//! `holonomy-jail` is a leaf by construction (see the crate comment in `lib.rs`): every
//! other crate depends on it and it depends on nothing but `libc`. So it cannot name
//! [`SecureBlock`], and `SecureBlock` cannot be reached from here. What *can* be shared is
//! the thing that actually matters, which is the list of ranges to scrub, and that is
//! `(base, total, data, len)` tuples -- four integers, no type.
//!
//! So `SecureBlock::allocate` registers its mapping here on the way out of `mmap`, and
//! `Drop` deregisters it. The tripwire handler walks the table with nothing but atomics
//! and volatile stores, which is what makes it safe to call from a signal handler at all.
//!
//! # The ordering that makes it work
//!
//! Three rules, each of which closes a window that the others leave open.
//!
//! **1. `len` is published last, and it doubles as the claim.** A slot is empty exactly
//! when `len == 0`, and a slot being filled exactly when `len == CLAIMED`. `register`
//! does `compare_exchange(0, CLAIMED)`, writes `base`/`total`/`data`/`len`, then stores
//! the real `len` with `Release`. A reader does `len.load(Acquire)` first and skips
//! anything that is not a plausible length, so it can never observe a half-written slot
//! and go scrub a stale address.
//!
//! **2. `Drop` scrubs *before* it deregisters.** The tempting order is deregister first so
//! the handler cannot touch a block that is going away, and it is the wrong one: between
//! deregister and scrub there is a live, registered-nowhere, *still full of plaintext*
//! mapping, and a guard fault in that window would exit with the plaintext intact. Scrub
//! first instead. Then the only interval in which a clean block is unregistered is one in
//! which there is nothing left to steal.
//!
//! **3. `munmap` is last.** Unmapping before scrubbing would make the scrub a write to
//! `PROT_NONE` memory, which is the exact segfault-in-`memset` shape that Phase 6 spent a
//! commit killing (see `SecureBlock::allocate`).
//!
//! # What this does not cover, stated rather than implied
//!
//! A fault *caused by* a secure block's own lifecycle -- inside `mmap`, `mlock`, `mprotect`
//! or `munmap` -- is not covered, because at that point the block is not yet published (so
//! the handler skips it, which is safe, since nothing can name the pointer) or is already
//! scrubbed (so there is nothing to steal). The residual is the `register` window itself,
//! where a mapping exists and is published as `CLAIMED`; the handler skips `CLAIMED` slots,
//! so a block whose fault arrives mid-registration is not scrubbed. Closing that needs a
//! stop-the-world lock, which a signal handler cannot take without deadlocking against the
//! thread it interrupted. The window is a handful of instructions in `allocate`, and the
//! alternative -- trusting the table more than it deserves -- is worse.
//!
//! `CAPACITY` is 4096 slots. A 2,000-page document is ~1,747 leaves (Phase 6's measured
//! figure), so this holds a full-budget document plus the alternate signal stack with room
//! to spare. Overflow is an error, not a silent drop: a block that cannot be registered
//! cannot be guaranteed scrubbed on a guard fault, and that is a security failure, not a
//! bookkeeping detail.

use core::sync::atomic::{AtomicUsize, Ordering};

/// Maximum number of simultaneously registered mappings.
///
/// See the module comment for why 4096 and why overflow is an error.
pub const CAPACITY: usize = 4096;

/// Sentinel `len` meaning "this slot has been claimed but not yet published".
///
/// Distinct from both `0` (empty) and any real length, which is what lets the reader
/// reject it with a single comparison.
const CLAIMED: usize = usize::MAX;

/// One registered mapping: `[guard_lo][data][guard_hi]`.
struct Slot {
    /// First guard page. Written during registration, never mutated afterwards.
    base: AtomicUsize,
    /// Total mapped length including both guards.
    total: AtomicUsize,
    /// First data byte, i.e. `base + page`.
    data: AtomicUsize,
    /// Data length. `0` = empty, `CLAIMED` = being filled, anything else = live.
    ///
    /// **This is the publication word.** Everything else is written before it, and it is
    /// read before everything else.
    len: AtomicUsize,
    /// Bumped on every registration so a stale handle cannot clear a live slot.
    generation: AtomicUsize,
}

impl Slot {
    const fn new() -> Self {
        Self {
            base: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            data: AtomicUsize::new(0),
            len: AtomicUsize::new(0),
            generation: AtomicUsize::new(0),
        }
    }
}

static REGISTRY: [Slot; CAPACITY] = [const { Slot::new() }; CAPACITY];

/// Why a mapping could not be registered.
///
/// Two variants, and both are refusals rather than warnings: a mapping the tripwire cannot reach
/// is a mapping whose plaintext may survive a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterError {
    /// No free slot.
    Full,
    /// A zero-length payload.
    ///
    /// `SecureBlock::allocate` already rejects zero length, so reaching this means a caller
    /// bypassed it. Refused rather than tolerated because the slot would be published with
    /// `len == 0`, which every reader treats as *empty* -- so the mapping would look unregistered
    /// to the scrub and to `classify`, silently. `tests/guard_page.rs` asserts the refusal.
    EmptyPayload,
}

impl core::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Full => write!(
                f,
                "secure-mapping registry full ({CAPACITY} slots); a live block would be \
                 unreachable by the tripwire scrub"
            ),
            Self::EmptyPayload => f.write_str("cannot register a zero-length payload"),
        }
    }
}

impl std::error::Error for RegisterError {}

/// Proof of registration, and the only thing that can undo one.
///
/// Carries a generation so that a handle whose slot has since been recycled cannot
/// deregister the *new* occupant. Getting that wrong would be a use-after-free with extra
/// steps, and the guard pages would not catch it because both mappings are valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryHandle {
    index: usize,
    generation: usize,
}

/// Why a faulting address was rejected by the tripwire handler.
///
/// The distinction is not academic: `Guard` means the containment worked and the process
/// is exiting on purpose, while `Unregistered` means something faulted in memory the
/// registry has never heard of, which is a bug rather than a containment event. Both
/// exit 137 -- the point of the process is gone either way -- but only one of them is a
/// success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultSite {
    /// Inside a registered block's `PROT_NONE` guard. Containment held.
    Guard,
    /// Inside a registered block's data region. A wild write into live data.
    Data,
    /// Not inside any registered mapping. An unowned address, e.g. a null dereference.
    Unregistered,
}

impl FaultSite {
    /// The word used in the tripwire report.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Guard => "guard",
            Self::Data => "data",
            Self::Unregistered => "unregistered",
        }
    }
}

/// Register a guarded mapping.
///
/// `base`/`total` describe the whole mapping including both `PROT_NONE` guards; `data`/`len`
/// describe the payload. Both are needed: the guards are what makes a fault reportable and
/// the payload is what must be scrubbed.
///
/// Allocates nothing and takes no lock, which is what lets it run on the session's hot
/// path without becoming a place that fails under memory pressure.
pub fn register(
    base: usize,
    total: usize,
    data: usize,
    len: usize,
) -> Result<RegistryHandle, RegisterError> {
    if len == 0 {
        return Err(RegisterError::EmptyPayload);
    }
    debug_assert!(
        data >= base && data + len <= base + total,
        "data region escapes its mapping"
    );
    for (index, slot) in REGISTRY.iter().enumerate() {
        if slot
            .len
            .compare_exchange(0, CLAIMED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            continue;
        }
        // Claimed. Write the payload description, then publish `len` last.
        let generation = slot.generation.load(Ordering::Relaxed).wrapping_add(1);
        slot.base.store(base, Ordering::Relaxed);
        slot.total.store(total, Ordering::Relaxed);
        slot.data.store(data, Ordering::Relaxed);
        slot.generation.store(generation, Ordering::Relaxed);
        slot.len.store(len, Ordering::Release);
        return Ok(RegistryHandle { index, generation });
    }
    Err(RegisterError::Full)
}

/// Remove a mapping from the registry.
///
/// Safe to call from any thread at any time relative to the handler: the handler either
/// sees the block and scrubs it (harmless if the caller already did) or does not (the
/// caller is about to scrub it). The generation check means a handle for a slot that has
/// been recycled since does nothing.
pub fn deregister(handle: RegistryHandle) {
    let slot = &REGISTRY[handle.index];
    if slot.generation.load(Ordering::Acquire) != handle.generation {
        return;
    }
    // Publish empty *before* the caller scrubs and unmaps. Release so a reader that sees 0
    // cannot then see a stale `data`.
    slot.len.store(0, Ordering::Release);
}

/// Overwrite the payload of every registered mapping with zeroes.
///
/// Async-signal-safe by construction: atomics and `write_volatile`, no locks, no
/// allocation, no libc beyond what the compiler emits for the loop. This is the same
/// volatile-store discipline `zeroize` uses, inlined here so the jail keeps its single
/// `libc` dependency and the handler needs no unwinding machinery.
///
/// Returns the number of bytes scrubbed, for the tripwire report.
pub fn scrub_all() -> usize {
    let mut bytes = 0usize;
    for slot in REGISTRY.iter() {
        // Read `len` first and everything else second. Combined with `register` publishing
        // `len` last, this is what makes a half-written slot unreadable rather than merely
        // unlikely.
        let len = slot.len.load(Ordering::Acquire);
        if len == 0 || len == CLAIMED {
            continue;
        }
        let data = slot.data.load(Ordering::Relaxed);
        scrub(data, len);
        bytes += len;
    }
    bytes
}

/// Zero `len` bytes at `ptr`, byte by byte, through a volatile pointer.
///
/// The byte loop is deliberate. A `ptr::write_bytes` over a large range can be widened by
/// the backend into a `memset`, and the only defence against a dead store there is the
/// compiler barrier -- whereas a volatile store per byte cannot be elided at all. This is
/// the same argument `zeroize` makes and the same cost, on a path that runs once.
fn scrub(ptr: usize, len: usize) {
    let mut offset = 0usize;
    while offset < len {
        // SAFETY: the registry only ever holds ranges that came from `SecureBlock::allocate`,
        // which reserved `data..data+len` as a read/write region of a live mapping. The
        // caller of `scrub_all` is a signal handler, and this is the one operation that is
        // permitted to write there outside the block's owner.
        unsafe { core::ptr::write_volatile(ptr.wrapping_add(offset) as *mut u8, 0) };
        offset += 1;
    }
    // Nothing below may be reordered above the stores: the data is plaintext and the next
    // step is process exit.
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// Classify a faulting address against the registry.
///
/// Read-only and allocation-free, so the tripwire handler can call it to produce an
/// accurate report instead of guessing.
pub fn classify(addr: usize) -> FaultSite {
    for slot in REGISTRY.iter() {
        let len = slot.len.load(Ordering::Acquire);
        if len == 0 || len == CLAIMED {
            continue;
        }
        let base = slot.base.load(Ordering::Relaxed);
        let total = slot.total.load(Ordering::Relaxed);
        if addr >= base && addr < base.wrapping_add(total) {
            let data = slot.data.load(Ordering::Relaxed);
            if addr >= data && addr < data + len {
                return FaultSite::Data;
            }
            return FaultSite::Guard;
        }
    }
    FaultSite::Unregistered
}

/// Number of mappings currently registered.
pub fn active_count() -> usize {
    REGISTRY
        .iter()
        .filter(|slot| {
            let len = slot.len.load(Ordering::Acquire);
            len != 0 && len != CLAIMED
        })
        .count()
}
