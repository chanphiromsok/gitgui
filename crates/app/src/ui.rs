//! Colors and small widgets shared by the views.

use gitgui_core::CommitKind;
use gpui::{
    BorderStyle, Bounds, ElementId, PathBuilder, Pixels, Point, Rgba, SharedString, Stateful, Window, canvas, div, point,
    prelude::*, px, quad, rgb, size,
};

pub const BG: u32 = 0x1e1e1e;
pub const PANEL: u32 = 0x252526;
pub const BORDER: u32 = 0x333333;
pub const TEXT: u32 = 0xd4d4d4;
pub const MUTED: u32 = 0x858585;
pub const ACCENT: u32 = 0x4fc1ff;
pub const LINK: u32 = 0x3794ff;
pub const SELECTED: u32 = 0x264f78;
pub const HOVER: u32 = 0x2a2d2e;
pub const HEAD_ROW: u32 = 0x1b2a38;
pub const ADDED: u32 = 0x4ec9b0;
pub const REMOVED: u32 = 0xf48771;
pub const MODIFIED: u32 = 0xe5c07b;
pub const WARNING: u32 = 0xe5c07b;

const LANE_COLORS: [u32; 8] = [0x4fc1ff, 0xc586c0, 0xdcdcaa, 0x4ec9b0, 0xce9178, 0x569cd6, 0xb5cea8, 0xd16969];

/// The color of a branch line. Lines are numbered in the order they start, so neighbors differ.
pub fn line_color(lineage: usize) -> Rgba {
    rgb(LANE_COLORS[lineage % LANE_COLORS.len()])
}

pub const PR_COLOR: u32 = 0xa371f7;
pub const MERGE_COLOR: u32 = 0x8b7bb8;
pub const COMMIT_COLOR: u32 = 0x7d8590;

/// The small icon beside a commit: a pull request, a merge, or a plain commit.
pub fn kind_icon(kind: CommitKind) -> impl IntoElement + use<> {
    canvas(|_, _, _| (), move |bounds, _, window, _| paint_icon(kind, bounds, window)).w(px(14.)).h(px(14.)).flex_none()
}

fn paint_icon(kind: CommitKind, bounds: Bounds<Pixels>, window: &mut Window) {
    let at = |x: f32, y: f32| point(bounds.origin.x + px(x), bounds.origin.y + px(y));
    let line = |window: &mut Window, color: Rgba, points: &[Point<Pixels>]| {
        let mut path = PathBuilder::stroke(px(1.4));
        path.move_to(points[0]);
        for next in &points[1..] {
            path.line_to(*next);
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    };
    let ring = |window: &mut Window, color: Rgba, center: Point<Pixels>, radius: f32| {
        window.paint_quad(quad(
            Bounds { origin: point(center.x - px(radius), center.y - px(radius)), size: size(px(radius * 2.), px(radius * 2.)) },
            px(radius),
            rgb(BG),
            1.4,
            color,
            BorderStyle::default(),
        ));
    };
    match kind {
        CommitKind::Commit => {
            let color = rgb(COMMIT_COLOR);
            line(window, color, &[at(0., 7.), at(14., 7.)]);
            ring(window, color, at(7., 7.), 3.3);
        }
        CommitKind::Merge => {
            let color = rgb(MERGE_COLOR);
            line(window, color, &[at(3., 3.5), at(3., 10.5)]);
            line(window, color, &[at(3., 7.), at(6., 7.), at(10., 7.)]);
            ring(window, color, at(3., 3.), 2.2);
            ring(window, color, at(3., 11.), 2.2);
            ring(window, color, at(11., 7.), 2.2);
        }
        CommitKind::PullRequest => {
            let color = rgb(PR_COLOR);
            line(window, color, &[at(3., 3.5), at(3., 10.5)]);
            line(window, color, &[at(11., 10.5), at(11., 4.)]);
            // The arrow head, pointing at the branch it lands on.
            line(window, color, &[at(8.8, 5.8), at(11., 3.6), at(13.2, 5.8)]);
            ring(window, color, at(3., 3.), 2.2);
            ring(window, color, at(3., 11.), 2.2);
            ring(window, color, at(11., 11.), 2.2);
        }
    }
}

pub const MONO: &str = "Menlo";

/// A small text button.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .px_2()
        .h(px(22.))
        .flex()
        .items_center()
        .rounded_sm()
        .bg(rgb(0x3a3d41))
        .text_xs()
        .cursor_pointer()
        .hover(|style| style.bg(rgb(0x4a4d51)))
        .child(label.into())
}

/// A small hollow circle, the marker for the current branch.
pub fn ring(color: Rgba) -> impl IntoElement {
    div().flex_none().size(px(9.)).rounded_full().border_2().border_color(color)
}

/// "just now", "5 min ago", "3 d ago": enough to tell fresh comments from old ones, with no time zones.
pub fn ago(seconds: i64) -> String {
    match seconds {
        s if s < 45 => "just now".into(),
        s if s < 90 * 60 => format!("{} min ago", (s + 30) / 60),
        s if s < 36 * 3600 => format!("{} h ago", (s + 1800) / 3600),
        s => format!("{} d ago", (s + 43_200) / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::ago;

    #[test]
    fn relative_times_round_to_a_sensible_unit() {
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(-5), "just now", "a clock a little behind is not an error");
        assert_eq!(ago(60), "1 min ago");
        assert_eq!(ago(59 * 60), "59 min ago");
        assert_eq!(ago(2 * 3600), "2 h ago");
        assert_eq!(ago(3 * 86_400), "3 d ago");
    }
}
