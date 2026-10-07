//! How the graph looks: its colors, how a line bends, how a commit is marked.
//!
//! A style is a small, fixed set of choices, so every one of them can be checked: each palette is fitted to the
//! color theme's background (a line is never less than 3:1 against it, the contrast WCAG asks of graphics), and
//! the tests check that neighboring lines stay tellable apart in every built-in theme. The look of the rest of
//! the app (avatars, project tiles) stays with the color theme.

use std::sync::RwLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use gitgui_core::Half;
use gpui::{Bounds, Path, PathBuilder, Pixels, Point, Window, point, px, quad, rgb, size};

use crate::theme::{self, t};

/// How a line changes lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// A smooth bend into the lane.
    Curve,
    /// A straight diagonal.
    Straight,
    /// Straight up and down with a rounded right-angle corner, like a circuit board.
    Elbow,
}

/// How a commit that has no author picture is marked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    Dot,
    /// A hollow circle: the background shows through.
    Ring,
    Square,
}

/// How a branch, tag or stash label (the "release/1.0.0" beside a commit) is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// A tint of the line's color, outlined in it; rounded corners.
    Tinted,
    /// Filled with the line's color.
    Solid,
    /// No fill, an outline in the line's color.
    Outline,
    /// A tint, fully rounded, no outline.
    Pill,
    /// Filled, fully rounded.
    Capsule,
    /// No box: a dot in the line's color, then the name.
    Dot,
    /// Filled, square corners, bold.
    Block,
}

/// What a label is, for choosing its color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Branch,
    Remote,
    Tag,
    Stash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Radius {
    Square,
    Small,
    Medium,
    Full,
}

/// The ink, fill, outline and corners of one label.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub fill: Option<u32>,
    pub ink: u32,
    /// Width and color.
    pub border: Option<(f32, u32)>,
    pub radius: Radius,
    /// A dot before the name, in this color.
    pub dot: Option<u32>,
    pub bold: bool,
}

/// The ink that reads best on `fill`: whichever of near-black and white has the greater contrast with it. (Not the
/// theme's `text_on`, which prefers white on a saturated blue even where black scores higher: labels are small text.)
fn ink_on(fill: u32) -> u32 {
    if contrast(0x111111, fill) > contrast(0xffffff, fill) { 0x111111 } else { 0xffffff }
}

const GOLD: u32 = 0xd4a72c;
const GREEN: u32 = 0x23a455;

/// How a label looks in a style. `color` is the line's color for a branch; `head` marks the branch that is checked out,
/// which is always the loudest thing in its row.
pub fn badge_look(theme: &theme::Theme, badge: Badge, role: Role, head: bool, color: u32) -> Look {
    let base = match role {
        Role::Branch => color,
        Role::Remote => theme.muted,
        Role::Tag => GOLD,
        Role::Stash => GREEN,
    };
    let tint = theme::mix(theme.bg, base, 0.2);
    let radius = match badge {
        Badge::Tinted | Badge::Solid | Badge::Outline => Radius::Medium,
        Badge::Pill | Badge::Capsule => Radius::Full,
        Badge::Dot => Radius::Small,
        Badge::Block => Radius::Square,
    };
    if head {
        // Filled in every style, with a white outline.
        return Look {
            fill: Some(base),
            ink: ink_on(base),
            border: Some((2., theme.text_strong)),
            radius: if badge == Badge::Dot { Radius::Full } else { radius },
            dot: None,
            bold: true,
        };
    }
    // A remote branch is quiet in every style: grey, never in a line's color.
    if role == Role::Remote {
        return match badge {
            Badge::Outline => Look { fill: None, ink: theme.text, border: Some((1., theme.muted)), radius, dot: None, bold: false },
            Badge::Dot => Look { fill: None, ink: theme.text, border: None, radius, dot: Some(theme.muted), bold: false },
            _ => Look { fill: Some(theme.element), ink: theme.text, border: None, radius, dot: None, bold: badge == Badge::Block },
        };
    }
    match badge {
        Badge::Tinted => Look { fill: Some(tint), ink: theme.text_strong, border: Some((1., base)), radius, dot: None, bold: false },
        Badge::Solid => Look { fill: Some(base), ink: ink_on(base), border: None, radius, dot: None, bold: false },
        Badge::Outline => Look { fill: None, ink: theme.text_strong, border: Some((1., base)), radius, dot: None, bold: false },
        Badge::Pill => Look { fill: Some(tint), ink: theme.text_strong, border: None, radius, dot: None, bold: false },
        Badge::Capsule => Look { fill: Some(base), ink: ink_on(base), border: None, radius, dot: None, bold: false },
        Badge::Dot => Look { fill: None, ink: theme.text_strong, border: None, radius, dot: Some(base), bold: false },
        Badge::Block => Look { fill: Some(base), ink: ink_on(base), border: None, radius, dot: None, bold: true },
    }
}

pub struct GraphStyle {
    pub id: &'static str,
    pub name: &'static str,
    /// One short line for the settings card.
    pub note: &'static str,
    /// Line colors, written for a dark background; they are fitted to the theme in use. Empty: the theme's own.
    pub palette: &'static [u32],
    pub shape: Shape,
    /// How thick lines are, against the usual.
    pub weight: f32,
    pub node: Node,
    pub badge: Badge,
}

#[allow(clippy::too_many_arguments)]
const fn style(
    id: &'static str,
    name: &'static str,
    note: &'static str,
    palette: &'static [u32],
    shape: Shape,
    weight: f32,
    node: Node,
    badge: Badge,
) -> GraphStyle {
    GraphStyle { id, name, note, palette, shape, weight, node, badge }
}

/// The styles, in the order the settings show them. The first follows the color theme.
pub const STYLES: &[GraphStyle] = &[
    style("theme", "Theme", "Your color theme's own", &[], Shape::Curve, 1.0, Node::Dot, Badge::Tinted),
    style(
        "aurora",
        "Aurora",
        "Cool northern hues",
        &[0x88c0d0, 0xa3be8c, 0xb48ead, 0xebcb8b, 0x81a1c1, 0xd08770, 0x8fbcbb, 0xbf616a],
        Shape::Curve,
        1.0,
        Node::Dot,
        Badge::Pill,
    ),
    style(
        "neon",
        "Neon",
        "Vivid, straight, square",
        &[0x00e5ff, 0xff3df2, 0xb6ff00, 0xff9100, 0x7c4dff, 0x00e676, 0xffea00, 0xff1744],
        Shape::Straight,
        1.2,
        Node::Square,
        Badge::Block,
    ),
    style(
        "soft",
        "Soft",
        "Pastel, thick and friendly",
        &[0x9ec5fe, 0xf8b4c8, 0xa8e6cf, 0xffd8a8, 0xcdb4f6, 0xfff3a3, 0x8fd3e8, 0xf4a6a0],
        Shape::Curve,
        1.3,
        Node::Dot,
        Badge::Capsule,
    ),
    style(
        "circuit",
        "Circuit",
        "Right angles, hollow",
        &[0x2dd4bf, 0xf59e0b, 0x38bdf8, 0xa3e635, 0xe879f9, 0xfb7185, 0x818cf8, 0xfde047],
        Shape::Elbow,
        0.9,
        Node::Ring,
        Badge::Outline,
    ),
    style(
        "graphite",
        "Graphite",
        "Grey, with a blue main line",
        &[0x7aa2f7, 0xd6d9df, 0x7a7f8a, 0xb0b4bd, 0x5e636d, 0x999ea8],
        Shape::Straight,
        0.85,
        Node::Ring,
        Badge::Dot,
    ),
    style(
        "accessible",
        "Colour-blind safe",
        "The Okabe–Ito palette",
        &[0xe69f00, 0x56b4e9, 0x009e73, 0xf0e442, 0x0072b2, 0xd55e00, 0xcc79a7, 0x999999],
        Shape::Curve,
        1.1,
        Node::Dot,
        Badge::Solid,
    ),
    style(
        "gruvbox",
        "Gruvbox",
        "Warm and retro",
        &[0x83a598, 0xfabd2f, 0xd3869b, 0xb8bb26, 0xfe8019, 0x8ec07c, 0xfb4934, 0xa89984],
        Shape::Straight,
        1.0,
        Node::Square,
        Badge::Block,
    ),
    style(
        "dracula",
        "Dracula",
        "Purple, pink and green",
        &[0xbd93f9, 0x50fa7b, 0xff79c6, 0x8be9fd, 0xffb86c, 0xf1fa8c, 0xff5555, 0x6272a4],
        Shape::Straight,
        1.1,
        Node::Dot,
        Badge::Capsule,
    ),
    style(
        "catppuccin",
        "Catppuccin",
        "Mocha, soft and round",
        &[0x89b4fa, 0xf38ba8, 0xa6e3a1, 0xfab387, 0xcba6f7, 0x94e2d5, 0xf9e2af, 0xf5c2e7],
        Shape::Curve,
        1.15,
        Node::Ring,
        Badge::Pill,
    ),
    style(
        "solarized",
        "Solarized",
        "Balanced, right angles",
        &[0x268bd2, 0xcb4b16, 0x2aa198, 0xd33682, 0x859900, 0x6c71c4, 0xb58900, 0xdc322f],
        Shape::Elbow,
        1.0,
        Node::Dot,
        Badge::Outline,
    ),
    style(
        "tokyo",
        "Tokyo Night",
        "Neon dusk, right angles",
        &[0x7aa2f7, 0xf7768e, 0x9ece6a, 0xff9e64, 0xbb9af7, 0x73daca, 0xe0af68, 0x7dcfff],
        Shape::Elbow,
        1.0,
        Node::Dot,
        Badge::Tinted,
    ),
    style(
        "sunset",
        "Sunset",
        "Coral, gold and orchid",
        &[0xff6b6b, 0xffa94d, 0xcc5de8, 0xffd43b, 0xf06595, 0x845ef7, 0xe8590c, 0xd6336c],
        Shape::Curve,
        1.25,
        Node::Dot,
        Badge::Capsule,
    ),
    style(
        "ocean",
        "Ocean",
        "Blues and greens",
        &[0x4dabf7, 0x20c997, 0x7950f2, 0x66d9e8, 0x69db7c, 0x5c7cfa, 0x3bc9db, 0xa9e34b],
        Shape::Curve,
        1.0,
        Node::Ring,
        Badge::Dot,
    ),
];

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Uses the style with this id; an unknown one means the theme's own.
pub fn set_active(id: &str) {
    ACTIVE.store(STYLES.iter().position(|s| s.id == id).unwrap_or(0), Ordering::Relaxed);
}

pub fn active() -> &'static GraphStyle {
    &STYLES[ACTIVE.load(Ordering::Relaxed).min(STYLES.len() - 1)]
}

/// The colors the active style gives its lines, fitted to the theme in use; made again when either changes.
struct Fitted {
    style: usize,
    background: u32,
    colors: Vec<u32>,
}

static FITTED: RwLock<Option<Fitted>> = RwLock::new(None);

/// The color of branch line number `lineage`.
pub fn lane(lineage: usize) -> u32 {
    let at = ACTIVE.load(Ordering::Relaxed).min(STYLES.len() - 1);
    let style = &STYLES[at];
    let theme = t();
    if style.palette.is_empty() {
        return theme.lane(lineage);
    }
    if let Some(fitted) = FITTED.read().ok().and_then(|f| f.as_ref().map(|f| (f.style, f.background, f.colors.clone())))
        && fitted.0 == at
        && fitted.1 == theme.bg
    {
        return fitted.2[lineage % fitted.2.len()];
    }
    let colors = fitted_palette(style, theme.bg);
    let color = colors[lineage % colors.len()];
    if let Ok(mut slot) = FITTED.write() {
        *slot = Some(Fitted { style: at, background: theme.bg, colors });
    }
    color
}

/// A style's palette as it reads on `background`.
pub fn fitted_palette(style: &GraphStyle, background: u32) -> Vec<u32> {
    if style.palette.is_empty() {
        return t().lanes.clone();
    }
    style.palette.iter().map(|&color| fit(color, background)).collect()
}

// ---- color ---------------------------------------------------------------------------------------

/// The least contrast a line may have against the background.
const MIN_CONTRAST: f32 = 3.1;

fn linear(channel: f32) -> f32 {
    if channel <= 0.04045 { channel / 12.92 } else { ((channel + 0.055) / 1.055).powf(2.4) }
}

fn gamma(channel: f32) -> f32 {
    if channel <= 0.003_130_8 { channel * 12.92 } else { 1.055 * channel.powf(1. / 2.4) - 0.055 }
}

/// Björn Ottosson's Oklab: lightness and chroma that match how the eye sees them, so a color can be made
/// lighter or darker without changing what color it is.
pub fn oklab(color: u32) -> (f32, f32, f32) {
    let channel = |shift: u32| linear(((color >> shift) & 0xff) as f32 / 255.);
    let (r, g, b) = (channel(16), channel(8), channel(0));
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    (
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    )
}

/// The color for an Oklab triple, or `None` when it falls outside what a screen can show.
fn from_oklab(lightness: f32, a: f32, b: f32) -> Option<u32> {
    let l = (lightness + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m = (lightness - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s = (lightness - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let channels = [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_4 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ];
    if channels.iter().any(|c| !(-0.001..=1.001).contains(c)) {
        return None;
    }
    let byte = |c: f32| (gamma(c.clamp(0., 1.)) * 255.).round() as u32;
    Some(byte(channels[0]) << 16 | byte(channels[1]) << 8 | byte(channels[2]))
}

/// How far apart two colors look: the distance in Oklab (0.02 is only just visible, 0.1 is plain).
#[cfg(test)]
pub fn distance(a: u32, b: u32) -> f32 {
    let (a, b) = (oklab(a), oklab(b));
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
}

/// WCAG contrast ratio, 1 to 21.
pub fn contrast(a: u32, b: u32) -> f32 {
    let (a, b) = (theme::luminance(a), theme::luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `color`, moved along lightness (its hue kept, its chroma as much as fits) until it has the contrast a line needs
/// against `background`: lighter on a dark one, darker on a light one.
pub fn fit(color: u32, background: u32) -> u32 {
    if contrast(color, background) >= MIN_CONTRAST {
        return color;
    }
    let (lightness, a, b) = oklab(color);
    let step = if theme::luminance(background) < 0.25 { 0.01 } else { -0.01 };
    let mut at = lightness;
    for _ in 0..90 {
        at += step;
        if !(0.02..=0.99).contains(&at) {
            break;
        }
        // The chroma is eased until the color exists on a screen.
        for ease in [1.0, 0.9, 0.8, 0.7, 0.55, 0.4, 0.25, 0.1, 0.0] {
            if let Some(candidate) = from_oklab(at, a * ease, b * ease) {
                if contrast(candidate, background) >= MIN_CONTRAST {
                    return candidate;
                }
                break;
            }
        }
    }
    color
}

// ---- lines ---------------------------------------------------------------------------------------

/// The path of a line that changes lane within a row. `from` and `to` are where it starts and ends, `mid` the
/// height of the row's middle (where the commit is) and `corner` the radius of an elbow.
pub fn stroke(shape: Shape, half: Half, from: Point<Pixels>, to: Point<Pixels>, mid: Pixels, corner: f32, width: f32) -> Option<Path<Pixels>> {
    let mut line = PathBuilder::stroke(px(width));
    line.move_to(from);
    let dx = f32::from(to.x - from.x);
    if dx == 0. || half == Half::Through && shape != Shape::Elbow || shape == Shape::Straight {
        line.line_to(to);
    } else if shape == Shape::Curve {
        // Comes down the lane then bends into the commit; leaves the commit sideways then bends down.
        line.curve_to(to, if half == Half::Top { point(from.x, mid) } else { point(to.x, mid) });
    } else {
        let side = dx.signum();
        let r = corner.min(dx.abs() / 2.);
        let (bend_in, bend_out) = (px(r), px(side * r));
        match half {
            Half::Top => {
                line.line_to(point(from.x, mid - bend_in));
                line.curve_to(point(from.x + bend_out, mid), point(from.x, mid));
                line.line_to(to);
            }
            Half::Bottom => {
                line.line_to(point(to.x - bend_out, mid));
                line.curve_to(point(to.x, mid + bend_in), point(to.x, mid));
                line.line_to(to);
            }
            Half::Through => {
                line.line_to(point(from.x, mid - bend_in));
                line.curve_to(point(from.x + bend_out, mid), point(from.x, mid));
                line.line_to(point(to.x - bend_out, mid));
                line.curve_to(point(to.x, mid + bend_in), point(to.x, mid));
                line.line_to(to);
            }
        }
    }
    line.build().ok()
}

/// The radius of an elbow for rows `row_h` tall and lanes `lane_w` apart.
pub fn corner(row_h: f32, lane_w: f32) -> f32 {
    (row_h * 0.32).min(lane_w * 0.5)
}

// ---- the picture on a settings card ------------------------------------------------------------------

/// A small history drawn in `style`: a main line, a branch that is merged back, and one that is still open,
/// beside bars standing for the messages.
pub fn paint_preview(style: &GraphStyle, colors: &[u32], bounds: Bounds<Pixels>, window: &mut Window) {
    let theme = t();
    let rows = 5usize;
    let row_h = f32::from(bounds.size.height) / rows as f32;
    let lane_w = (row_h * 1.15).min(20.);
    let radius = (row_h * 0.2).clamp(2.6, 3.6);
    let width = 1.7 * style.weight;
    let color = |lineage: usize| rgb(colors[lineage % colors.len()]);
    let x = |lane: usize| bounds.origin.x + px(14. + lane as f32 * lane_w);
    let top = |row: usize| bounds.origin.y + px(row as f32 * row_h);
    let mid = |row: usize| top(row) + px(row_h / 2.);

    // (half, row, from lane, to lane, the line's color)
    let strokes: [(Half, usize, usize, usize, usize); 13] = [
        (Half::Bottom, 0, 0, 0, 0),
        (Half::Bottom, 0, 0, 1, 1),
        (Half::Through, 1, 0, 0, 0),
        (Half::Top, 1, 1, 1, 1),
        (Half::Bottom, 1, 1, 1, 1),
        (Half::Through, 2, 0, 0, 0),
        (Half::Through, 2, 1, 1, 1),
        (Half::Top, 3, 0, 0, 0),
        (Half::Top, 3, 1, 0, 1),
        (Half::Bottom, 3, 0, 0, 0),
        (Half::Bottom, 3, 0, 2, 2),
        (Half::Top, 4, 0, 0, 0),
        (Half::Top, 4, 2, 2, 2),
    ];
    for (half, row, from, to, lineage) in strokes {
        let (a, b) = match half {
            Half::Top => (point(x(from), top(row)), point(x(to), mid(row))),
            Half::Bottom => (point(x(from), mid(row)), point(x(to), top(row) + px(row_h))),
            Half::Through => (point(x(from), top(row)), point(x(to), top(row) + px(row_h))),
        };
        if let Some(path) = stroke(style.shape, half, a, b, mid(row), corner(row_h, lane_w), width) {
            window.paint_path(path, color(lineage));
        }
    }

    // (row, lane, the line's color)
    for (row, lane, lineage) in [(0usize, 0usize, 0usize), (1, 1, 1), (2, 1, 1), (3, 0, 0), (4, 0, 0), (4, 2, 2)] {
        let center = point(x(lane), mid(row));
        let (r, fill, border) = match style.node {
            Node::Dot => (radius, color(lineage), 0.),
            Node::Ring => (radius + 0.4, rgb(theme.bg), 1.6),
            Node::Square => (radius * 0.3, color(lineage), 0.),
        };
        let half = if style.node == Node::Square { radius * 0.95 } else { radius + if style.node == Node::Ring { 0.4 } else { 0. } };
        window.paint_quad(quad(
            Bounds { origin: point(center.x - px(half), center.y - px(half)), size: size(px(half * 2.), px(half * 2.)) },
            px(if style.node == Node::Square { r } else { half }),
            fill,
            border,
            color(lineage),
            gpui::BorderStyle::default(),
        ));
    }

    // The messages, as bars: the first is bold like a commit on the selected line, and has a label before it, drawn
    // in the style's own way.
    let bars = [0.62, 0.5, 0.7, 0.44, 0.56];
    let left = x(2) + px(lane_w * 0.9);
    let room = f32::from(bounds.origin.x + bounds.size.width - left) - 6.;
    let look = badge_look(&theme, style.badge, Role::Branch, false, colors[1 % colors.len()]);
    let (badge_w, badge_h) = (30., 12.);
    for (row, share) in bars.into_iter().enumerate() {
        let strong = row == 0;
        let tint = rgb(theme::mix(theme.bg, if strong { theme.text_strong } else { theme.muted }, if strong { 0.55 } else { 0.4 }));
        let mut start = left;
        let mut width = room * share;
        if strong {
            let corner = match look.radius {
                Radius::Square => 0.5,
                Radius::Small => 2.5,
                Radius::Medium => 3.5,
                Radius::Full => badge_h / 2.,
            };
            let origin = point(left, mid(row) - px(badge_h / 2.));
            let (border_w, border_c) = look.border.map_or((0., theme.bg), |(w, c)| (w.min(1.4), c));
            if let Some(color) = look.dot {
                window.paint_quad(quad(
                    Bounds { origin: point(origin.x, mid(row) - px(3.)), size: size(px(6.), px(6.)) },
                    px(3.),
                    rgb(color),
                    0.,
                    rgb(color),
                    gpui::BorderStyle::default(),
                ));
            } else {
                window.paint_quad(quad(
                    Bounds { origin, size: size(px(badge_w), px(badge_h)) },
                    px(corner),
                    look.fill.map_or(gpui::transparent_black(), |c| rgb(c).into()),
                    border_w,
                    rgb(border_c),
                    gpui::BorderStyle::default(),
                ));
            }
            // The name inside it, as a short bar in the label's ink.
            let ink = rgb(look.ink);
            let text_x = if look.dot.is_some() { left + px(10.) } else { left + px(6.) };
            let text_w = badge_w - 12.;
            window.paint_quad(quad(
                Bounds { origin: point(text_x, mid(row) - px(1.75)), size: size(px(text_w), px(3.5)) },
                px(1.75),
                ink,
                0.,
                ink,
                gpui::BorderStyle::default(),
            ));
            start = left + px(badge_w + 6.);
            width = (room - badge_w - 6.) * share;
        }
        window.paint_quad(quad(
            Bounds { origin: point(start, mid(row) - px(2.)), size: size(px(width), px(4.)) },
            px(2.),
            tint,
            0.,
            tint,
            gpui::BorderStyle::default(),
        ));
    }
}

/// Tests that change the active style take this, since the style is process-wide and tests run side by side.
#[cfg(test)]
pub static STYLE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_at_least_ten_styles_with_their_own_ids_and_the_first_follows_the_theme() {
        assert!(STYLES.len() >= 10, "{} styles", STYLES.len());
        let mut ids: Vec<_> = STYLES.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), STYLES.len(), "ids are unique");
        assert_eq!(STYLES[0].id, "theme");
        assert!(STYLES[0].palette.is_empty());
        for style in STYLES.iter().skip(1) {
            assert!(style.palette.len() >= 6, "{} has a palette of {}", style.name, style.palette.len());
            assert!((0.7..=1.4).contains(&style.weight), "{} weight {}", style.name, style.weight);
        }
    }

    #[test]
    fn the_looks_differ_in_more_than_color() {
        let shapes: std::collections::HashSet<_> = STYLES.iter().map(|s| std::mem::discriminant(&s.shape)).collect();
        let nodes: std::collections::HashSet<_> = STYLES.iter().map(|s| std::mem::discriminant(&s.node)).collect();
        assert_eq!((shapes.len(), nodes.len()), (3, 3), "every line shape and node shape is used");
    }

    #[test]
    fn every_line_reads_against_every_built_in_theme_and_neighbors_differ() {
        let mut problems = Vec::new();
        for theme in theme::built_in() {
            for style in STYLES.iter().filter(|s| !s.palette.is_empty()) {
                let colors = fitted_palette(style, theme.bg);
                for (i, &color) in colors.iter().enumerate() {
                    let ratio = contrast(color, theme.bg);
                    if ratio < 3.0 {
                        problems.push(format!("{} in {}: line {i} {color:06x} has contrast {ratio:.2}", style.name, theme.name));
                    }
                }
                for i in 0..colors.len() {
                    let next = colors[(i + 1) % colors.len()];
                    let apart = distance(colors[i], next);
                    if apart < 0.045 {
                        problems.push(format!(
                            "{} in {}: lines {i} and {} look alike ({:06x} {:06x}, {apart:.3})",
                            style.name,
                            theme.name,
                            (i + 1) % colors.len(),
                            colors[i],
                            next,
                        ));
                    }
                }
            }
        }
        assert!(problems.is_empty(), "{} problems:\n{}", problems.len(), problems.join("\n"));
    }

    #[test]
    fn every_label_in_every_style_is_readable_and_the_looks_differ() {
        let mut problems = Vec::new();
        for theme in theme::built_in() {
            for style in STYLES {
                let colors = fitted_palette(style, theme.bg);
                for role in [Role::Branch, Role::Remote, Role::Tag, Role::Stash] {
                    for head in [false, true] {
                        let look = badge_look(&theme, style.badge, role, head, colors[1 % colors.len()]);
                        let behind = look.fill.unwrap_or(theme.bg);
                        let ratio = contrast(look.ink, behind);
                        if ratio < 4.0 {
                            problems.push(format!(
                                "{} in {}: {role:?} head={head} {:?} has ink contrast {ratio:.2}",
                                style.name, theme.name, style.badge
                            ));
                        }
                        if head && look.border.is_none() {
                            problems.push(format!("{}: the checked-out branch has no outline", style.name));
                        }
                    }
                }
            }
        }
        assert!(problems.is_empty(), "{} problems:\n{}", problems.len(), problems.join("\n"));
        let kinds: std::collections::HashSet<_> = STYLES.iter().map(|s| std::mem::discriminant(&s.badge)).collect();
        assert_eq!(kinds.len(), 7, "every label look is used by some style");
    }

    #[test]
    fn fitting_keeps_the_hue_and_only_moves_what_is_needed() {
        // Already readable: untouched.
        assert_eq!(fit(0x88c0d0, 0x1e1e1e), 0x88c0d0);
        // Too pale for white: darker, but still the same blue.
        let darker = fit(0x9ec5fe, 0xffffff);
        assert!(contrast(darker, 0xffffff) >= 3.0);
        let (l0, a0, b0) = oklab(0x9ec5fe);
        let (l1, a1, b1) = oklab(darker);
        assert!(l1 < l0);
        assert!(((b1.atan2(a1)) - (b0.atan2(a0))).abs() < 0.12, "the hue stays");
    }

    #[test]
    fn an_unknown_style_means_the_theme_and_a_known_one_changes_the_colors() {
        let _only = STYLE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        set_active("no-such-style");
        assert_eq!(active().id, "theme");
        set_active("neon");
        assert_eq!(active().id, "neon");
        let neon = lane(0);
        set_active("theme");
        assert_eq!(lane(0), t().lane(0));
        assert_ne!(neon, t().lane(0));
    }

    #[test]
    fn every_shape_makes_a_path_for_every_half() {
        let at = |x: f32, y: f32| point(px(x), px(y));
        for shape in [Shape::Curve, Shape::Straight, Shape::Elbow] {
            for (half, from, to) in [
                (Half::Top, at(10., 0.), at(30., 13.)),
                (Half::Bottom, at(10., 13.), at(30., 26.)),
                (Half::Through, at(10., 0.), at(30., 26.)),
                (Half::Through, at(10., 0.), at(10., 26.)),
                (Half::Top, at(30., 0.), at(10., 13.)),
            ] {
                assert!(stroke(shape, half, from, to, px(13.), corner(26., 20.), 2.).is_some(), "{shape:?} {half:?}");
            }
        }
    }
}
