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
        assert!(now <= 1000. - 240. - GRAPH_MIN + 0.01 && now >= PANE_MIN);
    }
}
