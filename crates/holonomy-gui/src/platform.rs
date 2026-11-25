//! Platform desktop window abstraction shared by the Unix (`x11`), macOS (`cocoa`), and
//! Windows (`win32`) GUI backends (`zero` external GUI crate dependencies under `--locked`).

use std::error::Error;

use zeroize::Zeroizing;

use crate::{Key, Modifiers};

/// Maximum clipboard UTF-8 payload accepted into the editor in a single paste (`4 KiB`).
pub const MAX_CLIPBOARD_BYTES: usize = 4_096;

/// High-level input/window events dispatched from a native desktop backend to the GUI loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopEvent {
    /// Key press translated by the native platform window.
    Key {
        /// Key symbol.
        key: Key,
        /// Active modifier flags.
        modifiers: Modifiers,
    },
    /// Left mouse press (`x, y` in window client coordinates).
    MouseDown {
        /// Window-relative X pixel.
        x: i16,
        /// Window-relative Y pixel.
        y: i16,
        /// Active modifiers (`Shift` extends selection).
        modifiers: Modifiers,
    },
    /// Left mouse drag motion (`x, y` in window client coordinates).
    MouseDrag {
        /// Window-relative X pixel.
        x: i16,
        /// Window-relative Y pixel.
        y: i16,
    },
    /// Left mouse release.
    MouseUp,
    /// Mouse scroll wheel delta (`lines < 0` scrolls up, `> 0` scrolls down).
    Scroll {
        /// Scroll direction in logical steps (`-1` up, `+1` down).
        delta_lines: i32,
    },
    /// Window resized to `(width, height)` client pixels.
    Resize {
        /// Window width (`px`).
        width: u16,
        /// Window height (`px`).
        height: u16,
    },
    /// Window content must be presented again.
    Expose,
    /// Window manager asked to close (`WM_DELETE_WINDOW` / `WM_CLOSE` / `windowShouldClose:`).
    CloseRequested,
    /// Window lost keyboard focus.
    FocusOut,
    /// Clipboard UTF-8 contents arrived in response to [`DesktopWindow::request_clipboard`].
    ClipboardPasted(Zeroizing<Vec<u8>>),
}

/// Platform window capability required by Holonomy's `PixelSurface` event loop.
pub trait DesktopWindow {
    /// Current client viewport `(width, height)` in pixels.
    fn size(&self) -> (u16, u16);

    /// Estimated display DPI (`96` baseline).
    fn dpi(&self) -> i64;

    /// Presents a row-major `0x00RRGGBB` pixel framebuffer (`pixels.len() >= width * height`).
    ///
    /// # Errors
    ///
    /// Returns a backend error if blitting to the native surface fails.
    fn present_argb(
        &mut self,
        pixels: &[u32],
        width: u16,
        height: u16,
    ) -> Result<(), Box<dyn Error>>;

    /// Waits up to `timeout_ms` for native input/window events and returns coalesced items.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the native message queue closes or fails.
    fn poll_events(&mut self, timeout_ms: i32) -> Result<Vec<DesktopEvent>, Box<dyn Error>>;

    /// Publishes `utf8` to the native OS clipboard (retaining local copies in `Zeroizing`).
    ///
    /// # Errors
    ///
    /// Returns a backend error if clipboard ownership cannot be acquired.
    fn set_clipboard(&mut self, utf8: &[u8]) -> Result<(), Box<dyn Error>>;

    /// Requests clipboard UTF-8 contents from the native OS clipboard.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the clipboard read/conversion request fails.
    fn request_clipboard(&mut self) -> Result<(), Box<dyn Error>>;

    /// Releases/scrubs any owned clipboard buffer held on behalf of this window.
    fn clear_clipboard(&mut self);
}

/// Opens the native desktop window for the current OS (`Win32` on Windows, `Cocoa`/`X11` on
/// macOS, pure-`libc` `X11` on Linux/Unix).
///
/// # Errors
///
/// Returns an error if the platform display server or window subsystem cannot be opened.
pub fn open_desktop_window(
    title: &str,
    width: u16,
    height: u16,
) -> Result<Box<dyn DesktopWindow>, Box<dyn Error>> {
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(crate::win32::Win32Window::open(
            title, width, height,
        )?))
    }
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os("DISPLAY").is_some() {
            if let Ok(x11) = crate::x11::X11Window::open(None, title, width, height) {
                return Ok(Box::new(x11));
            }
        }
        Ok(Box::new(crate::cocoa::CocoaWindow::open(
            title, width, height,
        )?))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Ok(Box::new(crate::x11::X11Window::open(
            None, title, width, height,
        )?))
    }
}
