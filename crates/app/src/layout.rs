//! How wide the sidebar and the file pane are allowed to be.
//!
//! The window is split left to right into the sidebar, the graph, and (once a commit is picked) the
//! file pane. Dragging a divider asks for a width; these functions say what it may be, so that
//! neither the graph nor the other pane is ever squeezed out, even after the window is made smaller.

pub const SIDEBAR_DEFAULT: f32 = 240.;
pub const SIDEBAR_MIN: f32 = 170.;
pub const SIDEBAR_MAX: f32 = 480.;
pub const PANE_MIN: f32 = 420.;
/// The least room the graph keeps.
pub const GRAPH_MIN: f32 = 320.;
/// Without a width of its own, the file pane takes this share of what is right of the sidebar.
pub const PANE_SHARE: f32 = 0.56;
/// The share while a file's diff is open: the code is what is being read, so it gets most of the width.
pub const PANE_SHARE_READING: f32 = 0.76;

/// How the date is written in the graph's Date column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateStyle {
    /// `7 Oct 2026 15:32`.
    Full,
    /// `7 Oct 15:32`: no year.
    Short,
    Hidden,
}

/// Which of the graph's columns after Description fit. Description always gets its share first: the
/// others are dropped, the date shortened or hidden, author and commit hidden, until it has room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Columns {
    pub date: DateStyle,
    pub author: bool,
    pub commit: bool,
}

impl Columns {
    pub const DATE_W: f32 = 130.;
    pub const SHORT_DATE_W: f32 = 92.;
    pub const AUTHOR_W: f32 = 130.;
    pub const COMMIT_W: f32 = 64.;
    /// What Description is kept at least, before anything else is given room.
    pub const DESCRIPTION_MIN: f32 = 340.;
    /// The row's padding and the gap between its columns.
    const PADDING: f32 = 16.;
    const GAP: f32 = 8.;

    /// The columns for a graph area `area` wide whose graph drawing takes `graph`.
    pub fn fit(area: f32, graph: f32) -> Self {
        let tiers = [
            Columns { date: DateStyle::Full, author: true, commit: true },
            Columns { date: DateStyle::Full, author: false, commit: false },
            Columns { date: DateStyle::Short, author: false, commit: false },
        ];
        let room = area - graph - Self::PADDING - Self::GAP;
        tiers
            .into_iter()
            .find(|tier| room - tier.width() >= Self::DESCRIPTION_MIN)
            .unwrap_or(Columns { date: DateStyle::Hidden, author: false, commit: false })
    }

    pub fn date_width(&self) -> f32 {
        match self.date {
            DateStyle::Full => Self::DATE_W,
            DateStyle::Short => Self::SHORT_DATE_W,
            DateStyle::Hidden => 0.,
        }
    }

    /// What the columns take, with the gap before each.
    fn width(&self) -> f32 {
        let mut width = 0.;
        if self.date != DateStyle::Hidden {
            width += self.date_width() + Self::GAP;
        }
        if self.author {
            width += Self::AUTHOR_W + Self::GAP;
        }
        if self.commit {
            width += Self::COMMIT_W + Self::GAP;
        }
        width
    }
}

/// `7 Oct 2026 15:32` as `7 Oct 15:32`.
pub fn short_date(date: &str) -> String {
    date.split_whitespace()
        .filter(|word| !(word.len() == 4 && word.chars().all(|c| c.is_ascii_digit())))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The sidebar's width for a requested one in a window `total` wide.
pub fn sidebar_width(requested: f32, total: f32, pane_open: bool) -> f32 {
    let reserved = GRAPH_MIN + if pane_open { PANE_MIN } else { 0. };
    let most = SIDEBAR_MAX.min(total - reserved).max(SIDEBAR_MIN);
    requested.clamp(SIDEBAR_MIN, most)
}

/// The file pane's width. `requested` is what the user dragged it to, if they have.
pub fn pane_width(requested: Option<f32>, total: f32, sidebar: f32) -> f32 {
    let most = (total - sidebar - GRAPH_MIN).max(PANE_MIN);
    requested.unwrap_or((total - sidebar) * PANE_SHARE).clamp(PANE_MIN, most)
}

/// Dragging the sidebar's divider left of this hides the sidebar, as in VS Code and Zed.
pub const SIDEBAR_HIDE_AT: f32 = SIDEBAR_MIN / 2.;

/// The sidebar's new width when its divider is dragged to `x`, measured from the window's left edge.
pub fn sidebar_at(x: f32, total: f32, pane_open: bool) -> f32 {
    sidebar_width(x, total, pane_open)
}

/// The file pane's new width when its divider is dragged to `x`; the pane runs from there to the
/// window's right edge.
pub fn pane_at(x: f32, total: f32, sidebar: f32) -> f32 {
    pane_width(Some(total - x), total, sidebar)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: f32 = 1600.;

    #[test]
    fn description_keeps_its_room_and_the_other_columns_give_way_first() {
        use super::{Columns, DateStyle};
        let graph = 100.;
        assert_eq!(Columns::fit(1400., graph), Columns { date: DateStyle::Full, author: true, commit: true });
        let tier = |area| Columns::fit(area, graph);
        // Narrower: author and commit go, then the year, then the date.
        assert_eq!(tier(800.), Columns { date: DateStyle::Full, author: false, commit: false });
        assert_eq!(tier(610.), Columns { date: DateStyle::Full, author: false, commit: false });
        assert_eq!(tier(580.), Columns { date: DateStyle::Short, author: false, commit: false });
        assert_eq!(tier(540.).date, DateStyle::Hidden);
        // Whatever is shown, Description keeps at least its minimum (unless even that does not fit).
        for area in [500., 540., 580., 640., 800., 1000., 1400., 2000.] {
            let cols = tier(area);
            let used = graph + 16. + 8. + cols.width();
            assert!(area - used >= Columns::DESCRIPTION_MIN || cols.date == DateStyle::Hidden, "{area}: {cols:?}");
        }
    }

    #[test]
    fn a_short_date_drops_only_the_year() {
        assert_eq!(super::short_date("7 Oct 2026 15:32"), "7 Oct 15:32");
        assert_eq!(super::short_date("12 Jan 2025 09:05"), "12 Jan 09:05");
        assert_eq!(super::short_date(""), "");
    }

    #[test]
    fn the_sidebar_stays_between_its_limits() {
        assert_eq!(sidebar_width(240., WIDE, true), 240.);
        assert_eq!(sidebar_width(10., WIDE, true), SIDEBAR_MIN);
        assert_eq!(sidebar_width(900., WIDE, true), SIDEBAR_MAX);
    }

    #[test]
    fn the_sidebar_leaves_room_for_the_graph_and_an_open_pane() {
        // 1000 wide: with a pane open, 1000 - 320 - 420 = 260 is all the sidebar may take.
        assert_eq!(sidebar_width(480., 1000., true), 260.);
        // With no pane, 1000 - 320 = 680 is more than the cap.
        assert_eq!(sidebar_width(480., 1000., false), 480.);
    }

    #[test]
    fn a_window_too_small_for_everything_still_gets_a_sidebar() {
        assert_eq!(sidebar_width(300., 600., true), SIDEBAR_MIN);
    }

    #[test]
    fn the_pane_defaults_to_a_share_of_the_room_beside_the_sidebar() {
        let expected = (WIDE - 240.) * PANE_SHARE;
        assert!((pane_width(None, WIDE, 240.) - expected).abs() < 0.01);
    }

    #[test]
    fn the_pane_keeps_its_minimum_and_leaves_the_graph_its_room() {
        assert_eq!(pane_width(Some(100.), WIDE, 240.), PANE_MIN);
        assert_eq!(pane_width(Some(5000.), WIDE, 240.), WIDE - 240. - GRAPH_MIN);
    }

    #[test]
    fn the_pane_never_goes_below_its_minimum_even_in_a_tiny_window() {
        assert_eq!(pane_width(None, 700., 240.), PANE_MIN);
        assert_eq!(pane_width(Some(900.), 700., 240.), PANE_MIN);
    }

    #[test]
    fn dragging_a_divider_asks_for_the_width_it_is_dragged_to() {
        assert_eq!(sidebar_at(300., WIDE, true), 300.);
        // The pane's divider at x = 900 in a 1600 window makes the pane 700 wide.
        assert_eq!(pane_at(900., WIDE, 240.), 700.);
        // Dragged past the graph's minimum, it stops there.
        assert_eq!(pane_at(100., WIDE, 240.), WIDE - 240. - GRAPH_MIN);
    }

    #[test]
    fn a_saved_width_is_pulled_back_in_when_the_window_shrinks() {
        let was = pane_width(Some(900.), WIDE, 240.);
        let now = pane_width(Some(was), 1000., 240.);
        assert!((PANE_MIN..=1000. - 240. - GRAPH_MIN + 0.01).contains(&now));
    }
}
