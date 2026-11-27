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

use crate::platform::{
    CaptureExclusionState, DesktopEvent, DesktopWindow, SecureInputState, WindowSecurityInfo,
    MAX_CLIPBOARD_BYTES,
};
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

const WM_POWERBROADCAST: u32 = 0x0218;
const PBT_APMSUSPEND: usize = 0x0004;
const WM_IME_STARTCOMPOSITION: u32 = 0x010D;
const WM_IME_ENDCOMPOSITION: u32 = 0x010E;
const WM_IME_COMPOSITION: u32 = 0x010F;
const WM_IME_CHAR: u32 = 0x0286;
const GCS_COMPSTR: u32 = 0x0008;
const GCS_RESULTSTR: u32 = 0x0800;

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
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const WDA_MONITOR: u32 = 0x0000_0001;
const WDA_EXCLUDEFROMCAPTURE: u32 = 0x0000_0011;

/// `DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2` (Windows 10 1703+).
const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: isize = -4;
/// `ERROR_ACCESS_DENIED`: the process DPI awareness was already pinned, which is success for us.
const ERROR_ACCESS_DENIED: u32 = 5;

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
    fn LoadLibraryA(lp_lib_file_name: *const u8) -> *mut c_void;
    fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const u8) -> *const c_void;
    fn GlobalAlloc(u_flags: u32, dw_bytes: usize) -> Hglobal;
    fn GlobalFree(h_mem: Hglobal) -> Hglobal;
    fn GlobalLock(h_mem: Hglobal) -> *mut c_void;
    fn GlobalUnlock(h_mem: Hglobal) -> Bool;
    fn SetLastError(dw_err_code: u32);
    fn GetLastError() -> u32;
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
    fn SetWindowDisplayAffinity(h_wnd: Hwnd, dw_affinity: u32) -> Bool;
    fn LoadLibraryW(lp_lib_file_name: *const u16) -> *mut c_void;
    fn SetWindowPos(
        h_wnd: Hwnd,
        h_wnd_insert_after: Hwnd,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        u_flags: u32,
    ) -> Bool;
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
    ime_result_committed: bool,
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

fn utf16_le_bytes_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

#[derive(Clone, Copy)]
struct Imm32Api {
    get_context: unsafe extern "system" fn(Hwnd) -> *mut c_void,
    get_comp_str_w: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32) -> i32,
    release_context: unsafe extern "system" fn(Hwnd, *mut c_void) -> Bool,
}

fn imm32_api() -> Option<Imm32Api> {
    static API: std::sync::OnceLock<Option<Imm32Api>> = std::sync::OnceLock::new();
    *API.get_or_init(|| {
        // SAFETY: dynamically loads `imm32.dll` and resolves standard Win32 IME symbols.
        unsafe {
            let hmod = LoadLibraryA(c"imm32.dll".as_ptr().cast());
            if hmod.is_null() {
                return None;
            }
            let get_context = GetProcAddress(hmod, c"ImmGetContext".as_ptr().cast());
            let get_comp_str_w = GetProcAddress(hmod, c"ImmGetCompositionStringW".as_ptr().cast());
            let release_context = GetProcAddress(hmod, c"ImmReleaseContext".as_ptr().cast());
            if get_context.is_null() || get_comp_str_w.is_null() || release_context.is_null() {
                return None;
            }
            Some(Imm32Api {
                get_context: std::mem::transmute::<
                    *const c_void,
                    unsafe extern "system" fn(Hwnd) -> *mut c_void,
                >(get_context),
                get_comp_str_w: std::mem::transmute::<
                    *const c_void,
                    unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32) -> i32,
                >(get_comp_str_w),
                release_context: std::mem::transmute::<
                    *const c_void,
                    unsafe extern "system" fn(Hwnd, *mut c_void) -> Bool,
                >(release_context),
            })
        }
    })
}

fn read_imm_string(hwnd: Hwnd, kind: u32) -> Option<String> {
    let api = imm32_api()?;
    // SAFETY: function pointers resolved from `imm32.dll` match Win32 `imm.h` signatures.
    unsafe {
        let himc = (api.get_context)(hwnd);
        if himc.is_null() {
            return None;
        }
        let byte_len = (api.get_comp_str_w)(himc, kind, null_mut(), 0);
        if byte_len <= 0 {
            let _ = (api.release_context)(hwnd, himc);
            return Some(String::new());
        }
        let mut buf = Zeroizing::new(vec![0u8; byte_len as usize]);
        let read = (api.get_comp_str_w)(himc, kind, buf.as_mut_ptr().cast(), byte_len as u32);
        let _ = (api.release_context)(hwnd, himc);
        if read <= 0 {
            return None;
        }
        Some(utf16_le_bytes_to_string(
            &buf[..(read as usize).min(buf.len())],
        ))
    }
}

/// The `shcore.dll` awareness entry point, resolved lazily so it may be absent.
struct DpiApi {
    set_process_dpi_awareness_context: Option<unsafe extern "system" fn(isize) -> Bool>,
}

fn dpi_api() -> &'static DpiApi {
    static API: std::sync::OnceLock<DpiApi> = std::sync::OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: `shcore.dll` is optional; a missing library simply leaves the slot empty.
        unsafe {
            let shcore = LoadLibraryW(wide_null("shcore.dll").as_ptr());
            let set_ctx = if shcore.is_null() {
                null()
            } else {
                GetProcAddress(shcore, c"SetProcessDpiAwarenessContext".as_ptr().cast())
            };
            let set_context = if set_ctx.is_null() {
                None
            } else {
                Some(std::mem::transmute::<
                    *const c_void,
                    unsafe extern "system" fn(isize) -> Bool,
                >(set_ctx))
            };
            DpiApi {
                set_process_dpi_awareness_context: set_context,
            }
        }
    })
}

/// Opts the process into per-monitor DPI awareness *before* the first window exists.
///
/// Without this the process is DPI-unaware, so Windows reports a 96 dpi client area and then
/// bitmap-stretches the finished window to the real display scale. Everything Holonomy draws —
/// glyphs, icon strokes, hairlines — is produced at 96 dpi and resampled by the compositor, which
/// is why the UI looks soft next to native apps on a scaled display. Declaring awareness makes
/// `GetDeviceCaps`/`GetDpiForWindow` report the true scale and the framebuffer reach the screen
/// 1:1. The later, richer APIs win when available; each step is a no-op if unavailable, and
/// `ERROR_ACCESS_DENIED` means an earlier call already pinned awareness, which is also fine.
pub fn enable_per_monitor_dpi_awareness() {
    // SAFETY: the call only takes a scalar; failure is reported through the return value.
    unsafe {
        let api = dpi_api();
        if let Some(set_ctx) = api.set_process_dpi_awareness_context {
            SetLastError(0);
            let ok = set_ctx(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            if ok != 0 || GetLastError() == ERROR_ACCESS_DENIED {
                return;
            }
        }
    }
    // Windows 8.1 fallback: per-monitor aware (v1) via `SetProcessDpiAwareness`.
    // SAFETY: dynamically resolved; absence is tolerated.
    unsafe {
        let shcore = LoadLibraryW(wide_null("shcore.dll").as_ptr());
        if !shcore.is_null() {
            let addr = GetProcAddress(shcore, c"SetProcessDpiAwareness".as_ptr().cast());
            if !addr.is_null() {
                let set_awareness = std::mem::transmute::<
                    *const c_void,
                    unsafe extern "system" fn(i32) -> i32,
                >(addr);
                let _ = set_awareness(2); // PROCESS_PER_MONITOR_DPI_AWARE
            }
        }
    }
    // Last resort for Windows 7/8.0 and for a static-import failure above.
    // SAFETY: `SetProcessDPIAware` exists since Vista.
    unsafe {
        let user32 = LoadLibraryW(wide_null("user32.dll").as_ptr());
        if !user32.is_null() {
            let addr = GetProcAddress(user32, c"SetProcessDPIAware".as_ptr().cast());
            if !addr.is_null() {
                let set_aware = std::mem::transmute::<
                    *const c_void,
                    unsafe extern "system" fn() -> Bool,
                >(addr);
                let _ = set_aware();
            }
        }
    }
}

/// The monitor DPI of an existing window.
///
/// `GetDpiForWindow` reports the scale of the monitor the window is *currently on*, so the window
/// stays sharp when dragged between displays of different scale. It is resolved lazily because it
/// requires per-monitor awareness to be in effect; the `GetDeviceCaps` fallback is what a
/// DPI-unaware or Windows 7 session can report.
fn dpi_for(hwnd: Hwnd) -> i64 {
    // SAFETY: `hwnd` is a live window handle owned by this module; both calls are read-only.
    let raw = unsafe {
        let user32 = LoadLibraryW(wide_null("user32.dll").as_ptr());
        let get_for_window = if user32.is_null() {
            null()
        } else {
            GetProcAddress(user32, c"GetDpiForWindow".as_ptr().cast())
        };
        if !get_for_window.is_null() && !hwnd.is_null() {
            let get = std::mem::transmute::<
                *const c_void,
                unsafe extern "system" fn(Hwnd) -> u32,
            >(get_for_window);
            get(hwnd)
        } else {
            let hdc = GetDC(hwnd);
            if hdc.is_null() {
                96u32
            } else {
                let d = GetDeviceCaps(hdc, LOGPIXELSX);
                ReleaseDC(hwnd, hdc);
                u32::try_from(d).unwrap_or(96)
            }
        }
    };
    i64::from(if raw == 0 { 96 } else { raw }).clamp(72, 288)
}

unsafe extern "system" fn wnd_proc(
    hwnd: Hwnd,
    msg: u32,
    w_param: Wparam,
    l_param: Lparam,
) -> Lresult {
    let mut def_l_param = l_param;
    // SAFETY: `GetPropW` retrieves the `Rc<RefCell<WinQueue>>` installed during `open`.
    let raw_ptr = unsafe { GetPropW(hwnd, PROP_NAME.as_ptr()) } as *const RefCell<WinQueue>;
    if !raw_ptr.is_null() {
        // SAFETY: `raw_ptr` stays alive until `Drop` removes the property and balances `into_raw`.
        let q_ref = unsafe { &*raw_ptr };
        let mut handled_close = false;
        let mut clear_owned_clip = false;
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
                    q.events.push_back(DesktopEvent::Preedit(String::new()));
                    q.events.push_back(DesktopEvent::FocusOut);
                }
                WM_POWERBROADCAST if w_param == PBT_APMSUSPEND => {
                    clear_owned_clip = q.owns_clipboard;
                    q.owns_clipboard = false;
                    q.events.push_back(DesktopEvent::SystemSuspend);
                }
                WM_DESTROYCLIPBOARD => {
                    q.owns_clipboard = false;
                }
                WM_IME_STARTCOMPOSITION => {
                    q.ime_result_committed = false;
                    q.events.push_back(DesktopEvent::Preedit(String::new()));
                }
                WM_IME_COMPOSITION => {
                    let flags = l_param as u32;
                    if flags == 0 {
                        q.ime_result_committed = false;
                        q.events.push_back(DesktopEvent::Preedit(String::new()));
                    }
                    if (flags & GCS_RESULTSTR) != 0 {
                        if let Some(res) = read_imm_string(hwnd, GCS_RESULTSTR) {
                            q.events.push_back(DesktopEvent::Preedit(String::new()));
                            if !res.is_empty() {
                                q.events.push_back(DesktopEvent::CommitText(res));
                            }
                            q.ime_result_committed = true;
                            // Suppress GCS_RESULTSTR in DefWindowProcW so Windows does not re-emit
                            // duplicate WM_CHAR / WM_IME_CHAR messages for the committed string.
                            def_l_param = l_param & !(GCS_RESULTSTR as isize);
                        }
                    }
                    if (flags & GCS_COMPSTR) != 0 {
                        let comp = read_imm_string(hwnd, GCS_COMPSTR).unwrap_or_default();
                        q.events.push_back(DesktopEvent::Preedit(comp));
                    }
                }
                WM_IME_ENDCOMPOSITION => {
                    q.ime_result_committed = false;
                    q.events.push_back(DesktopEvent::Preedit(String::new()));
                }
                WM_IME_CHAR => {
                    if std::mem::take(&mut q.ime_result_committed) {
                        handled_close = true;
                    }
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
                    q.ime_result_committed = false;
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
        if clear_owned_clip {
            // SAFETY: empties system clipboard immediately before OS enters suspend.
            unsafe {
                if OpenClipboard(hwnd) != 0 {
                    EmptyClipboard();
                    CloseClipboard();
                }
            }
        }
        if handled_close {
            return 0;
        }
    }
    // SAFETY: `q` lock is released before calling `DefWindowProcW`, preventing re-entrancy panics.
    unsafe { DefWindowProcW(hwnd, msg, w_param, def_l_param) }
}

/// Native Win32 desktop window presenting a 32-bpp `0x00RRGGBB` framebuffer via `StretchDIBits`.
pub struct Win32Window {
    hwnd: Hwnd,
    queue: Rc<RefCell<WinQueue>>,
    dpi: i64,
    prop_raw: *const RefCell<WinQueue>,
    capture_excluded: bool,
}

impl Win32Window {
    /// Opens and displays a Win32 window sized `width × height` with UTF-16 `title`.
    ///
    /// # Errors
    ///
    /// Returns [`Win32Error`] if window class registration or creation fails.
    pub fn open(title: &str, width: u16, height: u16) -> Result<Self, Win32Error> {
        // Must happen before the first window: awareness cannot be raised once one exists.
        enable_per_monitor_dpi_awareness();
        // The display DPI is only known once a window exists, so seed with the primary display's
        // scale and correct it below from the window that was actually created.
        let hint = crate::platform::physical_size_hint((width, height), dpi_for(null_mut()));
        let w = hint.0.max(320);
        let h = hint.1.max(240);
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
        let dpi = dpi_for(hwnd);
        // `CreateWindowExW` sizes the frame in physical pixels for the monitor it lands on. If that
        // monitor is not the primary one its scale differs from the seed used above, so ask for the
        // same apparent size again now that the real DPI is known.
        let corrected = crate::platform::physical_size_hint((width, height), dpi);
        let (want_w, want_h) = (corrected.0.max(320), corrected.1.max(240));
        if (want_w, want_h) != (w, h) {
            // SAFETY: resizing an owned, already-created window; failure leaves the seed size.
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    null_mut(),
                    0,
                    0,
                    i32::from(want_w),
                    i32::from(want_h),
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                GetClientRect(hwnd, &mut rc);
            }
        }
        let cw = u16::try_from((rc.right - rc.left).max(1)).unwrap_or(want_w);
        let ch = u16::try_from((rc.bottom - rc.top).max(1)).unwrap_or(want_h);

        let queue = Rc::new(RefCell::new(WinQueue {
            events: VecDeque::with_capacity(32),
            left_down: false,
            owns_clipboard: false,
            ime_result_committed: false,
            pending_surrogate: None,
            width: cw,
            height: ch,
        }));
        let prop_raw = Rc::into_raw(Rc::clone(&queue));
        // SAFETY: `SetPropW` binds `prop_raw` until `Drop` calls `RemovePropW` and drops `Rc`.
        // Also exclude the window from desktop capture via `WDA_EXCLUDEFROMCAPTURE` (or `WDA_MONITOR`).
        let capture_excluded = unsafe {
            SetPropW(hwnd, PROP_NAME.as_ptr(), prop_raw as *mut c_void);
            ShowWindow(hwnd, SW_SHOWNORMAL);
            SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) != 0
                || SetWindowDisplayAffinity(hwnd, WDA_MONITOR) != 0
        };

        Ok(Self {
            hwnd,
            queue,
            dpi,
            prop_raw,
            capture_excluded,
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
            let viewport = crate::platform::blit_viewport(self.size(), (width, height));
            if let Some((w_dst, h_dst, x_src, y_src)) = viewport {
                StretchDIBits(
                    hdc,
                    0,
                    0,
                    i32::from(w_dst),
                    i32::from(h_dst),
                    i32::from(x_src),
                    i32::from(y_src),
                    i32::from(w_dst),
                    i32::from(h_dst),
                    pixels.as_ptr().cast(),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            }
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

    fn security_info(&self) -> WindowSecurityInfo {
        WindowSecurityInfo {
            capture_exclusion: if self.capture_excluded {
                CaptureExclusionState::Excluded
            } else {
                CaptureExclusionState::Unsupported(
                    "SetWindowDisplayAffinity rejected by window session",
                )
            },
            secure_input: SecureInputState::Unsupported(
                "Win32 desktop shares keyboard queue across same-integrity processes",
            ),
            clipboard_policy: "CF_UNICODETEXT cleared on WM_KILLFOCUS, WM_DESTROYCLIPBOARD & exit",
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

        // Win32 UTF-16LE IME composition strings (multi-codepoint CJK + surrogate + NUL) decode cleanly.
        let mut ime_utf16: Vec<u8> = "日本語🦀\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(utf16_le_bytes_to_string(&ime_utf16), "日本語🦀");
        ime_utf16.zeroize();

        // Stripping GCS_RESULTSTR from LPARAM preserves upper bits on 64-bit targets.
        let sample_lp: isize = GCS_COMPSTR as isize | GCS_RESULTSTR as isize;
        assert_eq!(sample_lp & !(GCS_RESULTSTR as isize), GCS_COMPSTR as isize);
    }
}
