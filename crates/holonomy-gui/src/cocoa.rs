//! Pure-FFI macOS AppKit / CoreGraphics desktop backend (`libobjc` + `AppKit` + `CoreGraphics` +
//! `Foundation`) with zero external crates under `--locked`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::error::Error;
use std::ffi::{c_char, c_void, CStr};
use std::fmt;
use std::ptr::{null, null_mut};
use std::rc::Rc;

use zeroize::Zeroizing;

use crate::platform::{
    CaptureExclusionState, DesktopEvent, DesktopWindow, SecureInputState, WindowSecurityInfo,
    MAX_CLIPBOARD_BYTES,
};
use crate::{Key, Modifiers};

type Id = *mut c_void;
type Sel = *mut c_void;
type Class = *mut c_void;
type Bool = i8;

#[repr(C)]
#[derive(Clone, Copy)]
struct NsPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NsSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NsRect {
    origin: NsPoint,
    size: NsSize,
}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Class;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    fn objc_allocateClassPair(superclass: Class, name: *const c_char, extra_bytes: usize) -> Class;
    fn objc_registerClassPair(cls: Class);
    fn class_addMethod(cls: Class, name: Sel, imp: *const c_void, types: *const c_char) -> Bool;
    fn class_addIvar(
        cls: Class,
        name: *const c_char,
        size: usize,
        alignment: u8,
        types: *const c_char,
    ) -> Bool;
    fn object_setInstanceVariable(obj: Id, name: *const c_char, value: *mut c_void) -> *mut c_void;
    fn object_getInstanceVariable(
        obj: Id,
        name: *const c_char,
        out_val: *mut *mut c_void,
    ) -> *mut c_void;
}

#[link(name = "Foundation", kind = "framework")]
extern "C" {
    static NSDefaultRunLoopMode: Id;
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {
    static NSPasteboardTypeString: Id;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGDataProviderCreateWithData(
        info: *mut c_void,
        data: *const c_void,
        size: usize,
        release_data: *const c_void,
    ) -> *mut c_void;
    fn CGDataProviderRelease(provider: *mut c_void);
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        bitmap_info: u32,
        provider: *mut c_void,
        decode: *const f64,
        should_interpolate: bool,
        intent: i32,
    ) -> *mut c_void;
    fn CGImageRelease(image: *mut c_void);
    fn CGContextDrawImage(ctx: *mut c_void, rect: NsRect, image: *mut c_void);
}

#[inline]
fn sel(name: &[u8]) -> Sel {
    // SAFETY: callers pass static NUL-terminated C selector names.
    unsafe { sel_registerName(name.as_ptr().cast()) }
}

#[inline]
fn cls(name: &[u8]) -> Class {
    // SAFETY: callers pass static NUL-terminated Objective-C class names.
    unsafe { objc_getClass(name.as_ptr().cast()) }
}

macro_rules! msg {
    ($obj:expr, $sel:expr => $ret:ty) => {{
        // SAFETY: `objc_msgSend` is the Objective-C ABI dispatch entrypoint; caller specifies the
        // exact receiver, selector, and signature matching the Objective-C method.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> $ret = std::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(Id, Sel) -> $ret,
            >(objc_msgSend);
            f($obj, $sel)
        }
    }};
    ($obj:expr, $sel:expr, $($arg:expr => $ty:ty),+ => $ret:ty) => {{
        // SAFETY: `objc_msgSend` is the Objective-C ABI dispatch entrypoint; caller specifies the
        // exact receiver, selector, arguments, and return signature.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, $($ty),+) -> $ret = std::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(Id, Sel, $($ty),+) -> $ret,
            >(objc_msgSend);
            f($obj, $sel, $($arg),+)
        }
    }};
}

/// Errors returned by the native macOS Cocoa desktop backend.
#[derive(Debug)]
pub struct CocoaError(pub String);

impl fmt::Display for CocoaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cocoa GUI error: {}", self.0)
    }
}

impl Error for CocoaError {}

struct CocoaQueue {
    events: VecDeque<DesktopEvent>,
    pixels: Vec<u32>,
    fb_w: u16,
    fb_h: u16,
    width: u16,
    height: u16,
}

fn get_queue(obj: Id) -> Option<&'static RefCell<CocoaQueue>> {
    let mut ptr: *mut c_void = null_mut();
    // SAFETY: reads `_holoQueue` ivar set in `CocoaWindow::open`.
    unsafe {
        object_getInstanceVariable(obj, c"_holoQueue".as_ptr(), &mut ptr);
    }
    if ptr.is_null() {
        None
    } else {
        // SAFETY: pointer remains valid until `CocoaWindow::drop` clears ivar and drops `Rc`.
        Some(unsafe { &*(ptr as *const RefCell<CocoaQueue>) })
    }
}

extern "C" fn view_accepts_first_responder(_self: Id, _cmd: Sel) -> Bool {
    1
}

extern "C" fn view_is_flipped(_self: Id, _cmd: Sel) -> Bool {
    0
}

extern "C" fn view_draw_rect(this: Id, _cmd: Sel, _rect: NsRect) {
    let Some(q_ref) = get_queue(this) else {
        return;
    };
    let q = q_ref.borrow();
    if q.fb_w == 0 || q.fb_h == 0 || q.pixels.len() < usize::from(q.fb_w) * usize::from(q.fb_h) {
        return;
    }
    let bounds: NsRect = msg!(this, sel(b"bounds\0") => NsRect);
    let ns_ctx: Id = msg!(cls(b"NSGraphicsContext\0"), sel(b"currentContext\0") => Id);
    if ns_ctx.is_null() {
        return;
    }
    let cg_ctx: *mut c_void = msg!(ns_ctx, sel(b"CGContext\0") => *mut c_void);
    if cg_ctx.is_null() {
        return;
    }
    let w = usize::from(q.fb_w);
    let h = usize::from(q.fb_h);
    let byte_len = w * h * 4;
    // kCGImageAlphaNoneSkipFirst (6) | kCGBitmapByteOrder32Little (2 << 12) => 0x00RRGGBB on LE.
    const BITMAP_INFO_XRGB: u32 = 6 | (2 << 12);
    // SAFETY: `q.pixels` stays borrowed immutably throughout `CGContextDrawImage`.
    unsafe {
        let space = CGColorSpaceCreateDeviceRGB();
        let prov =
            CGDataProviderCreateWithData(null_mut(), q.pixels.as_ptr().cast(), byte_len, null());
        if !prov.is_null() {
            let img = CGImageCreate(
                w,
                h,
                8,
                32,
                w * 4,
                space,
                BITMAP_INFO_XRGB,
                prov,
                null(),
                false,
                0,
            );
            if !img.is_null() {
                CGContextDrawImage(cg_ctx, bounds, img);
                CGImageRelease(img);
            }
            CGDataProviderRelease(prov);
        }
        CGColorSpaceRelease(space);
    }
}

fn translate_cocoa_event(ev: Id) -> Option<(Key, Modifiers)> {
    let flags: usize = msg!(ev, sel(b"modifierFlags\0") => usize);
    let shift = (flags & (1 << 17)) != 0;
    let control = (flags & ((1 << 18) | (1 << 20))) != 0; // Ctrl or Cmd
    let alt = (flags & (1 << 19)) != 0;
    let modifiers = Modifiers {
        shift,
        control,
        alt,
    };
    let keycode: u16 = msg!(ev, sel(b"keyCode\0") => u16);
    if let Some(k) = translate_cocoa_keycode(keycode) {
        return Some((k, modifiers));
    }
    let chars_ns: Id = if control {
        msg!(ev, sel(b"charactersIgnoringModifiers\0") => Id)
    } else {
        msg!(ev, sel(b"characters\0") => Id)
    };
    if chars_ns.is_null() {
        return None;
    }
    let utf8: *const c_char = msg!(chars_ns, sel(b"UTF8String\0") => *const c_char);
    if utf8.is_null() {
        return None;
    }
    // SAFETY: `UTF8String` returns NUL-terminated UTF-8 owned by the `NSString`.
    let s = unsafe { CStr::from_ptr(utf8) }.to_str().ok()?;
    let ch = s.chars().next()?;
    if ch.is_control() && !control {
        return None;
    }
    let (normalized, out_mods) = if control {
        let lower = if ch.is_ascii_uppercase() {
            ch.to_ascii_lowercase()
        } else {
            ch
        };
        (lower, modifiers)
    } else {
        // When `characters` emitted a printable scalar under Shift/Option layout rules, keep `alt`
        // cleared so non-shortcut characters insert cleanly into `App::press`.
        (
            ch,
            Modifiers {
                shift,
                control: false,
                alt: false,
            },
        )
    };
    Some((Key::Character(normalized), out_mods))
}

struct AutoreleasePool(Id);

impl AutoreleasePool {
    fn new() -> Self {
        let alloc: Id = msg!(cls(b"NSAutoreleasePool\0"), sel(b"alloc\0") => Id);
        let pool: Id = msg!(alloc, sel(b"init\0") => Id);
        Self(pool)
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        if !self.0.is_null() {
            let _: () = msg!(self.0, sel(b"drain\0") => ());
        }
    }
}

fn truncate_utf8_clipboard(slice: &[u8], max_bytes: usize) -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(slice[..slice.len().min(max_bytes)].to_vec());
    if slice.len() > max_bytes {
        while !bytes.is_empty() && std::str::from_utf8(&bytes).is_err() {
            bytes.pop();
        }
    }
    bytes
}

fn translate_cocoa_keycode(keycode: u16) -> Option<Key> {
    match keycode {
        36 | 76 => Some(Key::Enter),
        48 => Some(Key::Tab),
        51 => Some(Key::Backspace),
        53 => Some(Key::Escape),
        115 => Some(Key::Home),
        116 => Some(Key::PageUp),
        117 => Some(Key::Delete),
        119 => Some(Key::End),
        121 => Some(Key::PageDown),
        123 => Some(Key::Left),
        124 => Some(Key::Right),
        125 => Some(Key::Down),
        126 => Some(Key::Up),
        _ => None,
    }
}

extern "C" fn view_key_down(this: Id, _cmd: Sel, ev: Id) {
    if let Some(q_ref) = get_queue(this) {
        if let Some((key, modifiers)) = translate_cocoa_event(ev) {
            q_ref
                .borrow_mut()
                .events
                .push_back(DesktopEvent::Key { key, modifiers });
        }
    }
}

fn view_local_xy(view: Id, ev: Id) -> (i16, i16) {
    let win_pt: NsPoint = msg!(ev, sel(b"locationInWindow\0") => NsPoint);
    let local: NsPoint = msg!(
        view,
        sel(b"convertPoint:fromView:\0"),
        win_pt => NsPoint,
        null_mut() => Id
        => NsPoint
    );
    let bounds: NsRect = msg!(view, sel(b"bounds\0") => NsRect);
    let top_y = (bounds.size.height - local.y).round() as i16;
    (local.x.round() as i16, top_y)
}

extern "C" fn view_mouse_down(this: Id, _cmd: Sel, ev: Id) {
    if let Some(q_ref) = get_queue(this) {
        let (x, y) = view_local_xy(this, ev);
        let flags: usize = msg!(ev, sel(b"modifierFlags\0") => usize);
        let modifiers = Modifiers {
            shift: (flags & (1 << 17)) != 0,
            control: (flags & ((1 << 18) | (1 << 20))) != 0,
            alt: (flags & (1 << 19)) != 0,
        };
        q_ref
            .borrow_mut()
            .events
            .push_back(DesktopEvent::MouseDown { x, y, modifiers });
    }
}

extern "C" fn view_mouse_dragged(this: Id, _cmd: Sel, ev: Id) {
    if let Some(q_ref) = get_queue(this) {
        let (x, y) = view_local_xy(this, ev);
        q_ref
            .borrow_mut()
            .events
            .push_back(DesktopEvent::MouseDrag { x, y });
    }
}

extern "C" fn view_mouse_up(this: Id, _cmd: Sel, _ev: Id) {
    if let Some(q_ref) = get_queue(this) {
        q_ref.borrow_mut().events.push_back(DesktopEvent::MouseUp);
    }
}

extern "C" fn view_scroll_wheel(this: Id, _cmd: Sel, ev: Id) {
    if let Some(q_ref) = get_queue(this) {
        let dy: f64 = msg!(ev, sel(b"scrollingDeltaY\0") => f64);
        let delta_lines = if dy > 0.0 {
            -3
        } else if dy < 0.0 {
            3
        } else {
            return;
        };
        q_ref
            .borrow_mut()
            .events
            .push_back(DesktopEvent::Scroll { delta_lines });
    }
}

extern "C" fn delegate_window_should_close(this: Id, _cmd: Sel, _sender: Id) -> Bool {
    if let Some(q_ref) = get_queue(this) {
        q_ref
            .borrow_mut()
            .events
            .push_back(DesktopEvent::CloseRequested);
    }
    0 // let the Rust loop finalize & close cleanly
}

extern "C" fn delegate_window_did_resign_key(this: Id, _cmd: Sel, _notif: Id) {
    if let Some(q_ref) = get_queue(this) {
        q_ref.borrow_mut().events.push_back(DesktopEvent::FocusOut);
    }
}

fn ensure_objc_classes() -> (Class, Class) {
    let view_name = c"HolonomyCocoaView";
    let del_name = c"HolonomyCocoaDelegate";
    // SAFETY: registers custom subclasses once on the main Cocoa thread.
    unsafe {
        let mut view_cls = objc_getClass(view_name.as_ptr());
        if view_cls.is_null() {
            view_cls = objc_allocateClassPair(cls(b"NSView\0"), view_name.as_ptr(), 0);
            class_addIvar(
                view_cls,
                c"_holoQueue".as_ptr(),
                std::mem::size_of::<*mut c_void>(),
                3,
                c"^v".as_ptr(),
            );
            class_addMethod(
                view_cls,
                sel(b"acceptsFirstResponder\0"),
                view_accepts_first_responder as *const c_void,
                c"c@:".as_ptr(),
            );
            class_addMethod(
                view_cls,
                sel(b"isFlipped\0"),
                view_is_flipped as *const c_void,
                c"c@:".as_ptr(),
            );
            class_addMethod(
                view_cls,
                sel(b"drawRect:\0"),
                view_draw_rect as *const c_void,
                c"v@:{CGRect={CGPoint=dd}{CGSize=dd}}".as_ptr(),
            );
            class_addMethod(
                view_cls,
                sel(b"keyDown:\0"),
                view_key_down as *const c_void,
                c"v@:@".as_ptr(),
            );
            class_addMethod(
                view_cls,
                sel(b"mouseDown:\0"),
                view_mouse_down as *const c_void,
                c"v@:@".as_ptr(),
            );
            class_addMethod(
                view_cls,
                sel(b"mouseDragged:\0"),
                view_mouse_dragged as *const c_void,
                c"v@:@".as_ptr(),
            );
            class_addMethod(
                view_cls,
                sel(b"mouseUp:\0"),
                view_mouse_up as *const c_void,
                c"v@:@".as_ptr(),
            );
            class_addMethod(
                view_cls,
                sel(b"scrollWheel:\0"),
                view_scroll_wheel as *const c_void,
                c"v@:@".as_ptr(),
            );
            objc_registerClassPair(view_cls);
        }

        let mut del_cls = objc_getClass(del_name.as_ptr());
        if del_cls.is_null() {
            del_cls = objc_allocateClassPair(cls(b"NSObject\0"), del_name.as_ptr(), 0);
            class_addIvar(
                del_cls,
                c"_holoQueue".as_ptr(),
                std::mem::size_of::<*mut c_void>(),
                3,
                c"^v".as_ptr(),
            );
            class_addMethod(
                del_cls,
                sel(b"windowShouldClose:\0"),
                delegate_window_should_close as *const c_void,
                c"c@:@".as_ptr(),
            );
            class_addMethod(
                del_cls,
                sel(b"windowDidResignKey:\0"),
                delegate_window_did_resign_key as *const c_void,
                c"v@:@".as_ptr(),
            );
            objc_registerClassPair(del_cls);
        }
        (view_cls, del_cls)
    }
}

fn make_nsstring(text: &str) -> Id {
    let bytes = text.as_bytes();
    let alloc: Id = msg!(cls(b"NSString\0"), sel(b"alloc\0") => Id);
    // 4 == NSUTF8StringEncoding
    msg!(
        alloc,
        sel(b"initWithBytes:length:encoding:\0"),
        bytes.as_ptr().cast() => *const c_void,
        bytes.len() => usize,
        4usize => usize
        => Id
    )
}

/// Native macOS `NSWindow` + `CGContext` pixel surface backend (`--locked`, zero external crates).
pub struct CocoaWindow {
    app: Id,
    window: Id,
    view: Id,
    delegate: Id,
    queue: Rc<RefCell<CocoaQueue>>,
    dpi: i64,
    prop_raw: *const RefCell<CocoaQueue>,
    clipboard_change_count: Option<isize>,
    capture_excluded: bool,
    secure_input: secure_input::Guard,
}

mod secure_input {
    use std::ffi::c_void;

    pub(super) struct Guard {
        hitoolbox: *mut c_void,
        enabled: bool,
    }

    impl Guard {
        pub(super) fn acquire() -> Self {
            // SAFETY: dynamically resolves HIToolbox secure-event-input APIs without hard-linking Carbon.
            unsafe {
                let hitoolbox = libc::dlopen(
                    c"/System/Library/Frameworks/Carbon.framework/Frameworks/HIToolbox.framework/HIToolbox".as_ptr(),
                    libc::RTLD_LAZY,
                );
                if hitoolbox.is_null() {
                    return Self {
                        hitoolbox: std::ptr::null_mut(),
                        enabled: false,
                    };
                }
                let sym = libc::dlsym(hitoolbox, c"EnableSecureEventInput".as_ptr());
                let enabled = if sym.is_null() {
                    false
                } else {
                    let f = std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> i32>(sym);
                    f() == 0
                };
                Self { hitoolbox, enabled }
            }
        }

        pub(super) fn is_active(&self) -> bool {
            if self.hitoolbox.is_null() || !self.enabled {
                return false;
            }
            // SAFETY: queries `IsSecureEventInputEnabled` from the open HIToolbox handle.
            unsafe {
                let sym = libc::dlsym(self.hitoolbox, c"IsSecureEventInputEnabled".as_ptr());
                if sym.is_null() {
                    return self.enabled;
                }
                let f = std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> u8>(sym);
                f() != 0
            }
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            if self.hitoolbox.is_null() {
                return;
            }
            // SAFETY: balances `EnableSecureEventInput` and releases the HIToolbox handle.
            unsafe {
                if self.enabled {
                    let sym = libc::dlsym(self.hitoolbox, c"DisableSecureEventInput".as_ptr());
                    if !sym.is_null() {
                        let f =
                            std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> i32>(sym);
                        let _ = f();
                    }
                }
                libc::dlclose(self.hitoolbox);
            }
        }
    }
}

impl CocoaWindow {
    /// Opens an AppKit window sized `width × height` points with UTF-8 `title`.
    ///
    /// # Errors
    ///
    /// Returns [`CocoaError`] if AppKit initialization fails.
    pub fn open(title: &str, width: u16, height: u16) -> Result<Self, CocoaError> {
        let _pool = AutoreleasePool::new();
        let w = width.max(320);
        let h = height.max(240);
        let (view_cls, del_cls) = ensure_objc_classes();

        let app: Id = msg!(cls(b"NSApplication\0"), sel(b"sharedApplication\0") => Id);
        if app.is_null() {
            return Err(CocoaError(
                "NSApplication sharedApplication returned null".to_owned(),
            ));
        }
        let _: Bool = msg!(app, sel(b"setActivationPolicy:\0"), 0isize => isize => Bool);

        let rect = NsRect {
            origin: NsPoint { x: 100.0, y: 100.0 },
            size: NsSize {
                width: f64::from(w),
                height: f64::from(h),
            },
        };
        let win_alloc: Id = msg!(cls(b"NSWindow\0"), sel(b"alloc\0") => Id);
        // styleMask: Titled(1) | Closable(2) | Miniaturizable(4) | Resizable(8) = 15; backing=2 (Buffered)
        let window: Id = msg!(
            win_alloc,
            sel(b"initWithContentRect:styleMask:backing:defer:\0"),
            rect => NsRect,
            15usize => usize,
            2usize => usize,
            0i8 => Bool
            => Id
        );
        if window.is_null() {
            return Err(CocoaError("NSWindow initialization failed".to_owned()));
        }

        // Exclude window contents from macOS window-sharing / ScreenCaptureKit captures.
        let _: () = msg!(window, sel(b"setSharingType:\0"), 0usize => usize => ());
        let sharing: usize = msg!(window, sel(b"sharingType\0") => usize);
        let capture_excluded = sharing == 0;
        let secure_input = secure_input::Guard::acquire();

        let ns_title = make_nsstring(title);
        let _: () = msg!(window, sel(b"setTitle:\0"), ns_title => Id => ());
        let _: () = msg!(ns_title, sel(b"release\0") => ());

        let queue = Rc::new(RefCell::new(CocoaQueue {
            events: VecDeque::with_capacity(32),
            pixels: Vec::new(),
            fb_w: w,
            fb_h: h,
            width: w,
            height: h,
        }));
        let prop_raw = Rc::into_raw(Rc::clone(&queue));

        let view_alloc: Id = msg!(view_cls, sel(b"alloc\0") => Id);
        let view: Id = msg!(view_alloc, sel(b"initWithFrame:\0"), rect => NsRect => Id);
        let del_alloc: Id = msg!(del_cls, sel(b"alloc\0") => Id);
        let delegate: Id = msg!(del_alloc, sel(b"init\0") => Id);
        // SAFETY: associates `prop_raw` with view & delegate until `Drop`.
        unsafe {
            object_setInstanceVariable(view, c"_holoQueue".as_ptr(), prop_raw as *mut c_void);
            object_setInstanceVariable(delegate, c"_holoQueue".as_ptr(), prop_raw as *mut c_void);
        }

        let _: () = msg!(window, sel(b"setContentView:\0"), view => Id => ());
        let _: () = msg!(window, sel(b"setDelegate:\0"), delegate => Id => ());
        let _: Bool = msg!(window, sel(b"makeFirstResponder:\0"), view => Id => Bool);
        let _: () = msg!(window, sel(b"makeKeyAndOrderFront:\0"), null_mut() => Id => ());
        let _: () = msg!(app, sel(b"activateIgnoringOtherApps:\0"), 1i8 => Bool => ());
        let _: () = msg!(app, sel(b"finishLaunching\0") => ());

        let scale: f64 = msg!(window, sel(b"backingScaleFactor\0") => f64);
        let dpi = dpi_from_backing_scale(scale);

        Ok(Self {
            app,
            window,
            view,
            delegate,
            queue,
            dpi,
            prop_raw,
            clipboard_change_count: None,
            capture_excluded,
            secure_input,
        })
    }
}

fn dpi_from_backing_scale(scale: f64) -> i64 {
    ((96.0 * scale.max(1.0)).round() as i64).clamp(72, 288)
}

impl Drop for CocoaWindow {
    fn drop(&mut self) {
        self.clear_clipboard();
        // SAFETY: clears queue ivar pointers so callbacks stop touching queue during teardown.
        unsafe {
            object_setInstanceVariable(self.view, c"_holoQueue".as_ptr(), null_mut());
            object_setInstanceVariable(self.delegate, c"_holoQueue".as_ptr(), null_mut());
        }
        let _: () = msg!(self.window, sel(b"close\0") => ());
        let _: () = msg!(self.view, sel(b"release\0") => ());
        let _: () = msg!(self.delegate, sel(b"release\0") => ());
        // SAFETY: balances `Rc::into_raw` allocated in `CocoaWindow::open`.
        unsafe {
            drop(Rc::from_raw(self.prop_raw));
        }
    }
}

impl DesktopWindow for CocoaWindow {
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
        let _pool = AutoreleasePool::new();
        {
            let mut q = self.queue.borrow_mut();
            q.fb_w = width;
            q.fb_h = height;
            q.pixels.clear();
            q.pixels.extend_from_slice(pixels);
        }
        let _: () = msg!(self.view, sel(b"setNeedsDisplay:\0"), 1i8 => Bool => ());
        let _: () = msg!(self.view, sel(b"displayIfNeeded\0") => ());
        Ok(())
    }

    fn poll_events(&mut self, timeout_ms: i32) -> Result<Vec<DesktopEvent>, Box<dyn Error>> {
        let _pool = AutoreleasePool::new();
        let secs = f64::from(timeout_ms.max(0)) / 1000.0;
        let date: Id = if timeout_ms > 0 {
            msg!(cls(b"NSDate\0"), sel(b"dateWithTimeIntervalSinceNow:\0"), secs => f64 => Id)
        } else {
            msg!(cls(b"NSDate\0"), sel(b"distantPast\0") => Id)
        };
        // SAFETY: `NSDefaultRunLoopMode` is an AppKit framework constant.
        let mode = unsafe { NSDefaultRunLoopMode };
        let mut first = true;
        loop {
            let until = if first {
                first = false;
                date
            } else {
                msg!(cls(b"NSDate\0"), sel(b"distantPast\0") => Id)
            };
            let ev: Id = msg!(
                self.app,
                sel(b"nextEventMatchingMask:untilDate:inMode:dequeue:\0"),
                usize::MAX => usize,
                until => Id,
                mode => Id,
                1i8 => Bool
                => Id
            );
            if ev.is_null() {
                break;
            }
            let _: () = msg!(self.app, sel(b"sendEvent:\0"), ev => Id => ());
        }

        let bounds: NsRect = msg!(self.view, sel(b"bounds\0") => NsRect);
        let cur_w = (bounds.size.width.round() as i64).clamp(1, i64::from(u16::MAX)) as u16;
        let cur_h = (bounds.size.height.round() as i64).clamp(1, i64::from(u16::MAX)) as u16;
        let mut q = self.queue.borrow_mut();
        if (cur_w, cur_h) != (q.width, q.height) {
            q.width = cur_w;
            q.height = cur_h;
            q.events.push_back(DesktopEvent::Resize {
                width: cur_w,
                height: cur_h,
            });
        }
        let mut out = Vec::new();
        while let Some(ev) = q.events.pop_front() {
            out.push(ev);
        }
        Ok(out)
    }

    fn set_clipboard(&mut self, utf8: &[u8]) -> Result<(), Box<dyn Error>> {
        let _pool = AutoreleasePool::new();
        let s = std::str::from_utf8(utf8).map_err(|e| Box::new(CocoaError(e.to_string())))?;
        let pb: Id = msg!(cls(b"NSPasteboard\0"), sel(b"generalPasteboard\0") => Id);
        if pb.is_null() {
            return Ok(());
        }
        let _: isize = msg!(pb, sel(b"clearContents\0") => isize);
        let ns_str = make_nsstring(s);
        // SAFETY: `NSPasteboardTypeString` is an AppKit framework global string constant.
        let ptype = unsafe { NSPasteboardTypeString };
        let ok: Bool = msg!(pb, sel(b"setString:forType:\0"), ns_str => Id, ptype => Id => Bool);
        let _: () = msg!(ns_str, sel(b"release\0") => ());
        if ok != 0 {
            let cnt: isize = msg!(pb, sel(b"changeCount\0") => isize);
            self.clipboard_change_count = Some(cnt);
        } else {
            self.clipboard_change_count = None;
        }
        Ok(())
    }

    fn request_clipboard(&mut self) -> Result<(), Box<dyn Error>> {
        let _pool = AutoreleasePool::new();
        let pb: Id = msg!(cls(b"NSPasteboard\0"), sel(b"generalPasteboard\0") => Id);
        if pb.is_null() {
            return Ok(());
        }
        // SAFETY: `NSPasteboardTypeString` is an AppKit framework global string constant.
        let ptype = unsafe { NSPasteboardTypeString };
        let ns_str: Id = msg!(pb, sel(b"stringForType:\0"), ptype => Id => Id);
        if ns_str.is_null() {
            return Ok(());
        }
        let utf8: *const c_char = msg!(ns_str, sel(b"UTF8String\0") => *const c_char);
        if utf8.is_null() {
            return Ok(());
        }
        // SAFETY: `UTF8String` returns NUL-terminated UTF-8 owned by the `NSString`.
        let slice = unsafe { CStr::from_ptr(utf8) }.to_bytes();
        let bytes = truncate_utf8_clipboard(slice, MAX_CLIPBOARD_BYTES);
        self.queue
            .borrow_mut()
            .events
            .push_back(DesktopEvent::ClipboardPasted(bytes));
        Ok(())
    }

    fn clear_clipboard(&mut self) {
        let Some(owned_count) = self.clipboard_change_count.take() else {
            return;
        };
        let _pool = AutoreleasePool::new();
        let pb: Id = msg!(cls(b"NSPasteboard\0"), sel(b"generalPasteboard\0") => Id);
        if !pb.is_null() {
            let cur_count: isize = msg!(pb, sel(b"changeCount\0") => isize);
            if cur_count == owned_count {
                let _: isize = msg!(pb, sel(b"clearContents\0") => isize);
            }
        }
    }

    fn security_info(&self) -> WindowSecurityInfo {
        WindowSecurityInfo {
            capture_exclusion: if self.capture_excluded {
                CaptureExclusionState::Excluded
            } else {
                CaptureExclusionState::Unsupported("NSWindowSharingNone did not stick")
            },
            secure_input: if self.secure_input.is_active() {
                SecureInputState::Active
            } else {
                SecureInputState::Unsupported("EnableSecureEventInput unavailable or inactive")
            },
            clipboard_policy: "NSPasteboard cleared on windowDidResignKey & exit",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cocoa_keycode_translation_dpi_clamping_and_utf8_boundary() {
        assert_eq!(translate_cocoa_keycode(123), Some(Key::Left));
        assert_eq!(translate_cocoa_keycode(51), Some(Key::Backspace));
        assert_eq!(translate_cocoa_keycode(36), Some(Key::Enter));
        assert_eq!(dpi_from_backing_scale(1.0), 96);
        assert_eq!(dpi_from_backing_scale(2.0), 192);
        assert_eq!(dpi_from_backing_scale(4.0), 288);

        // Complete 4-byte emoji ending right at boundary must stay intact.
        let exact = format!("{}🦀extra", "a".repeat(MAX_CLIPBOARD_BYTES - 4));
        let kept = truncate_utf8_clipboard(exact.as_bytes(), MAX_CLIPBOARD_BYTES);
        assert_eq!(kept.len(), MAX_CLIPBOARD_BYTES);
        assert!(std::str::from_utf8(&kept).unwrap().ends_with('🦀'));

        // Split 4-byte emoji across boundary must be trimmed cleanly without dropping prior bytes.
        let split = format!("{}🦀", "a".repeat(MAX_CLIPBOARD_BYTES - 2));
        let trimmed = truncate_utf8_clipboard(split.as_bytes(), MAX_CLIPBOARD_BYTES);
        assert_eq!(trimmed.len(), MAX_CLIPBOARD_BYTES - 2);
        assert!(std::str::from_utf8(&trimmed).is_ok());
    }
}
