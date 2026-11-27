//! Platform desktop window abstraction shared by the Unix (`x11`), macOS (`cocoa`), and
//! Windows (`win32`) GUI backends (`zero` external GUI crate dependencies under `--locked`).

use std::error::Error;
use std::path::PathBuf;

use zeroize::Zeroizing;

use crate::{Key, Modifiers};

/// Maximum clipboard UTF-8 payload accepted into the editor in a single paste (`4 KiB`).
pub const MAX_CLIPBOARD_BYTES: usize = 4_096;

/// Whole-desktop display scale, applied at the native-window boundary to *every* windowed view
/// (unlock prompt, editor chrome, paper, overlays, history drawer) in one transform.
///
/// The logical layout keeps the existing 96-dpi pixel geometry; the window presents it at
/// `percent / 100` physical size and divides pointer coordinates by the same number. That keeps
/// rendering and hit-testing on a single shared coordinate space instead of scaling some widgets
/// twice and others not at all. Document zoom (`View`) stays a separate, intentional setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UiScale(u16);

impl UiScale {
    /// The supported presets, including the common 133% HiDPI step.
    pub const PRESETS: [u16; 6] = [100, 125, 133, 150, 175, 200];

    /// Unscaled layout (`100%`).
    pub const DEFAULT: Self = Self(100);

    /// Scale in percent (`100` = unscaled).
    #[must_use]
    pub const fn percent(self) -> u16 {
        self.0
    }

    /// Scale in thousandths (`1_000` = unscaled).
    #[must_use]
    pub fn milli(self) -> i32 {
        i32::from(self.0) * 10
    }

    /// Nearest supported preset for a requested percentage. Out-of-range values clamp to the
    /// closest preset instead of producing a scale the renderer was never qualified at.
    #[must_use]
    pub fn from_percent(percent: u16) -> Self {
        Self(
            Self::PRESETS
                .iter()
                .copied()
                .min_by_key(|p| (i32::from(*p) - i32::from(percent)).abs())
                .unwrap_or(100),
        )
    }

    /// Parses `150`, `150%`, or `1.5` style values; `None` leaves the current scale untouched.
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let text = input.trim();
        let text = text.strip_suffix('%').unwrap_or(text).trim();
        if text == "2" || text == "2.0" {
            return Some(Self(200));
        }
        if let Ok(percent) = text.parse::<u16>() {
            return (50..=300)
                .contains(&percent)
                .then(|| Self::from_percent(percent));
        }
        let fraction = text.strip_prefix("1.")?;
        if fraction.is_empty()
            || fraction.len() > 2
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let hundredths = fraction
            .bytes()
            .fold(0u16, |acc, b| acc * 10 + u16::from(b - b'0'));
        if fraction.len() == 1 {
            Some(Self::from_percent(100 + hundredths * 10))
        } else {
            Some(Self::from_percent(100 + hundredths))
        }
    }

    /// Next preset, wrapping from `200%` back to `100%`.
    #[must_use]
    pub const fn next(self) -> Self {
        match self.0 {
            100 => Self(125),
            125 => Self(133),
            133 => Self(150),
            150 => Self(175),
            175 => Self(200),
            _ => Self(100),
        }
    }

    /// Previous preset, wrapping from `100%` back to `200%`.
    #[must_use]
    pub const fn previous(self) -> Self {
        match self.0 {
            200 => Self(175),
            175 => Self(150),
            150 => Self(133),
            133 => Self(125),
            125 => Self(100),
            _ => Self(200),
        }
    }

    /// Moves `steps` presets; negative steps scale down.
    #[must_use]
    pub fn step(self, steps: i32) -> Self {
        match steps {
            0.. => (0..steps).fold(self, |s, _| s.next()),
            _ => (0..-steps).fold(self, |s, _| s.previous()),
        }
    }

    /// Physical pixels to logical pixels for one window axis, preserving the surface's minimum
    /// framebuffer dimensions so a very small window still has a paintable client area.
    #[must_use]
    fn logical_axis(self, physical: u16, minimum: u16) -> u16 {
        let scaled = (i32::from(physical) * 1_000 + self.milli() / 2) / self.milli();
        scaled
            .clamp(i32::from(minimum), i32::from(u16::MAX))
            .try_into()
            .unwrap_or(u16::MAX)
    }

    /// Logical pixel size of a physical client area at this scale.
    #[must_use]
    pub fn logical_size(self, physical: (u16, u16)) -> (u16, u16) {
        (
            self.logical_axis(physical.0, 320),
            self.logical_axis(physical.1, 240),
        )
    }
}

impl Default for UiScale {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// OS-level window screen-capture exclusion status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureExclusionState {
    /// OS windowing API confirmed capture exclusion (`WDA_EXCLUDEFROMCAPTURE` / `NSWindowSharingNone`).
    Excluded,
    /// OS or display protocol provides no enforced capture exclusion.
    Unsupported(&'static str),
}

/// OS-level secure keyboard input status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecureInputState {
    /// OS secure keyboard input mode is active (`EnableSecureEventInput`).
    Active,
    /// OS or display protocol offers no secure-input isolation mode here.
    Unsupported(&'static str),
}

/// Live OS window security telemetry surfaced into [`crate::panel::Facts`] (`UI-SPEC.md` §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSecurityInfo {
    /// Window capture-exclusion state.
    pub capture_exclusion: CaptureExclusionState,
    /// Secure keyboard input isolation state.
    pub secure_input: SecureInputState,
    /// Clipboard lifetime guarantee on this backend.
    pub clipboard_policy: &'static str,
}

impl Default for WindowSecurityInfo {
    fn default() -> Self {
        Self {
            capture_exclusion: CaptureExclusionState::Unsupported("not queried"),
            secure_input: SecureInputState::Unsupported("not queried"),
            clipboard_policy: "cleared on focus loss & exit",
        }
    }
}

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
    /// OS requested sleep/suspend or delivered a suspend tripwire notification.
    SystemSuspend,
    /// Active dead-key or OS IME preedit string (`""` clears preedit composition).
    Preedit(String),
    /// Multi-codepoint text committed by OS IME or dead-key composition.
    CommitText(String),
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

    /// Reports live OS capture-exclusion, secure-input, and clipboard policy status.
    fn security_info(&self) -> WindowSecurityInfo {
        WindowSecurityInfo::default()
    }
}

fn ui_scale_config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return Some(
            PathBuf::from(dir)
                .join("holonomy")
                .join("display-scale.txt"),
        );
    }
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(|h| {
            PathBuf::from(h)
                .join(".config")
                .join("holonomy")
                .join("display-scale.txt")
        })
        .or_else(|| {
            std::env::var_os("APPDATA")
                .filter(|s| !s.is_empty())
                .map(|a| PathBuf::from(a).join("holonomy").join("display-scale.txt"))
        })
}

/// Resolves the startup display scale: `HOLONOMY_UI_SCALE` overrides the persisted preference.
#[must_use]
pub fn load_ui_scale() -> UiScale {
    if let Some(value) = std::env::var("HOLONOMY_UI_SCALE")
        .ok()
        .and_then(|s| UiScale::parse(&s))
    {
        return value;
    }
    let Some(path) = ui_scale_config_path() else {
        return UiScale::DEFAULT;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| UiScale::parse(&s))
        .unwrap_or_default()
}

/// Persists a display-scale choice so it applies to every subsequently opened window.
pub fn save_ui_scale(scale: UiScale) {
    let Some(path) = ui_scale_config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{}\n", scale.percent()));
}

/// Applies [`UiScale`] to an entire native window: the renderer sees a smaller logical viewport,
/// pointer events are divided down into that viewport, and the finished framebuffer is resampled
/// back to the physical client area. Every consumer of [`DesktopWindow`] therefore shares one
/// coordinate system, including the in-window unlock prompt and runtime scale changes.
pub struct ScaledWindow {
    inner: Box<dyn DesktopWindow>,
    scale: UiScale,
    logical: (u16, u16),
    pending_resize: Option<(u16, u16)>,
    scratch: Vec<u32>,
    columns: Vec<(usize, usize, u32)>,
}

impl ScaledWindow {
    /// Wraps a native window at `scale`. The native window keeps its physical client size.
    #[must_use]
    pub fn new(inner: Box<dyn DesktopWindow>, scale: UiScale) -> Self {
        let logical = scale.logical_size(inner.size());
        Self {
            inner,
            scale,
            logical,
            pending_resize: None,
            scratch: Vec::new(),
            columns: Vec::new(),
        }
    }

    /// Currently applied display scale.
    #[must_use]
    pub const fn scale(&self) -> UiScale {
        self.scale
    }

    /// Changes the scale for this window and every frame it presents. A synthetic `Resize` is
    /// delivered on the next poll so the editor reflows to the new logical viewport.
    pub fn set_scale(&mut self, scale: UiScale) -> bool {
        if scale == self.scale {
            return false;
        }
        self.scale = scale;
        self.logical = scale.logical_size(self.inner.size());
        self.pending_resize = Some(self.logical);
        true
    }

    /// Access to the underlying native window for operations that are scale-independent.
    pub fn as_window_mut(&mut self) -> &mut dyn DesktopWindow {
        &mut *self.inner
    }

    fn device_to_logical(&self, device: i16) -> i16 {
        if self.scale.milli() == 1_000 {
            return device;
        }
        let milli = self.scale.milli();
        let value = i32::from(device) * 1_000;
        let logical = if value >= 0 {
            (value + milli / 2) / milli
        } else {
            (value - milli / 2) / milli
        };
        logical
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
            .try_into()
            .unwrap_or(i16::MAX)
    }
}

impl DesktopWindow for ScaledWindow {
    fn size(&self) -> (u16, u16) {
        self.logical
    }

    fn dpi(&self) -> i64 {
        self.inner.dpi()
    }

    fn present_argb(
        &mut self,
        pixels: &[u32],
        width: u16,
        height: u16,
    ) -> Result<(), Box<dyn Error>> {
        let (physical_w, physical_h) = self.inner.size();
        let milli = self.scale.milli();
        if milli == 1_000 && width == physical_w && height == physical_h {
            return self.inner.present_argb(pixels, width, height);
        }
        if width == 0 || height == 0 || physical_w == 0 || physical_h == 0 {
            return Ok(());
        }
        let src_w = usize::from(width);
        let src_h = usize::from(height);
        if pixels.len() < src_w * src_h {
            return Err("framebuffer smaller than the logical viewport".into());
        }
        self.scratch
            .resize(usize::from(physical_w) * usize::from(physical_h), 0);

        if milli % 1_000 == 0 {
            // Integer scales repeat each logical pixel exactly: text stays crisp at 200%.
            let factor = usize::try_from(milli / 1_000).unwrap_or(1).max(1);
            let physical_stride = usize::from(physical_w);
            if factor == 1 {
                // `100%`: the logical framebuffer is already the physical one, so this is a plain
                // row copy. Keeps the common case out of any per-pixel work at all.
                let copy_w = physical_stride.min(src_w);
                for y in 0..usize::from(physical_h) {
                    let sy = y.min(src_h - 1) * src_w;
                    let dy = y * physical_stride;
                    let row = &mut self.scratch[dy..dy + physical_stride];
                    row[..copy_w].copy_from_slice(&pixels[sy..sy + copy_w]);
                }
            } else {
                for y in 0..usize::from(physical_h) {
                    let sy = (y / factor).min(src_h - 1) * src_w;
                    let dy = y * physical_stride;
                    let row = &mut self.scratch[dy..dy + physical_stride];
                    for (x, out) in row.iter_mut().enumerate() {
                        *out = pixels[sy + (x / factor).min(src_w - 1)];
                    }
                }
            }
        } else {
            // Non-integer steps use bilinear sampling so glyph edges do not acquire hard,
            // uneven pixel widths at 125/133/150/175%. Weights are fixed-point (`1/256`) and the
            // column mapping is computed once per frame, so a keystroke never pays for
            // per-pixel floating point across the whole window.
            let physical_stride = usize::from(physical_w);
            self.columns.clear();
            self.columns.reserve(physical_stride);
            for x in 0..physical_stride {
                let fx = ((x as f64 + 0.5) * 1_000.0 / f64::from(milli) - 0.5)
                    .clamp(0.0, (src_w - 1) as f64);
                let x0 = fx as usize;
                let x1 = (x0 + 1).min(src_w - 1);
                let weight = ((fx - x0 as f64) * 256.0).round() as u32;
                self.columns.push((x0, x1, weight.min(256)));
            }
            for y in 0..usize::from(physical_h) {
                let fy = ((y as f64 + 0.5) * 1_000.0 / f64::from(milli) - 0.5)
                    .clamp(0.0, (src_h - 1) as f64);
                let y0 = fy as usize;
                let y1 = (y0 + 1).min(src_h - 1);
                let top_weight = 256 - ((fy - y0 as f64) * 256.0).round().min(256.0) as u32;
                let top_row = &pixels[y0 * src_w..(y0 + 1) * src_w];
                let bottom_row = &pixels[y1 * src_w..(y1 + 1) * src_w];
                let dy = y * physical_stride;
                let out_row = &mut self.scratch[dy..dy + physical_stride];
                for (out, &(x0, x1, wx)) in out_row.iter_mut().zip(&self.columns) {
                    let left_weight = 256 - wx;
                    let p00 = top_row[x0];
                    let p10 = top_row[x1];
                    let p01 = bottom_row[x0];
                    let p11 = bottom_row[x1];
                    let mut value = 0u32;
                    for shift in [16, 8, 0] {
                        let top =
                            ((p00 >> shift) & 0xff) * left_weight + ((p10 >> shift) & 0xff) * wx;
                        let bottom =
                            ((p01 >> shift) & 0xff) * left_weight + ((p11 >> shift) & 0xff) * wx;
                        let sample = (top * top_weight + bottom * (256 - top_weight)) >> 16;
                        value |= (sample & 0xff) << shift;
                    }
                    *out = value;
                }
            }
        }
        self.inner
            .present_argb(&self.scratch, physical_w, physical_h)
    }

    fn poll_events(&mut self, timeout_ms: i32) -> Result<Vec<DesktopEvent>, Box<dyn Error>> {
        let mut events = self.inner.poll_events(timeout_ms)?;
        for event in &mut events {
            match event {
                DesktopEvent::MouseDown { x, y, .. } | DesktopEvent::MouseDrag { x, y } => {
                    *x = self.device_to_logical(*x);
                    *y = self.device_to_logical(*y);
                }
                DesktopEvent::Resize { width, height } => {
                    let (w, h) = self.scale.logical_size((*width, *height));
                    self.logical = (w, h);
                    *width = w;
                    *height = h;
                }
                _ => {}
            }
        }
        if let Some((w, h)) = self.pending_resize.take() {
            if !events.iter().any(
                |event| matches!(event, DesktopEvent::Resize { width, height } if *width == w && *height == h),
            ) {
                events.push(DesktopEvent::Resize { width: w, height: h });
            }
        }
        Ok(events)
    }

    fn set_clipboard(&mut self, utf8: &[u8]) -> Result<(), Box<dyn Error>> {
        self.inner.set_clipboard(utf8)
    }

    fn request_clipboard(&mut self) -> Result<(), Box<dyn Error>> {
        self.inner.request_clipboard()
    }

    fn clear_clipboard(&mut self) {
        self.inner.clear_clipboard();
    }

    fn security_info(&self) -> WindowSecurityInfo {
        self.inner.security_info()
    }
}

/// Chooses the source crop and destination size for presenting one framebuffer.
///
/// A backend is handed a *logical* viewport whenever a fractional display scale is active, and at
/// integer scales such as 200 % the renderer keeps that logical buffer and presents it at its own
/// resolution. The client area can therefore be smaller than the framebuffer, and a backend that
/// hands both rectangles to a scaler asks the OS to *smooth-stretch* the image — softening every
/// glyph and icon a second time on top of the deliberate scale choice. Cropping the oversized
/// framebuffer to the visible client area keeps the pixels exactly as rasterized.
///
/// Returns `(width, height, source_x, source_y)` for the blit, or `None` when there is nothing to
/// present. Scaling *up* is left to the backend: that only happens when the window is larger than
/// the renderer's logical viewport, where shrinking the viewport instead would lose content.
#[must_use]
pub fn blit_viewport(client: (u16, u16), framebuffer: (u16, u16)) -> Option<(u16, u16, u16, u16)> {
    let (cw, ch) = client;
    let (fw, fh) = framebuffer;
    if cw == 0 || ch == 0 || fw == 0 || fh == 0 {
        return None;
    }
    if fw <= cw && fh <= ch {
        return Some((fw, fh, 0, 0));
    }
    let w = cw.min(fw);
    let h = ch.min(fh);
    Some((w, h, (fw - w) / 2, (fh - h) / 2))
}

/// Converts a window size the caller asked for in layout units into physical pixels for a backend
/// that reports true device pixels.
///
/// Enabling DPI awareness stops the OS from stretching the window, so a client area requested in
/// layout units has to be multiplied by the display scale or the window would open at a fraction of
/// its intended apparent size on a scaled display. 96 dpi is the reference, so the identity holds
/// everywhere except HiDPI. The result is clamped into `u16`, the widest a client area can be here.
#[must_use]
pub fn physical_size_hint(logical: (u16, u16), dpi: i64) -> (u16, u16) {
    fn axis(value: u16, dpi: i64) -> u16 {
        // Round to nearest physical pixel so 125 % of an odd size does not creep by one.
        let scaled = (i64::from(value) * dpi.clamp(72, 288) + 48) / 96;
        u16::try_from(scaled.clamp(1, i64::from(u16::MAX))).unwrap_or(u16::MAX)
    }
    (axis(logical.0, dpi), axis(logical.1, dpi))
}

/// Opts the process into per-monitor DPI awareness where the platform needs an explicit opt-in.
///
/// Win32 processes are DPI-unaware by default: Windows reports a 96 dpi client area and then
/// bitmap-stretches the finished window to the real display scale, which softens every glyph and
/// icon next to native apps. The call must precede the first window, is idempotent, and is a no-op
/// on platforms that derive scale from the display server instead (X11, Cocoa).
pub fn declare_dpi_awareness() {
    #[cfg(target_os = "windows")]
    crate::win32::enable_per_monitor_dpi_awareness();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    type Presentation = Rc<RefCell<(Vec<u32>, (u16, u16))>>;

    #[derive(Default)]
    struct MockWindow {
        size: (u16, u16),
        events: VecDeque<DesktopEvent>,
        presented: Presentation,
    }

    impl DesktopWindow for MockWindow {
        fn size(&self) -> (u16, u16) {
            self.size
        }

        fn dpi(&self) -> i64 {
            96
        }

        fn present_argb(
            &mut self,
            pixels: &[u32],
            width: u16,
            height: u16,
        ) -> Result<(), Box<dyn Error>> {
            *self.presented.borrow_mut() = (pixels.to_vec(), (width, height));
            Ok(())
        }

        fn poll_events(&mut self, _timeout_ms: i32) -> Result<Vec<DesktopEvent>, Box<dyn Error>> {
            Ok(self.events.drain(..).collect())
        }

        fn set_clipboard(&mut self, _utf8: &[u8]) -> Result<(), Box<dyn Error>> {
            Ok(())
        }

        fn request_clipboard(&mut self) -> Result<(), Box<dyn Error>> {
            Ok(())
        }

        fn clear_clipboard(&mut self) {}
    }

    fn window(scale: UiScale) -> (ScaledWindow, Presentation) {
        let inner = MockWindow {
            size: (1_120, 780),
            ..Default::default()
        };
        let presented = Rc::clone(&inner.presented);
        (ScaledWindow::new(Box::new(inner), scale), presented)
    }

    #[test]
    fn presets_cover_the_requested_range_and_cycle() {
        assert_eq!(UiScale::PRESETS, [100, 125, 133, 150, 175, 200]);
        assert_eq!(UiScale::parse("150%"), Some(UiScale::from_percent(150)));
        assert_eq!(UiScale::parse("1.33"), Some(UiScale::from_percent(133)));
        assert_eq!(UiScale::parse("2.0"), Some(UiScale::from_percent(200)));
        let mut scale = UiScale::DEFAULT;
        for &preset in &UiScale::PRESETS {
            assert_eq!(scale.percent(), preset);
            scale = scale.next();
        }
        assert_eq!(scale, UiScale::DEFAULT);
        assert_eq!(UiScale::DEFAULT.previous().percent(), 200);
        assert_eq!(UiScale::from_percent(140).step(-1).percent(), 125);
    }

    #[test]
    fn logical_viewport_matches_each_preset() {
        let physical = (1_120, 780);
        let expected = [
            (1_120, 780),
            (896, 624),
            (842, 586),
            (747, 520),
            (640, 446),
            (560, 390),
        ];
        for (preset, size) in UiScale::PRESETS.iter().zip(expected) {
            assert_eq!(UiScale::from_percent(*preset).logical_size(physical), size);
        }
    }

    #[test]
    fn pointer_and_resize_events_use_the_logical_coordinate_space() {
        let mut inner = MockWindow {
            size: (1_120, 780),
            ..Default::default()
        };
        inner.events.push_back(DesktopEvent::MouseDown {
            x: 150,
            y: 90,
            modifiers: Modifiers::none(),
        });
        inner
            .events
            .push_back(DesktopEvent::MouseDrag { x: 30, y: 60 });
        inner.events.push_back(DesktopEvent::Resize {
            width: 1_200,
            height: 900,
        });
        let mut win = ScaledWindow::new(Box::new(inner), UiScale::from_percent(150));
        assert_eq!(win.size(), (747, 520));
        let events = win.poll_events(0).expect("poll");
        assert!(matches!(
            events.first(),
            Some(DesktopEvent::MouseDown { x: 100, y: 60, .. })
        ));
        assert!(matches!(
            events.get(1),
            Some(DesktopEvent::MouseDrag { x: 20, y: 40 })
        ));
        assert!(matches!(
            events.get(2),
            Some(DesktopEvent::Resize {
                width: 800,
                height: 600
            })
        ));
        assert!(win.set_scale(UiScale::from_percent(200)));
        let events = win.poll_events(0).expect("poll");
        assert!(matches!(
            events.last(),
            Some(DesktopEvent::Resize {
                width: 560,
                height: 390
            })
        ));
    }

    #[test]
    fn blit_viewport_never_stretches_a_matching_framebuffer() {
        // The 1:1 case must reach the screen untouched: source and destination identical.
        assert_eq!(
            blit_viewport((800, 600), (800, 600)),
            Some((800, 600, 0, 0))
        );
        // A smaller framebuffer is still presented at its own size, anchored top-left.
        assert_eq!(
            blit_viewport((800, 600), (747, 520)),
            Some((747, 520, 0, 0))
        );
    }

    #[test]
    fn blit_viewport_crops_an_oversized_framebuffer_instead_of_shrinking_it() {
        // A window opened at a 200 % logical viewport stays logical-sized after the user shrinks
        // the frame; presenting it whole would smooth-stretch every glyph. Crop instead.
        let crop = blit_viewport((1000, 700), (1120, 780));
        assert_eq!(crop, Some((1000, 700, 60, 40)));
        // The crop is centred, so equal margins are dropped on both sides.
        let (w, h, x, y) = crop.expect("crop");
        assert_eq!(1_120 - w, 2 * x);
        assert_eq!(780 - h, 2 * y);
    }

    #[test]
    fn physical_size_hint_preserves_apparent_size_across_scales() {
        // 96 dpi is the reference, so a standard display gets exactly what was requested.
        assert_eq!(physical_size_hint((1_120, 780), 96), (1_120, 780));
        // A 200 % display needs twice the device pixels for the same apparent window.
        assert_eq!(physical_size_hint((1_120, 780), 192), (2_240, 1_560));
        assert_eq!(physical_size_hint((1_120, 780), 144), (1_680, 1_170));
        // Rounding is to nearest, and the result can never exceed a `u16` client area.
        assert_eq!(physical_size_hint((801, 801), 144), (1_202, 1_202));
        assert_eq!(
            physical_size_hint((u16::MAX, u16::MAX), 288),
            (u16::MAX, u16::MAX)
        );
        assert_eq!(physical_size_hint((0, 0), 96), (1, 1));
    }

    #[test]
    fn blit_viewport_rejects_empty_geometry() {
        assert_eq!(blit_viewport((0, 0), (800, 600)), None);
        assert_eq!(blit_viewport((800, 600), (0, 600)), None);
    }

    #[test]
    fn integer_scale_repeats_pixels_without_blurring() {
        let (mut win, presented) = window(UiScale::from_percent(200));
        win.present_argb(&[0x00ff_0000, 0x0000_00ff], 2, 1)
            .expect("present");
        let presented = presented.borrow();
        let (pixels, size) = &*presented;
        assert_eq!(*size, (1_120, 780));
        assert_eq!(pixels[0], 0x00ff_0000);
        assert_eq!(pixels[1], 0x00ff_0000);
        assert_eq!(pixels[2], 0x0000_00ff);
    }

    #[test]
    fn fractional_scale_keeps_flat_areas_exactly_flat() {
        // Fixed-point weights must sum to exactly 256, otherwise a uniform page would gain
        // visible banding or drift darker/lighter at 125/133/150/175%.
        for preset in [125, 133, 150, 175] {
            let (mut win, presented) = window(UiScale::from_percent(preset));
            let page = 0x00f7_f8fa;
            win.present_argb(&[page; 4], 2, 2).expect("present");
            let presented = presented.borrow();
            let (pixels, _) = &*presented;
            assert!(pixels.iter().all(|&px| px == page), "{preset}% drifted");
        }
    }

    #[test]
    fn fractional_scale_keeps_edges_monotonic() {
        // A black-to-white horizontal ramp must not overshoot or invert after resampling.
        for preset in [125, 133, 150, 175] {
            let (mut win, presented) = window(UiScale::from_percent(preset));
            let ramp: Vec<u32> = (0..64u32).map(|i| (i * 4) << 16).collect();
            win.present_argb(&ramp, 64, 1).expect("present");
            let presented = presented.borrow();
            let (pixels, size) = &*presented;
            let row = &pixels[..usize::from(size.0)];
            assert!(row.windows(2).all(|w| w[0] <= w[1]), "{preset}% inverted");
            assert_eq!(row[0] >> 16, 0, "{preset}% clipped left edge");
        }
    }

    #[test]
    fn fractional_scale_presents_at_the_physical_size() {
        let (mut win, presented) = window(UiScale::from_percent(133));
        win.present_argb(&[0x00ff_0000; 4], 2, 2).expect("present");
        let presented = presented.borrow();
        let (pixels, size) = &*presented;
        assert_eq!(*size, (1_120, 780));
        assert_eq!(pixels.len(), 1_120 * 780);
    }
}
