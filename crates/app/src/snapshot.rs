//! A picture of the program's own window, as macOS composes it (Metal and all), saved as a PNG.
//!
//! This exists so the app can be looked at without anyone at the screen: `script.rs` drives it through
//! some steps and calls `save` after each. An app may always capture its own windows, so no
//! screen-recording permission is involved. Only macOS has it.

use std::path::Path;

use gpui::Window;

#[cfg(target_os = "macos")]
pub fn save(window: &Window, path: &Path) -> Result<(), String> {
    use std::ffi::{c_char, c_void};

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    type Id = *mut c_void;
    type Sel = *const c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Size {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Size,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCreateImage(rect: Rect, list_option: u32, window_id: u32, image_option: u32) -> *mut c_void;
        fn CGImageRelease(image: *mut c_void);
    }
    #[link(name = "objc", kind = "dylib")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
    }
    // `[receiver selector: arg ...]`: `objc_msgSend` is called as a function of the types this message has. Only
    // pointer, integer and `void` results are used here, which all come back the same way.
    macro_rules! send {
        ($receiver:expr, $selector:literal $(, $arg:expr => $ty:ty)* ; $ret:ty) => {{
            let call = std::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(Id, Sel $(, $ty)*) -> $ret>(objc_msgSend);
            call($receiver, sel_registerName(concat!($selector, "\0").as_ptr().cast()) $(, $arg)*)
        }};
    }
    // kCGWindowListOptionIncludingWindow, kCGWindowImageBoundsIgnoreFraming.
    const INCLUDING_WINDOW: u32 = 1 << 3;
    const IGNORE_FRAMING: u32 = 1 << 0;
    // CGRectNull: the window's own bounds.
    let null = Rect { origin: Point { x: f64::INFINITY, y: f64::INFINITY }, size: Size { width: 0., height: 0. } };

    let handle = HasWindowHandle::window_handle(window).map_err(|err| err.to_string())?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return Err("not an AppKit window".into()) };
    // SAFETY: the view pointer is live while `window` is borrowed; these are plain Cocoa calls on it, made on
    // the main thread (this runs inside a window update).
    unsafe {
        let view: Id = appkit.ns_view.as_ptr();
        let ns_window = send!(view, "window"; Id);
        if ns_window.is_null() {
            return Err("the view has no window yet".into());
        }
        let number: i64 = send!(ns_window, "windowNumber"; i64);
        let image = CGWindowListCreateImage(null, INCLUDING_WINDOW, number as u32, IGNORE_FRAMING);
        if image.is_null() {
            return Err("macOS gave no image of the window".into());
        }
        let rep = send!(objc_getClass(c"NSBitmapImageRep".as_ptr()), "alloc"; Id);
        let rep = send!(rep, "initWithCGImage:", image => *mut c_void; Id);
        // NSBitmapImageFileTypePNG is 4.
        let data = send!(rep, "representationUsingType:properties:", 4usize => usize, std::ptr::null_mut() => Id; Id);
        let result = if data.is_null() {
            Err("could not make a PNG".to_owned())
        } else {
            let bytes = send!(data, "bytes"; *const u8);
            let length = send!(data, "length"; usize);
            std::fs::write(path, std::slice::from_raw_parts(bytes, length)).map_err(|err| err.to_string())
        };
        send!(rep, "release"; ());
        CGImageRelease(image);
        result
    }
}

#[cfg(not(target_os = "macos"))]
pub fn save(_window: &Window, _path: &Path) -> Result<(), String> {
    Err("window pictures are only made on macOS".into())
}
