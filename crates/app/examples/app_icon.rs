//! Draws the app icon, `assets/app-icon/icon.svg`, at every size a macOS `.iconset` needs.
//! `cargo run -p gitgui-app --example app_icon -- OUT_DIR`; `iconutil -c icns OUT_DIR` then makes the `.icns`.

use resvg::{tiny_skia, usvg};

fn main() {
    let out = std::env::args().nth(1).expect("usage: app_icon OUT_DIR");
    let svg = include_str!("../assets/app-icon/icon.svg");
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).expect("the icon parses");
    std::fs::create_dir_all(&out).expect("make the output folder");
    for (points, scale) in [16, 32, 128, 256, 512].into_iter().flat_map(|p| [(p, 1), (p, 2)]) {
        let pixels = points * scale;
        let mut pixmap = tiny_skia::Pixmap::new(pixels, pixels).expect("a pixmap");
        let s = pixels as f32 / tree.size().width();
        resvg::render(&tree, tiny_skia::Transform::from_scale(s, s), &mut pixmap.as_mut());
        let name = if scale == 1 { format!("icon_{points}x{points}.png") } else { format!("icon_{points}x{points}@2x.png") };
        pixmap.save_png(format!("{out}/{name}")).expect("write the png");
    }
}
