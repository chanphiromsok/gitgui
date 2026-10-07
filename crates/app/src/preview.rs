//! Pictures in the file pane: an image a commit added, removed or changed is shown, before and
//! after, with its size in pixels and in bytes.

use std::io::Cursor;
use std::sync::Arc;

use gpui::{Image, ImageFormat, ImageSource, RenderImage};

/// Images bigger than this are not read for a preview.
const MAX_BYTES: usize = 30 << 20;
/// SVGs are drawn this many pixels on their longer side, so they stay sharp when shown large.
const SVG_PIXELS: u32 = 1024;

/// One side of a changed image.
#[derive(Clone)]
pub struct Preview {
    pub source: Picture,
    pub width: u32,
    pub height: u32,
    /// The file's size.
    pub bytes: usize,
}

/// What to draw: an image GPUI decodes itself, or an SVG drawn here (GPUI's own swaps its colors).
#[derive(Clone)]
pub enum Picture {
    Decoded(Arc<Image>),
    Drawn(Arc<RenderImage>),
}

impl Picture {
    pub fn source(&self) -> ImageSource {
        match self {
            Picture::Decoded(image) => image.clone().into(),
            Picture::Drawn(image) => image.clone().into(),
        }
    }
}

/// Both sides of a changed image; a side is `None` where the file was not there (or would not read).
#[derive(Clone, Default)]
pub struct Images {
    pub old: Option<Preview>,
    pub new: Option<Preview>,
}

fn format_of(path: &str) -> Option<ImageFormat> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        "svg" => ImageFormat::Svg,
        _ => return None,
    })
}

/// The file is a picture this can show.
pub fn is_image(path: &str) -> bool {
    format_of(path).is_some()
}

/// A preview of `bytes`, the file `path`; `None` when it does not read as an image.
pub fn preview(path: &str, bytes: Vec<u8>) -> Option<Preview> {
    let size = bytes.len();
    if size > MAX_BYTES {
        return None;
    }
    match format_of(path)? {
        ImageFormat::Svg => {
            let svg = std::str::from_utf8(&bytes).ok()?;
            let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
            let (width, height) = (tree.size().width().round() as u32, tree.size().height().round() as u32);
            let drawn = crate::icons::rasterize(svg, SVG_PIXELS)?;
            Some(Preview { source: Picture::Drawn(drawn), width, height, bytes: size })
        }
        format => {
            let (width, height) =
                image::ImageReader::new(Cursor::new(&bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
            Some(Preview { source: Picture::Decoded(Arc::new(Image::from_bytes(format, bytes))), width, height, bytes: size })
        }
    }
}

/// `245.3 KB`, counting a kilobyte as 1000 bytes, as Finder does.
pub fn human_size(bytes: usize) -> String {
    let b = bytes as f64;
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} KB", b / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", b / 1e6),
        _ => format!("{:.1} GB", b / 1e9),
    }
}

/// How the size changed: `+12.4 KB`, `−3 B`, or `same size`.
pub fn size_change(old: usize, new: usize) -> String {
    match new.cmp(&old) {
        std::cmp::Ordering::Equal => "same size".into(),
        std::cmp::Ordering::Greater => format!("+{}", human_size(new - old)),
        std::cmp::Ordering::Less => format!("−{}", human_size(old - new)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3×2 PNG, written with the image crate.
    fn png() -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn pictures_are_known_by_extension() {
        assert!(is_image("assets/logo.PNG"));
        assert!(is_image("a/b.svg"));
        assert!(!is_image("src/app.ts"));
        assert!(!is_image("Makefile"));
    }

    #[test]
    fn a_png_reads_its_size_in_pixels_and_bytes() {
        let bytes = png();
        let size = bytes.len();
        let preview = preview("a.png", bytes).unwrap();
        assert_eq!((preview.width, preview.height, preview.bytes), (3, 2, size));
        assert!(matches!(preview.source, Picture::Decoded(_)));
    }

    #[test]
    fn an_svg_is_drawn_here_at_its_own_proportions() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#0288d1"/></svg>"##;
        let preview = preview("a.svg", svg.as_bytes().to_vec()).unwrap();
        assert_eq!((preview.width, preview.height), (40, 20));
        let Picture::Drawn(drawn) = &preview.source else { panic!("drawn here") };
        assert_eq!(drawn.size(0).width.0, SVG_PIXELS as i32);
        assert_eq!(drawn.size(0).height.0, SVG_PIXELS as i32 / 2);
    }

    #[test]
    fn something_that_is_not_an_image_gives_no_preview() {
        assert!(preview("a.png", b"not a png".to_vec()).is_none());
        assert!(preview("a.txt", png()).is_none());
    }

    #[test]
    fn sizes_read_like_finder() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(245_312), "245.3 KB");
        assert_eq!(human_size(3_400_000), "3.4 MB");
        assert_eq!(size_change(1000, 13_400), "+12.4 KB");
        assert_eq!(size_change(10, 7), "−3 B");
        assert_eq!(size_change(5, 5), "same size");
    }
}
