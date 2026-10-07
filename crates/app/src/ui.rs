//! Colors and small widgets shared by the views.

use gitgui_core::CommitKind;
use gpui::{
    BorderStyle, Bounds, ElementId, PathBuilder, Pixels, Point, Rgba, SharedString, Stateful, Window, canvas, div, point,
    prelude::*, px, quad, rgb, size,
};
use crate::theme::t;


/// The color of a branch line, from the theme's accents. Lines are numbered in the order they
/// start, so neighbors differ.
pub fn line_color(lineage: usize) -> Rgba {
    rgb(t().lane(lineage))
}

/// A file or folder icon, 14 pixels square; empty space when there is none.
pub fn file_icon(image: Option<std::sync::Arc<gpui::RenderImage>>) -> gpui::AnyElement {
    match image {
        Some(image) => gpui::img(image).flex_none().size(px(14.)).into_any_element(),
        None => div().flex_none().size(px(14.)).into_any_element(),
    }
}

/// An author's picture in a circle `size` pixels across, or their initials on their own color.
pub fn avatar(name: &str, email: &str, avatar: crate::avatars::Avatar, size: f32) -> gpui::AnyElement {
    use crate::avatars::{Avatar, hue, initials};
    match avatar {
        Avatar::Picture(image) => gpui::img(image).flex_none().size(px(size)).rounded_full().into_any_element(),
        Avatar::Pending | Avatar::None => {
            let color = t().lane(hue(email));
            div()
                .flex_none()
                .size(px(size))
                .rounded_full()
                .bg(rgb(color))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(text_on(color)))
                .text_size(px(size * 0.45))
                .font_weight(gpui::FontWeight::BOLD)
                .child(initials(name))
                .into_any_element()
        }
    }
}

/// Dark or light text, whichever reads on `background`.
pub fn text_on(background: u32) -> u32 {
    crate::theme::text_on(background)
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
            rgb(t().bg),
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
        .bg(rgb(t().element))
        .text_xs()
        .cursor_pointer()
        .hover(|style| style.bg(rgb(t().element_hover)))
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
