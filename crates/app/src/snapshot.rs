//! A picture of the program's own window, as macOS composes it (Metal and all), saved as a PNG.
//!
//! This exists so the app can be looked at without anyone at the screen: `script.rs` drives it through
//! some steps and calls `save` after each. An app may always capture its own windows, so no
//! screen-recording permission is involved. Only macOS has it.

use std::path::Path;

use gpui::Window;

// The old `objc` macros mention a `cargo-clippy` cfg this crate does not declare.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
pub fn save(window: &Window, path: &Path) -> Result<(), String> {
    use std::ffi::c_void;

    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

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
        let view = appkit.ns_view.as_ptr() as *mut Object;
        let ns_window: *mut Object = msg_send![view, window];
        if ns_window.is_null() {
            return Err("the view has no window yet".into());
        }
        let number: i64 = msg_send![ns_window, windowNumber];
        let image = CGWindowListCreateImage(null, INCLUDING_WINDOW, number as u32, IGNORE_FRAMING);
        if image.is_null() {
            return Err("macOS gave no image of the window".into());
        }
        let rep: *mut Object = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: *mut Object = msg_send![rep, initWithCGImage: image];
        // NSBitmapImageFileTypePNG is 4.
        let data: *mut Object = msg_send![rep, representationUsingType: 4usize properties: std::ptr::null_mut::<Object>()];
        let result = if data.is_null() {
            Err("could not make a PNG".to_owned())
        } else {
            let bytes: *const u8 = msg_send![data, bytes];
            let length: usize = msg_send![data, length];
            std::fs::write(path, std::slice::from_raw_parts(bytes, length)).map_err(|err| err.to_string())
        };
        let _: () = msg_send![rep, release];
        CGImageRelease(image);
        result
    }
}

#[cfg(not(target_os = "macos"))]
pub fn save(_window: &Window, _path: &Path) -> Result<(), String> {
    Err("window pictures are only made on macOS".into())
}
