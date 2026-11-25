//! Pure-FFI Win32 desktop backend (`user32.dll` + `gdi32.dll` + `kernel32.dll`) with zero
//! external Windows crates so `holonomy-gui` builds and runs natively under `--locked`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::error::Error;
use std::ffi::c_void;
use std::fmt;
use std::ptr::{null, null_mut};
use std::rc::Rc;

use zeroize::{Zeroize, Zeroizing};

use crate::platform::{DesktopEvent, DesktopWindow, MAX_CLIPBOARD_BYTES};
use crate::{Key, Modifiers};

type Hwnd = *mut c_void;
type Hdc = *mut c_void;
type Hinstance = *mut c_void;
type Hcursor = *mut c_void;
type Hglobal = *mut c_void;
type Lresult = isize;
type Wparam = usize;
type Lparam = isize;
type Bool = i32;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const CW_USEDEFAULT: i32 = 0x8000_0000_u32 as i32;
const SW_SHOWNORMAL: i32 = 1;
const CS_HREDRAW: u32 = 0x0002;
const CS_VREDRAW: u32 = 0x0001;
const IDC_ARROW: *const u16 = 32512 as *const u16;
const COLOR_WINDOW: usize = 5;

const WM_CLOSE: u32 = 0x0010;
const WM_DESTROY: u32 = 0x0002;
const WM_SIZE: u32 = 0x0005;
const WM_PAINT: u32 = 0x000F;
const WM_KILLFOCUS: u32 = 0x0008;
const WM_KEYDOWN: u32 = 0x0100;
const WM_CHAR: u32 = 0x0102;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_DESTROYCLIPBOARD: u32 = 0x0307;

const PM_REMOVE: u32 = 0x0001;
const QS_ALLINPUT: u32 = 0x04FF;
const LOGPIXELSX: i32 = 88;
const DIB_RGB_COLORS: u32 = 0;
const SRCCOPY: u32 = 0x00CC_0020;
const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_SHIFT: i32 = 0x10;
const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;
const VK_ESCAPE: u16 = 0x1B;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_DELETE: u16 = 0x2E;

#[repr(C)]
struct WndClassExW {
    cb_size: u32,
    style: u32,
    lpfn_wnd_proc: unsafe extern "system" fn(Hwnd, u32, Wparam, Lparam) -> Lresult,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: Hinstance,
    h_icon: *mut c_void,
    h_cursor: Hcursor,
    hbr_background: *mut c_void,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: *mut c_void,
}

#[repr(C)]
#[derive(Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Default)]
struct Msg {
    hwnd: Hwnd,
    message: u32,
    w_param: Wparam,
    l_param: Lparam,
    time: u32,
    pt: Point,
}

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct BitmapInfoHeader {
    bi_size: u32,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: u32,
    bi_size_image: u32,
    bi_xpels_per_meter: i32,
    bi_ypels_per_meter: i32,
    bi_clr_used: u32,
    bi_clr_important: u32,
}

#[repr(C)]
struct BitmapInfo {
    bmi_header: BitmapInfoHeader,
    bmi_colors: [u32; 3],
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(lp_module_name: *const u16) -> Hinstance;
    fn GlobalAlloc(u_flags: u32, dw_bytes: usize) -> Hglobal;
    fn GlobalFree(h_mem: Hglobal) -> Hglobal;
    fn GlobalLock(h_mem: Hglobal) -> *mut c_void;
    fn GlobalUnlock(h_mem: Hglobal) -> Bool;
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(unnamed_param1: *const WndClassExW) -> u16;
    fn CreateWindowExW(
        dw_ex_style: u32,
        lp_class_name: *const u16,
        lp_window_name: *const u16,
        dw_style: u32,
        x: i32,
        y: i32,
        n_width: i32,
        n_height: i32,
        h_wnd_parent: Hwnd,
        h_menu: *mut c_void,
        h_instance: Hinstance,
        lp_param: *mut c_void,
    ) -> Hwnd;
    fn DestroyWindow(h_wnd: Hwnd) -> Bool;
    fn ShowWindow(h_wnd: Hwnd, n_cmd_show: i32) -> Bool;
    fn DefWindowProcW(h_wnd: Hwnd, msg: u32, w_param: Wparam, l_param: Lparam) -> Lresult;
    fn LoadCursorW(h_instance: Hinstance, lp_cursor_name: *const u16) -> Hcursor;
    fn GetClientRect(h_wnd: Hwnd, lp_rect: *mut Rect) -> Bool;
    fn GetDC(h_wnd: Hwnd) -> Hdc;
    fn ReleaseDC(h_wnd: Hwnd, h_dc: Hdc) -> i32;
    fn PeekMessageW(
        lp_msg: *mut Msg,
        h_wnd: Hwnd,
        w_msg_filter_min: u32,
        w_msg_filter_max: u32,
        w_remove_msg: u32,
    ) -> Bool;
    fn TranslateMessage(lp_msg: *const Msg) -> Bool;
    fn DispatchMessageW(lp_msg: *const Msg) -> Lresult;
    fn MsgWaitForMultipleObjectsEx(
        n_count: u32,
        p_handles: *const *mut c_void,
        dw_milliseconds: u32,
        dw_wake_mask: u32,
        dw_flags: u32,
    ) -> u32;
    fn GetKeyState(n_virt_key: i32) -> i16;
    fn SetPropW(h_wnd: Hwnd, lp_string: *const u16, h_data: *mut c_void) -> Bool;
    fn GetPropW(h_wnd: Hwnd, lp_string: *const u16) -> *mut c_void;
    fn RemovePropW(h_wnd: Hwnd, lp_string: *const u16) -> *mut c_void;
    fn OpenClipboard(h_wnd_new_owner: Hwnd) -> Bool;
    fn EmptyClipboard() -> Bool;
    fn SetClipboardData(u_format: u32, h_mem: Hglobal) -> Hglobal;
    fn GetClipboardData(u_format: u32) -> Hglobal;
    fn CloseClipboard() -> Bool;
}

#[link(name = "gdi32")]
extern "system" {
    fn GetDeviceCaps(hdc: Hdc, index: i32) -> i32;
    fn StretchDIBits(
        hdc: Hdc,
        x_dest: i32,
        y_dest: i32,
        dest_width: i32,
        dest_height: i32,
        x_src: i32,
        y_src: i32,
        src_width: i32,
        src_height: i32,
        lp_bits: *const c_void,
        lpbmi: *const BitmapInfo,
        i_usage: u32,
        rop: u32,
    ) -> i32;
}

/// Errors returned by the Win32 native window backend.
#[derive(Debug)]
pub struct Win32Error(pub String);

impl fmt::Display for Win32Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Win32 GUI error: {}", self.0)
    }
}

impl Error for Win32Error {}

struct WinQueue {
    events: VecDeque<DesktopEvent>,
    left_down: bool,
    owns_clipboard: bool,
    pending_surrogate: Option<u16>,
    width: u16,
    height: u16,
}

fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

const PROP_NAME: [u16; 13] = [
    b'H' as u16,
    b'O' as u16,
    b'L' as u16,
    b'O' as u16,
    b'N' as u16,
    b'O' as u16,
    b'M' as u16,
    b'Y' as u16,
    b'_' as u16,
    b'P' as u16,
    b'T' as u16,
    b'R' as u16,
    0,
];

fn current_modifiers() -> Modifiers {
    // SAFETY: `GetKeyState` reads per-thread virtual-key state safely.
    unsafe {
        Modifiers {
            shift: (GetKeyState(VK_SHIFT) & (0x8000_u16 as i16)) != 0,
            control: (GetKeyState(VK_CONTROL) & (0x8000_u16 as i16)) != 0,
            alt: (GetKeyState(VK_MENU) & (0x8000_u16 as i16)) != 0,
        }
    }
}

fn translate_virtual_key(vk: u16, mods: Modifiers) -> Option<Key> {
    match vk {
        VK_BACK => Some(Key::Backspace),
        VK_DELETE => Some(Key::Delete),
        VK_TAB => Some(Key::Tab),
        VK_RETURN => Some(Key::Enter),
        VK_ESCAPE => Some(Key::Escape),
        VK_LEFT => Some(Key::Left),
        VK_RIGHT => Some(Key::Right),
        VK_UP => Some(Key::Up),
        VK_DOWN => Some(Key::Down),
        VK_HOME => Some(Key::Home),
        VK_END => Some(Key::End),
        VK_PRIOR => Some(Key::PageUp),
        VK_NEXT => Some(Key::PageDown),
        0x41..=0x5A if mods.control => {
            let lower = (vk as u8 - 0x41 + b'a') as char;
            Some(Key::Character(lower))
        }
        0xBB | 0x6B if mods.control => Some(Key::Character('=')),
        0xBD | 0x6D if mods.control => Some(Key::Character('-')),
        0xBF if mods.control => Some(Key::Character('/')),
        _ => None,
    }
}

fn lparam_xy(lp: Lparam) -> (i16, i16) {
    let raw = lp as usize;
    (
        (raw & 0xFFFF) as u16 as i16,
        ((raw >> 16) & 0xFFFF) as u16 as i16,
    )
}

fn decode_wm_char(pending_surrogate: &mut Option<u16>, unit: u16) -> Option<char> {
    if (0xD800..=0xDBFF).contains(&unit) {
        *pending_surrogate = Some(unit);
        return None;
    }
    if (0xDC00..=0xDFFF).contains(&unit) {
        let hi = pending_surrogate.take()?;
        return std::char::decode_utf16([hi, unit]).next()?.ok();
    }
    *pending_surrogate = None;
    char::from_u32(u32::from(unit))
}

unsafe extern "system" fn wnd_proc(
    hwnd: Hwnd,
    msg: u32,
    w_param: Wparam,
    l_param: Lparam,
) -> Lresult {
    // SAFETY: `GetPropW` retrieves the `Rc<RefCell<WinQueue>>` installed during `open`.
    let raw_ptr = unsafe { GetPropW(hwnd, PROP_NAME.as_ptr()) } as *const RefCell<WinQueue>;
    if !raw_ptr.is_null() {
        // SAFETY: `raw_ptr` stays alive until `Drop` removes the property and balances `into_raw`.
        let q_ref = unsafe { &*raw_ptr };
        let mut handled_close = false;
        {
            let mut q = q_ref.borrow_mut();
            match msg {
                WM_CLOSE => {
                    q.events.push_back(DesktopEvent::CloseRequested);
                    handled_close = true;
                }
                WM_PAINT => {
                    q.events.push_back(DesktopEvent::Expose);
                }
                WM_KILLFOCUS => {
                    q.left_down = false;
                    q.pending_surrogate = None;
                    q.events.push_back(DesktopEvent::FocusOut);
                }
                WM_DESTROYCLIPBOARD => {
                    q.owns_clipboard = false;
                }
                WM_SIZE => {
                    let w = ((l_param as usize) & 0xFFFF) as u16;
                    let h = (((l_param as usize) >> 16) & 0xFFFF) as u16;
                    if w > 0 && h > 0 && (w, h) != (q.width, q.height) {
                        q.width = w;
                        q.height = h;
                        q.events.push_back(DesktopEvent::Resize {
                            width: w,
                            height: h,
                        });
                    }
                }
                WM_LBUTTONDOWN => {
                    let (x, y) = lparam_xy(l_param);
                    q.left_down = true;
                    q.events.push_back(DesktopEvent::MouseDown {
                        x,
                        y,
                        modifiers: current_modifiers(),
                    });
                }
                WM_MOUSEMOVE => {
                    if q.left_down {
                        let (x, y) = lparam_xy(l_param);
                        q.events.push_back(DesktopEvent::MouseDrag { x, y });
                    }
                }
                WM_LBUTTONUP => {
                    q.left_down = false;
                    q.events.push_back(DesktopEvent::MouseUp);
                }
                WM_MOUSEWHEEL => {
                    let delta = ((w_param >> 16) as u16) as i16;
                    let lines = if delta > 0 { -3 } else { 3 };
                    q.events
                        .push_back(DesktopEvent::Scroll { delta_lines: lines });
                }
                WM_KEYDOWN => {
                    let mods = current_modifiers();
                    let vk = (w_param & 0xFFFF) as u16;
                    if let Some(key) = translate_virtual_key(vk, mods) {
                        q.events.push_back(DesktopEvent::Key {
                            key,
                            modifiers: mods,
                        });
                    }
                }
                WM_CHAR => {
                    let mods = current_modifiers();
                    let unit = (w_param & 0xFFFF) as u16;
                    if let Some(ch) = decode_wm_char(&mut q.pending_surrogate, unit) {
                        if !ch.is_control() && !(mods.control && !mods.alt) {
                            let ev_mods = if mods.control && mods.alt {
                                // Windows AltGr sets Ctrl+Alt; normalize so editor inserts text.
                                Modifiers {
                                    shift: mods.shift,
                                    control: false,
                                    alt: false,
                                }
                            } else {
                                mods
                            };
                            q.events.push_back(DesktopEvent::Key {
                                key: Key::Character(ch),
                                modifiers: ev_mods,
                            });
                        }
                    }
                }
                WM_DESTROY => {}
                _ => {}
            }
        }
        if handled_close {
            return 0;
        }
    }
    // SAFETY: `q` lock is released before calling `DefWindowProcW`, preventing re-entrancy panics.
    unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) }
}

/// Native Win32 desktop window presenting a 32-bpp `0x00RRGGBB` framebuffer via `StretchDIBits`.
pub struct Win32Window {
    hwnd: Hwnd,
    queue: Rc<RefCell<WinQueue>>,
    dpi: i64,
    prop_raw: *const RefCell<WinQueue>,
}

impl Win32Window {
    /// Opens and displays a Win32 window sized `width × height` with UTF-16 `title`.
    ///
    /// # Errors
    ///
    /// Returns [`Win32Error`] if window class registration or creation fails.
    pub fn open(title: &str, width: u16, height: u16) -> Result<Self, Win32Error> {
        let w = width.max(320);
        let h = height.max(240);
        let class_name = wide_null("HolonomyQuietMarkdownWin32");
        let title_w = wide_null(title);

        // SAFETY: `GetModuleHandleW(null)` returns current executable `HINSTANCE`.
        let h_inst = unsafe { GetModuleHandleW(null()) };
        let wc = WndClassExW {
            cb_size: std::mem::size_of::<WndClassExW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfn_wnd_proc: wnd_proc,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: h_inst,
            h_icon: null_mut(),
            h_cursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
            hbr_background: (COLOR_WINDOW + 1) as *mut c_void,
            lpsz_menu_name: null(),
            lpsz_class_name: class_name.as_ptr(),
            h_icon_sm: null_mut(),
        };
        // SAFETY: registering class with static `wnd_proc` and NUL-terminated UTF-16 name.
        let _ = unsafe { RegisterClassExW(&wc) };

        // SAFETY: creating visible overlapped window on the main GUI thread.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                title_w.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                i32::from(w),
                i32::from(h),
                null_mut(),
                null_mut(),
                h_inst,
                null_mut(),
            )
        };
        if hwnd.is_null() {
            return Err(Win32Error("CreateWindowExW returned null".to_owned()));
        }

        let mut rc = Rect::default();
        // SAFETY: reading client area of newly created `hwnd`.
        unsafe {
            GetClientRect(hwnd, &mut rc);
        }
        let cw = u16::try_from((rc.right - rc.left).max(1)).unwrap_or(w);
        let ch = u16::try_from((rc.bottom - rc.top).max(1)).unwrap_or(h);

        let dpi = unsafe {
            let hdc = GetDC(hwnd);
            let raw = if hdc.is_null() {
                96
            } else {
                let d = GetDeviceCaps(hdc, LOGPIXELSX);
                ReleaseDC(hwnd, hdc);
                d
            };
            i64::from(raw).clamp(72, 288)
        };

        let queue = Rc::new(RefCell::new(WinQueue {
            events: VecDeque::with_capacity(32),
            left_down: false,
            owns_clipboard: false,
            pending_surrogate: None,
            width: cw,
            height: ch,
        }));
        let prop_raw = Rc::into_raw(Rc::clone(&queue));
        // SAFETY: `SetPropW` binds `prop_raw` until `Drop` calls `RemovePropW` and drops `Rc`.
        unsafe {
            SetPropW(hwnd, PROP_NAME.as_ptr(), prop_raw as *mut c_void);
            ShowWindow(hwnd, SW_SHOWNORMAL);
        }

        Ok(Self {
            hwnd,
            queue,
            dpi,
            prop_raw,
        })
    }
}

impl Drop for Win32Window {
    fn drop(&mut self) {
        self.clear_clipboard();
        // SAFETY: removes prop pointer, destroys window, and releases the extra `Rc` ref.
        unsafe {
            RemovePropW(self.hwnd, PROP_NAME.as_ptr());
            DestroyWindow(self.hwnd);
            drop(Rc::from_raw(self.prop_raw));
        }
    }
}

impl DesktopWindow for Win32Window {
    fn size(&self) -> (u16, u16) {
        let q = self.queue.borrow();
        (q.width, q.height)
    }

    fn dpi(&self) -> i64 {
        self.dpi
    }

    fn present_argb(
        &mut self,
        pixels: &[u32],
        width: u16,
        height: u16,
    ) -> Result<(), Box<dyn Error>> {
        let w = usize::from(width);
        let h = usize::from(height);
        if w == 0 || h == 0 || pixels.len() < w.saturating_mul(h) {
            return Ok(());
        }
        let info = BitmapInfo {
            bmi_header: BitmapInfoHeader {
                bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                bi_width: i32::from(width),
                bi_height: -i32::from(height), // top-down DIB matching PixelSurface row order
                bi_planes: 1,
                bi_bit_count: 32,
                bi_compression: 0, // BI_RGB (0x00RRGGBB)
                bi_size_image: 0,
                bi_xpels_per_meter: 0,
                bi_ypels_per_meter: 0,
                bi_clr_used: 0,
                bi_clr_important: 0,
            },
            bmi_colors: [0; 3],
        };
        // SAFETY: `pixels` holds at least `w * h` 32-bit `0x00RRGGBB` entries matching `info`.
        unsafe {
            let hdc = GetDC(self.hwnd);
            if hdc.is_null() {
                return Err(Box::new(Win32Error("GetDC failed".to_owned())));
            }
            StretchDIBits(
                hdc,
                0,
                0,
                i32::from(width),
                i32::from(height),
                0,
                0,
                i32::from(width),
                i32::from(height),
                pixels.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            ReleaseDC(self.hwnd, hdc);
        }
        Ok(())
    }

    fn poll_events(&mut self, timeout_ms: i32) -> Result<Vec<DesktopEvent>, Box<dyn Error>> {
        let mut msg = Msg::default();
        // SAFETY: pumps all queued Win32 messages; waits up to `timeout_ms` if empty.
        unsafe {
            if self.queue.borrow().events.is_empty()
                && PeekMessageW(&mut msg, null_mut(), 0, 0, 0) == 0
                && timeout_ms > 0
            {
                MsgWaitForMultipleObjectsEx(0, null(), timeout_ms as u32, QS_ALLINPUT, 0);
            }
            while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let mut out = Vec::new();
        let mut q = self.queue.borrow_mut();
        while let Some(ev) = q.events.pop_front() {
            out.push(ev);
        }
        Ok(out)
    }

    fn set_clipboard(&mut self, utf8: &[u8]) -> Result<(), Box<dyn Error>> {
        let s = std::str::from_utf8(utf8).map_err(|e| Box::new(Win32Error(e.to_string())))?;
        let mut utf16 = wide_null(s);
        let bytes = utf16.len().saturating_mul(2);
        // SAFETY: allocates movable HGLOBAL, writes UTF-16LE, transfers ownership to OS or frees
        // `h_mem` via `GlobalFree` if `GlobalLock`/`SetClipboardData` fails.
        unsafe {
            if OpenClipboard(self.hwnd) == 0 {
                utf16.zeroize();
                return Err(Box::new(Win32Error("OpenClipboard failed".to_owned())));
            }
            EmptyClipboard();
            let h_mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if h_mem.is_null() {
                CloseClipboard();
                utf16.zeroize();
                return Err(Box::new(Win32Error("GlobalAlloc failed".to_owned())));
            }
            let ptr = GlobalLock(h_mem).cast::<u16>();
            if ptr.is_null() {
                let _ = GlobalFree(h_mem);
                CloseClipboard();
                utf16.zeroize();
                return Err(Box::new(Win32Error("GlobalLock failed".to_owned())));
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
            GlobalUnlock(h_mem);
            let accepted = SetClipboardData(CF_UNICODETEXT, h_mem);
            if accepted.is_null() {
                let _ = GlobalFree(h_mem);
                self.queue.borrow_mut().owns_clipboard = false;
                CloseClipboard();
                utf16.zeroize();
                return Err(Box::new(Win32Error("SetClipboardData failed".to_owned())));
            }
            self.queue.borrow_mut().owns_clipboard = true;
            CloseClipboard();
        }
        utf16.zeroize();
        Ok(())
    }

    fn request_clipboard(&mut self) -> Result<(), Box<dyn Error>> {
        // SAFETY: reads CF_UNICODETEXT handle from Win32 clipboard into bounded `Zeroizing<Vec<u8>>`.
        unsafe {
            if OpenClipboard(self.hwnd) == 0 {
                return Ok(());
            }
            let h_mem = GetClipboardData(CF_UNICODETEXT);
            if !h_mem.is_null() {
                let ptr = GlobalLock(h_mem).cast::<u16>();
                if !ptr.is_null() {
                    let mut len = 0usize;
                    while len < MAX_CLIPBOARD_BYTES && *ptr.add(len) != 0 {
                        len += 1;
                    }
                    let slice = std::slice::from_raw_parts(ptr, len);
                    let mut text = Zeroizing::new(String::from_utf16_lossy(slice));
                    let mut bytes = Zeroizing::new(text.as_bytes().to_vec());
                    if bytes.len() > MAX_CLIPBOARD_BYTES {
                        let mut cut = MAX_CLIPBOARD_BYTES;
                        while cut > 0 && (bytes[cut] & 0b1100_0000) == 0b1000_0000 {
                            cut -= 1;
                        }
                        bytes.truncate(cut);
                    }
                    text.zeroize();
                    self.queue
                        .borrow_mut()
                        .events
                        .push_back(DesktopEvent::ClipboardPasted(bytes));
                    GlobalUnlock(h_mem);
                }
            }
            CloseClipboard();
        }
        Ok(())
    }

    fn clear_clipboard(&mut self) {
        if !self.queue.borrow().owns_clipboard {
            return;
        }
        self.queue.borrow_mut().owns_clipboard = false;
        // SAFETY: empties system clipboard only while this window holds clipboard ownership.
        unsafe {
            if OpenClipboard(self.hwnd) != 0 {
                EmptyClipboard();
                CloseClipboard();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win32_virtual_key_translation_and_utf16_surrogates() {
        assert_eq!(
            translate_virtual_key(VK_LEFT, Modifiers::none()),
            Some(Key::Left)
        );
        assert_eq!(
            translate_virtual_key(
                b'S' as u16,
                Modifiers {
                    control: true,
                    ..Modifiers::none()
                }
            ),
            Some(Key::Character('s'))
        );
        assert_eq!(
            translate_virtual_key(
                0xBF,
                Modifiers {
                    control: true,
                    ..Modifiers::none()
                }
            ),
            Some(Key::Character('/'))
        );
        let mut pending = None;
        assert_eq!(decode_wm_char(&mut pending, 0xD83E), None);
        assert_eq!(decode_wm_char(&mut pending, 0xDD80), Some('🦀'));
        assert_eq!(pending, None);
    }
}
