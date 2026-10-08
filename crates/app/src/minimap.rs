//! The strip beside a diff that shows where the changes are, so a long file can be read by block:
//! a colored tick for every run of added, removed or commented rows, and the part in view.
//!
//! Rows are not all the same height (a hunk header is a little taller, a comment card much taller), so
//! positions are worked out from an estimate of each row's height. That makes the ticks land within a
//! row or two of the code they stand for, which is all a map is for.

use gitgui_core::{FileDiff, LineKind};

use crate::rows::DisplayRow;

/// What a tick says happened in its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkKind {
    Added,
    Removed,
    /// Lines removed and added side by side (split view).
    Changed,
    Comment,
}

/// A run of rows of one kind: where it starts and how tall it is, in estimated pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    pub top: f32,
    pub height: f32,
    pub kind: MarkKind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Minimap {
    /// Shared, so drawing a frame copies a pointer, not thousands of ticks.
    pub marks: std::sync::Arc<Vec<Mark>>,
    /// Where each row starts, in estimated pixels, and (last) the total.
    pub offsets: Vec<f32>,
}

impl Minimap {
    pub fn total(&self) -> f32 {
        self.offsets.last().copied().unwrap_or(0.)
    }

    /// The row at `fraction` (0 to 1) of the way down.
    pub fn row_at(&self, fraction: f32) -> usize {
        let rows = self.offsets.len().saturating_sub(1);
        if rows == 0 {
            return 0;
        }
        let y = fraction.clamp(0., 1.) * self.total();
        // The first offset beyond y, less one, is the row y falls in.
        (self.offsets.partition_point(|&offset| offset <= y).saturating_sub(1)).min(rows - 1)
    }
}

fn estimated_height(row: &DisplayRow, line_h: f32) -> f32 {
    match row {
        DisplayRow::Line { .. } | DisplayRow::Pair { .. } => line_h,
        DisplayRow::Hunk(_) | DisplayRow::Tail => line_h + 4.,
        DisplayRow::Notice(_) => line_h + 10.,
        DisplayRow::Comment(_) => 84.,
        DisplayRow::Composer(_) => 150.,
    }
}

fn kind_of(row: &DisplayRow, diff: &FileDiff) -> Option<MarkKind> {
    let line_kind = |hunk: usize, line: usize| diff.hunks.get(hunk).and_then(|h| h.lines.get(line)).map(|l| l.kind);
    match *row {
        DisplayRow::Line { hunk, line } => match line_kind(hunk, line)? {
            LineKind::Added => Some(MarkKind::Added),
            LineKind::Removed => Some(MarkKind::Removed),
            LineKind::Context => None,
        },
        DisplayRow::Pair { hunk, left, right } => {
            let removed = left.and_then(|l| line_kind(hunk, l)) == Some(LineKind::Removed);
            let added = right.and_then(|r| line_kind(hunk, r)) == Some(LineKind::Added);
            match (removed, added) {
                (true, true) => Some(MarkKind::Changed),
                (true, false) => Some(MarkKind::Removed),
                (false, true) => Some(MarkKind::Added),
                (false, false) => None,
            }
        }
        DisplayRow::Comment(_) | DisplayRow::Composer(_) => Some(MarkKind::Comment),
        DisplayRow::Hunk(_) | DisplayRow::Notice(_) | DisplayRow::Tail => None,
    }
}

/// The ticks and row positions for a diff's rows. `line_h` is the height of one code line.
pub fn build(diff: &FileDiff, rows: &[DisplayRow], line_h: f32) -> Minimap {
    let mut offsets = Vec::with_capacity(rows.len() + 1);
    let mut marks: Vec<Mark> = Vec::new();
    let mut y = 0.;
    let mut open: Option<MarkKind> = None;
    for row in rows {
        offsets.push(y);
        let height = estimated_height(row, line_h);
        match kind_of(row, diff) {
            // The same kind continuing: the tick grows.
            Some(kind) if open == Some(kind) => {
                if let Some(mark) = marks.last_mut() {
                    mark.height += height;
                }
            }
            Some(kind) => {
                marks.push(Mark { top: y, height, kind });
                open = Some(kind);
            }
            None => open = None,
        }
        y += height;
    }
    offsets.push(y);
    Minimap { marks: std::sync::Arc::new(marks), offsets }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::{Mode, display_rows};
    use gitgui_core::diff::parse;

    const DIFF: &str = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,6 +1,7 @@\n a\n b\n-c\n+C\n+C2\n d\n e\n f\n";

    #[test]
    fn a_run_of_changed_lines_is_one_tick_and_context_has_none() {
        let diff = parse(DIFF);
        let rows = display_rows(&diff, Mode::Unified, &[], None);
        let map = build(&diff, &rows, 20.);
        let kinds: Vec<MarkKind> = map.marks.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, [MarkKind::Removed, MarkKind::Added], "one removed line, then two added lines");
        assert_eq!(map.marks[1].height, 40.);
        assert_eq!(map.offsets.len(), rows.len() + 1);
    }

    #[test]
    fn split_view_marks_a_replaced_line_as_changed() {
        let diff = parse(DIFF);
        let rows = display_rows(&diff, Mode::Split, &[], None);
        let map = build(&diff, &rows, 20.);
        assert_eq!(map.marks.first().map(|m| m.kind), Some(MarkKind::Changed));
    }

    #[test]
    fn a_position_down_the_strip_finds_its_row() {
        let diff = parse(DIFF);
        let rows = display_rows(&diff, Mode::Unified, &[], None);
        let map = build(&diff, &rows, 20.);
        assert_eq!(map.row_at(0.), 0);
        assert_eq!(map.row_at(1.), rows.len() - 1);
        let tick = map.marks[0];
        assert_eq!(map.offsets[map.row_at((tick.top + 1.) / map.total())], tick.top, "the click lands on the changed row");
        assert_eq!(Minimap::default().row_at(0.5), 0, "an empty diff does not panic");
    }
}
