//! OS-level capture exclusion — the *replacement* for the deleted SPOC and `CryptScanline` layers.
//!
//! # Why this is what survived
//!
//! Two invariants were removed in v5.1.0 because they did not work:
//!
//! * `SPOC`'s ±42 constant-variance flicker, which a cross-correlating video scraper recovers the
//!   text from (the phase relation *is* the signal) and which is a photosensitive-epilepsy hazard;
//! * `CryptScanline`'s pre-baked encrypted bit-planes, which cannot fit in the container
//!   (10,000 pages ≈ 1,027 MiB, eight times the whole file) and which are one XOR from plaintext
//!   in the same buffer that gets blitted.
//!
//! What is left is the boring, effective, *supported* mechanism: ask the OS not to include this
//! window in captures. Three platforms, three APIs:
//!
//! | platform | API | excludes |
//! | :--- | :--- | :--- |
//! | Windows 10 2004+ | `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` | `BitBlt`, PrintScreen, most of the Desktop Duplication API, Recall |
//! | macOS 10.15+ | `NSWindow.sharingType = .none` | `CGWindowListCreateImage`, screen recording (the window renders black/absent) |
//! | Linux (X11/Wayland) | *nothing equivalent* | — |
//!
//! The Linux row is the important one and the old PRD never wrote it down. There is no portable
//! way to exclude a window from a screencast on X11 or Wayland: a compositor-level screencast
//! reads the same buffers the display does. So [`CaptureExclusion::LinuxUnavailable`] is a
//! **first-class outcome**, surfaced in the UI, rather than an error to be logged and hidden. Any
//! claim about screenshots on Linux has to be a claim about the *container*, not the window.

/// What capture exclusion this process obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureExclusion {
    /// The window is excluded from OS capture.
    Enforced(EnforcedBy),
    /// The platform has no mechanism; screenshots and screen recording will see the document.
    LinuxUnavailable,
    /// The API exists but the call failed. The errno/code is kept for the log.
    Failed {
        /// Platform error code returned by the failed API call.
        code: i32,
    },
    /// Not attempted (headless, or the window does not exist yet).
    NotAttempted,
}

/// Which API did the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcedBy {
    /// `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`.
    WindowsDisplayAffinity,
    /// `NSWindow.sharingType = NSWindowSharingNone`.
    MacSharingNone,
}

/// The result of an attempt, with the sentence the UI should show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureReport {
    /// Which rung of capture exclusion was obtained.
    pub exclusion: CaptureExclusion,
}

impl CaptureReport {
    /// The honest one-line status for the window chrome.
    ///
    /// This is user-facing copy. It must not claim protection the platform does not provide.
    pub fn describe(&self) -> &'static str {
        match self.exclusion {
            CaptureExclusion::Enforced(EnforcedBy::WindowsDisplayAffinity) => {
                "screen capture of this window is blocked by Windows"
            }
            CaptureExclusion::Enforced(EnforcedBy::MacSharingNone) => {
                "screen capture of this window is blocked by macOS"
            }
            CaptureExclusion::LinuxUnavailable => {
                "this platform cannot exclude a window from screen capture; the document is \
                 visible to screen recorders while unlocked"
            }
            CaptureExclusion::Failed { .. } => {
                "capture exclusion was refused by the OS; the document is visible to screen \
                 recorders while unlocked"
            }
            CaptureExclusion::NotAttempted => "capture exclusion not attempted",
        }
    }

    /// Whether the UI may show a "protected" badge.
    pub fn is_enforced(&self) -> bool {
        matches!(self.exclusion, CaptureExclusion::Enforced(_))
    }

    /// What kind of exclusion the running platform can even offer.
    pub fn platform_default() -> Self {
        #[cfg(windows)]
        {
            // The real call needs an HWND, which only exists after the window is created; the
            // shell calls `apply_windows` there.
            Self {
                exclusion: CaptureExclusion::NotAttempted,
            }
        }
        #[cfg(target_os = "macos")]
        {
            Self {
                exclusion: CaptureExclusion::NotAttempted,
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Self {
                exclusion: CaptureExclusion::LinuxUnavailable,
            }
        }
    }
}

/// Exclude `hwnd` from capture. Windows only; a no-op elsewhere.
///
/// # Safety
///
/// `hwnd` must be a live window handle owned by this process.
#[cfg(windows)]
pub unsafe fn apply_windows(hwnd: *mut core::ffi::c_void) -> CaptureReport {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowDisplayAffinity;
    // WDA_EXCLUDEFROMCAPTURE = 0x00000011. Windows 10 version 2004 and later; older builds
    // silently treat it as WDA_MONITOR (which blackens rather than excludes).
    const WDA_EXCLUDEFROMCAPTURE: u32 = 0x11;
    let ok = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
    if ok != 0 {
        CaptureReport {
            exclusion: CaptureExclusion::Enforced(EnforcedBy::WindowsDisplayAffinity),
        }
    } else {
        CaptureReport {
            exclusion: CaptureExclusion::Failed {
                code: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            },
        }
    }
}

/// Exclude the key window from capture. macOS only; a no-op elsewhere.
#[cfg(target_os = "macos")]
pub fn apply_macos() -> CaptureReport {
    // The implementation needs the `NSWindow` the shell created; that call site owns it. This
    // module keeps the *decision* and the *copy*, so the claim cannot drift from the API.
    CaptureReport {
        exclusion: CaptureExclusion::NotAttempted,
    }
}

/// Document the Linux answer in code, not only in prose.
pub fn linux_has_no_exclusion() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        true
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_never_overpromise() {
        let linux = CaptureReport {
            exclusion: CaptureExclusion::LinuxUnavailable,
        };
        assert!(!linux.is_enforced());
        assert!(linux.describe().contains("cannot exclude"));
        assert!(!linux.describe().contains("is blocked"));

        let failed = CaptureReport {
            exclusion: CaptureExclusion::Failed { code: 5 },
        };
        assert!(!failed.is_enforced());
        assert!(failed.describe().contains("refused"));

        let win = CaptureReport {
            exclusion: CaptureExclusion::Enforced(EnforcedBy::WindowsDisplayAffinity),
        };
        assert!(win.is_enforced());
        assert!(win.describe().contains("blocked by Windows"));
    }

    #[test]
    fn platform_default_matches_the_table() {
        let d = CaptureReport::platform_default();
        if linux_has_no_exclusion() {
            assert_eq!(d.exclusion, CaptureExclusion::LinuxUnavailable);
        }
    }

    #[test]
    fn linux_reports_unavailability_rather_than_success() {
        // The point of the module: on Linux the answer is "no", and it is a first-class answer.
        if linux_has_no_exclusion() {
            assert!(!CaptureReport::platform_default().is_enforced());
        }
    }
}
