//! Network namespace isolation, and the evidence that it actually happened.
//!
//! # The problem, exactly
//!
//! `unshare(CLONE_NEWNET)` needs `CAP_SYS_ADMIN` in the user namespace that owns the network
//! namespace. An unprivileged process has no capabilities in the initial user namespace, so
//! the call returns `EPERM`. On a bare-metal target running as root it is one syscall; in a
//! development container it is not available at all.
//!
//! The way through is to unshare a **user** namespace first. Creating one grants the creator a
//! full capability set *in the new namespace*, including `CAP_SYS_ADMIN`, which is then enough
//! to create the network namespace. So the unprivileged path is
//! `unshare(CLONE_NEWUSER | CLONE_NEWNET)` in one call.
//!
//! # Why the uid map is deliberately not written
//!
//! A process inside an unmapped user namespace appears as uid 65534 with no capabilities *in
//! the parent namespace* -- but it does hold every capability in its own. H1 has no
//! uid-dependent behaviour after boot: no `chown`, no privilege check, nothing that reads
//! ownership. Leaving the map unwritten is therefore strictly *more* restrictive than writing
//! `0 <outer uid>`, which would make the process root inside its own namespace and buy
//! nothing.
//!
//! It also saves two file operations in the boot sequence, which matters in a design that
//! counts boot syscalls.
//!
//! One consequence is load-bearing and is why this sits after `mlockall`: `RLIMIT_MEMLOCK` is
//! accounted per uid, and an unmapped uid is not the one the limit was granted to. Locking
//! first, inside the namespace that owns the limit, and then isolating, is the order that works
//! on both paths. Phase 7's boot does exactly that.
//!
//! # `EINVAL`, not `EPERM`: the boot must be single-threaded
//!
//! Measured, and it is the sharpest constraint in this module. `unshare(CLONE_NEWUSER)` calls
//! `copy_creds`, and the kernel refuses to change a thread group's credentials while more than one
//! thread is alive:
//!
//! ```c
//! if (atomic_read(&current->signal->live) != 1)
//!         return -EINVAL;
//! ```
//!
//! So in a process with any other thread alive, the combined
//! `unshare(CLONE_NEWUSER | CLONE_NEWNET)` fails with **`EINVAL`**, not `EPERM` -- and the two are
//! not interchangeable. `EPERM` means "this user may not have a network namespace, ever, from here",
//! which is a policy fact. `EINVAL` means "you are multi-threaded", which is a fact about the caller
//! and which the boot can fix by ordering itself differently.
//!
//! Measured on this host, in the same binary, one call apart:
//!
//! | process | `unshare(CLONE_NEWUSER\|CLONE_NEWNET)` |
//! |---------|----------------------------------------|
//! | main thread | **succeeds**, netns inode changes |
//! | a spawned thread | `EINVAL`, netns inode unchanged |
//!
//! And `mlockall` and `PR_SET_NO_NEW_PRIVS` before it change nothing, so the ordering that PROJECT.md
//! specifies is not the problem.
//!
//! **The consequence for the design is that the whole boot sequence must run on the main thread,
//! before anything spawns a thread.** Not "should" -- must, or stage 6 fails with an errno that
//! names neither the cause nor the fix. That is why [`crate::Sealed`] carries a
//! `NetworkIsolation` outcome rather than a bool, and why [`NetworkIsolation::reason`] exists.
//!
//! It is also why the Phase 7 census runs from `examples/census_session.rs` rather than from a
//! `#[test]`: `libtest` runs every test on a spawned thread, so a test *cannot* isolate the network
//! no matter how it is written.
//!
//! # Why the return value is an outcome and not a bool
//!
//! Because `Unprivileged` and `Privileged` are different security postures and the boot report
//! should distinguish them, and because `Unavailable` is not a failure to be swallowed: it is
//! the difference between a jail with no network namespace and one that merely claims to.
//! [`NetworkIsolation::is_isolated`] is the question the gate asks, and
//! [`NetworkIsolation::report`] is what it prints.

/// Why a network namespace was not entered.
///
/// A three-case enum rather than an `Option<&str>` because one of the cases has to carry a number,
/// and because a caller that pattern-matches cannot silently treat "isolated" as "not isolated and
/// nothing to say".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Isolated. Carries which route: `"privileged"` or `"user-namespace"`.
    Isolated(&'static str),
    /// Refused because the process had more than one thread alive.
    MultiThreaded,
    /// Refused because this host does not permit unprivileged user namespaces.
    Unprivileged,
    /// Refused for some other reason. The `errno` is the finding.
    Unknown(i32),
}

impl Reason {
    /// One line for a boot report.
    pub fn describe(self) -> String {
        match self {
            Self::Isolated(route) => format!("net: isolated via {route}"),
            Self::MultiThreaded => "net: NOT isolated -- EINVAL from unshare(CLONE_NEWUSER): \
                 the process has more than one thread alive and the kernel will not change a \
                 thread group's credentials while it does. The boot sequence must run on the main \
                 thread before anything spawns a thread."
                .to_string(),
            Self::Unprivileged => "net: NOT isolated -- EPERM from                  unshare(CLONE_NEWUSER|CLONE_NEWNET): this host does not permit unprivileged user                  namespaces, so there is no route to a network namespace without privilege.                  Production runs as root; a development host without it must not be on an                  untrusted network."
            .to_string(),
            Self::Unknown(e) => format!(
                "net: NOT isolated -- unshare failed with errno {e}, which is neither EINVAL \
                 (multi-threaded) nor EPERM (no unprivileged user namespaces)"
            ),
        }
    }
}

impl core::fmt::Display for Reason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// What happened when the jail asked for a network namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkIsolation {
    /// `unshare(CLONE_NEWNET)` succeeded directly: the process had `CAP_SYS_ADMIN`. The
    /// production path.
    Privileged,
    /// `unshare(CLONE_NEWUSER | CLONE_NEWNET)` succeeded. Isolation is real; the process is
    /// additionally inside a user namespace with no uid mapping.
    Unprivileged {
        /// The direct attempt's `errno`. Always `EPERM`; kept because a different one would
        /// mean something else is wrong.
        direct_errno: i32,
    },
    /// No network namespace was created. Carries `errno`.
    Unavailable {
        /// `errno` from the last attempt.
        errno: i32,
    },
}

impl NetworkIsolation {
    /// Whether a network namespace was actually entered.
    pub const fn is_isolated(self) -> bool {
        !matches!(self, Self::Unavailable { .. })
    }

    /// Why no namespace was entered, in terms that distinguish the two causes.
    ///
    /// `EPERM` and `EINVAL` mean different things and have different fixes, so a boot report that
    /// prints only the errno leaves the reader to guess which one they have. See the module comment
    /// for the measurement.
    pub fn reason(self) -> Reason {
        match self {
            Self::Privileged => Reason::Isolated("CLONE_NEWNET directly, holding CAP_SYS_ADMIN"),
            Self::Unprivileged { .. } => {
                Reason::Isolated("CLONE_NEWUSER|CLONE_NEWNET, with no uid map written")
            }
            Self::Unavailable {
                errno: libc::EINVAL,
            } => Reason::MultiThreaded,
            Self::Unavailable { errno: libc::EPERM } => Reason::Unprivileged,
            // An errno outside the two known cases: report the number rather than invent a
            // diagnosis.
            Self::Unavailable { errno } => Reason::Unknown(errno),
        }
    }

    /// Whether the process is also inside a user namespace it created for itself.
    pub const fn via_user_namespace(self) -> bool {
        matches!(self, Self::Unprivileged { .. })
    }

    /// One line for the boot report.
    pub fn report(self) -> String {
        match self {
            Self::Privileged => "net: CLONE_NEWNET (direct, CAP_SYS_ADMIN)".to_string(),
            Self::Unprivileged { direct_errno } => format!(
                "net: CLONE_NEWUSER|CLONE_NEWNET (direct gave EPERM {direct_errno}); \
                 no uid map written, so the process is uid 65534 inside its own namespace"
            ),
            Self::Unavailable { errno } => format!(
                "net: NOT ISOLATED (errno {errno}); this process has no network namespace and \
                 must not be exposed to an untrusted network"
            ),
        }
    }
}

/// Inode number of this process's network namespace.
///
/// The inode is the only thing that changes when `unshare(CLONE_NEWNET)` succeeds, and it is
/// how the jail *proves* isolation instead of trusting a return code. Read before and after;
/// equal inodes mean the call did not take effect even if it appeared to.
///
/// Costs an `openat` plus a `statx`, which is why it happens during boot and never after.
pub fn namespace_inode() -> Option<u64> {
    // `std::fs::metadata` rather than `libc::stat`, which on musl is a two-call
    // out-parameter API (`int stat(const char *, struct stat *)`) and buys nothing here.
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self/ns/net")
        .ok()
        .map(|meta| meta.ino())
}

/// Create a network namespace, preferring the privileged path.
///
/// See the module comment for why the unprivileged path is `CLONE_NEWUSER | CLONE_NEWNET`, and
/// why the uid map is left unwritten.
pub fn isolate_network() -> NetworkIsolation {
    // The privileged path first: it is one syscall and it is what production does. A failure
    // here is atomic -- nothing is unshared -- so falling through is safe.
    // SAFETY: `CLONE_NEWNET` takes only flag bits.
    if unsafe { libc::unshare(libc::CLONE_NEWNET) } == 0 {
        return NetworkIsolation::Privileged;
    }
    let direct_errno = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EPERM);

    // SAFETY: as above, with the user-namespace flag added. `CLONE_NEWUSER` is checked first
    // by the kernel and creates the namespace that makes `CLONE_NEWNET` permitted, so the two
    // flags together are one operation rather than two racing ones.
    if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) } == 0 {
        return NetworkIsolation::Unprivileged { direct_errno };
    }
    NetworkIsolation::Unavailable {
        errno: std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EPERM),
    }
}

/// Prove isolation by observing the namespace inode change.
///
/// Returns `(before, after)`, or `None` if `/proc/self/ns/net` could not be read. A `None`
/// means the proof was not obtained, which is different from a proof of non-isolation, and
/// the caller should treat it as the former.
pub fn isolate_and_verify() -> (NetworkIsolation, Option<(u64, u64)>) {
    let before = namespace_inode();
    let outcome = isolate_network();
    let after = namespace_inode();
    let evidence = match (before, after) {
        (Some(b), Some(a)) => Some((b, a)),
        _ => None,
    };
    (outcome, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_namespace_inode_is_readable_and_nonzero() {
        let inode = namespace_inode().expect("/proc/self/ns/net must be stat-able");
        assert_ne!(inode, 0, "a namespace inode of 0 is not a namespace");
    }

    #[test]
    fn outcomes_report_their_own_security_posture() {
        assert!(NetworkIsolation::Privileged.is_isolated());
        assert!(!NetworkIsolation::Privileged.via_user_namespace());
        assert!(NetworkIsolation::Unprivileged {
            direct_errno: libc::EPERM
        }
        .is_isolated());
        assert!(NetworkIsolation::Unprivileged {
            direct_errno: libc::EPERM
        }
        .via_user_namespace());
        assert!(!NetworkIsolation::Unavailable { errno: libc::EPERM }.is_isolated());
    }

    #[test]
    fn a_failed_isolation_says_so_in_its_report() {
        // The string a boot log shows has to make an absent namespace impossible to miss,
        // because the alternative is a process that believes it is isolated and is not.
        let report = NetworkIsolation::Unavailable { errno: 1 }.report();
        assert!(report.contains("NOT ISOLATED"), "{report}");
    }

    #[test]
    fn the_two_failures_are_told_apart() {
        // `EINVAL` means multi-threaded and `EPERM` means no permission. Printing the bare errno
        // for both would leave the reader to guess which one they are looking at, and the fixes are
        // completely different -- reorder the boot, versus get privilege.
        let threaded = NetworkIsolation::Unavailable {
            errno: libc::EINVAL,
        };
        assert_eq!(threaded.reason(), Reason::MultiThreaded);
        assert!(
            threaded.reason().describe().contains("main thread"),
            "{}",
            threaded.reason().describe()
        );

        let unprivileged = NetworkIsolation::Unavailable { errno: libc::EPERM };
        assert_eq!(unprivileged.reason(), Reason::Unprivileged);
        assert!(
            unprivileged.reason().describe().contains("privilege"),
            "{}",
            unprivileged.reason().describe()
        );

        assert_eq!(
            NetworkIsolation::Unavailable { errno: 4095 }.reason(),
            Reason::Unknown(4095),
            "an errno outside the two known cases must be reported, not guessed at"
        );
        assert_eq!(
            NetworkIsolation::Privileged.reason(),
            Reason::Isolated("CLONE_NEWNET directly, holding CAP_SYS_ADMIN"),
            "a successful isolation still says which route it took"
        );
    }

    #[test]
    fn a_user_namespace_report_mentions_the_missing_uid_map() {
        let report = NetworkIsolation::Unprivileged {
            direct_errno: libc::EPERM,
        }
        .report();
        assert!(report.contains("no uid map"), "{report}");
    }
}
