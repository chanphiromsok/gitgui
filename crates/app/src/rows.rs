//! Turns a parsed diff plus its comments into the flat list of fixed-height rows the diff view draws.
//!
//! Comments and the composer are rows of their own, placed right under the line they belong to, so
//! the whole view stays one uniform list.

use gitgui_core::diff::pairs;
use gitgui_core::{DiffLine, FileDiff, LineKind};
use gitgui_store::{Comment, Side};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Unified,
    Split,
}

/// A place a comment can attach: one line on one side of the diff. A removed line only exists on
/// the old side; added and unchanged lines are on the new side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub side: Side,
    pub line: u32,
}

pub fn anchor_of(line: &DiffLine) -> Option<Anchor> {
    match line.kind {
        LineKind::Removed => line.old_no.map(|line| Anchor { side: Side::Old, line }),
        LineKind::Added | LineKind::Context => line.new_no.map(|line| Anchor { side: Side::New, line }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Binary,
    /// Nothing to show: an empty file, or only a mode change.
    NoChanges,
    Truncated,
    /// Comments on lines outside the hunks being shown.
    HiddenComments(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayRow {
    Hunk(usize),
    /// Unified view: one line of a hunk.
    Line { hunk: usize, line: usize },
    /// Split view: an old line and a new line side by side; indices are into the hunk's lines.
    Pair { hunk: usize, left: Option<usize>, right: Option<usize> },
    /// An index into the comment list that was passed in.
    Comment(usize),
    /// The text field for a new comment on this anchor.
    Composer(Anchor),
    Notice(Notice),
}

/// `comments` are the ones for this file in this commit. `composing` is the line a comment is being
/// written on, if any.
pub fn display_rows(diff: &FileDiff, mode: Mode, comments: &[Comment], composing: Option<Anchor>) -> Vec<DisplayRow> {
    if diff.binary {
        return vec![DisplayRow::Notice(Notice::Binary)];
    }
    if diff.hunks.is_empty() {
        return vec![DisplayRow::Notice(Notice::NoChanges)];
    }

    let mut rows = Vec::new();
    let mut placed = vec![false; comments.len()];
    let mut place = |rows: &mut Vec<DisplayRow>, anchors: &[Anchor]| {
        for (n, anchor) in anchors.iter().enumerate() {
            if anchors[..n].contains(anchor) {
                continue;
            }
            for (i, comment) in comments.iter().enumerate() {
                if comment.side == anchor.side && comment.line == anchor.line {
                    rows.push(DisplayRow::Comment(i));
                    placed[i] = true;
                }
            }
            if composing == Some(*anchor) {
                rows.push(DisplayRow::Composer(*anchor));
            }
        }
    };

    for (h, hunk) in diff.hunks.iter().enumerate() {
        rows.push(DisplayRow::Hunk(h));
        match mode {
            Mode::Unified => {
                for (l, line) in hunk.lines.iter().enumerate() {
                    rows.push(DisplayRow::Line { hunk: h, line: l });
                    let anchors: Vec<Anchor> = anchor_of(line).into_iter().collect();
                    place(&mut rows, &anchors);
                }
            }
            Mode::Split => {
                for pair in pairs(hunk) {
                    rows.push(DisplayRow::Pair { hunk: h, left: pair.left, right: pair.right });
                    let anchors: Vec<Anchor> = [pair.left, pair.right]
                        .into_iter()
                        .flatten()
                        .filter_map(|l| anchor_of(&hunk.lines[l]))
                        .collect();
                    place(&mut rows, &anchors);
                }
            }
        }
    }

    if diff.truncated {
        rows.push(DisplayRow::Notice(Notice::Truncated));
    }
    let hidden = placed.iter().filter(|placed| !**placed).count();
    if hidden > 0 {
        rows.insert(0, DisplayRow::Notice(Notice::HiddenComments(hidden)));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitgui_core::diff::parse;

    // old: a b c d      new: a B c d e
    const DIFF: &str = "@@ -1,4 +1,5 @@\n a\n-b\n+B\n c\n d\n+e\n";

    fn comment(side: Side, line: u32, text: &str) -> Comment {
        Comment {
            id: text.into(),
            commit: "c".into(),
            path: "f".into(),
            side,
            line,
            text: text.into(),
            created: 0,
            resolved: false,
        }
    }

    use DisplayRow::{Comment as C, Composer, Hunk, Line, Notice as N, Pair};

    #[test]
    fn unified_rows_are_a_header_then_every_line() {
        let rows = display_rows(&parse(DIFF), Mode::Unified, &[], None);
        assert_eq!(
            rows,
            [
                Hunk(0),
                Line { hunk: 0, line: 0 },
                Line { hunk: 0, line: 1 },
                Line { hunk: 0, line: 2 },
                Line { hunk: 0, line: 3 },
                Line { hunk: 0, line: 4 },
                Line { hunk: 0, line: 5 },
            ]
        );
    }

    #[test]
    fn a_comment_sits_under_its_line_and_a_removed_line_is_on_the_old_side() {
        // Old line 2 is the removed `b`; new line 2 is the added `B`. Same number, different lines.
        let comments = [comment(Side::Old, 2, "about b"), comment(Side::New, 2, "about B")];
        let rows = display_rows(&parse(DIFF), Mode::Unified, &comments, None);
        assert_eq!(
            rows,
            [
                Hunk(0),
                Line { hunk: 0, line: 0 },
                Line { hunk: 0, line: 1 }, // -b
                C(0),
                Line { hunk: 0, line: 2 }, // +B
                C(1),
                Line { hunk: 0, line: 3 },
                Line { hunk: 0, line: 4 },
                Line { hunk: 0, line: 5 },
            ]
        );
    }

    #[test]
    fn a_comment_on_an_unchanged_line_is_on_the_new_side() {
        // `c` is old line 3 and new line 3.
        let on_new = [comment(Side::New, 3, "ctx")];
        let rows = display_rows(&parse(DIFF), Mode::Unified, &on_new, None);
        assert!(rows.contains(&C(0)));

        let on_old = [comment(Side::Old, 3, "ctx")];
        let rows = display_rows(&parse(DIFF), Mode::Unified, &on_old, None);
        assert!(!rows.contains(&C(0)), "a context line is not an old-side anchor");
        assert_eq!(rows[0], N(Notice::HiddenComments(1)));
    }

    #[test]
    fn the_composer_follows_the_existing_comments_on_its_line() {
        let comments = [comment(Side::New, 5, "first")];
        let at = Anchor { side: Side::New, line: 5 };
        let rows = display_rows(&parse(DIFF), Mode::Unified, &comments, Some(at));
        let tail: Vec<DisplayRow> = rows.iter().rev().take(3).rev().copied().collect();
        assert_eq!(tail, [Line { hunk: 0, line: 5 }, C(0), Composer(at)]);
    }

    #[test]
    fn split_rows_pair_the_changed_line_and_attach_comments_after_the_pair() {
        let comments = [comment(Side::Old, 2, "old b"), comment(Side::New, 2, "new B")];
        let rows = display_rows(&parse(DIFF), Mode::Split, &comments, None);
        assert_eq!(
            rows,
            [
                Hunk(0),
                Pair { hunk: 0, left: Some(0), right: Some(0) },
                Pair { hunk: 0, left: Some(1), right: Some(2) },
                C(0),
                C(1),
                Pair { hunk: 0, left: Some(3), right: Some(3) },
                Pair { hunk: 0, left: Some(4), right: Some(4) },
                Pair { hunk: 0, left: None, right: Some(5) },
            ]
        );
    }

    #[test]
    fn a_context_pair_shows_a_comment_once() {
        let comments = [comment(Side::New, 1, "on a")];
        let rows = display_rows(&parse(DIFF), Mode::Split, &comments, None);
        assert_eq!(rows.iter().filter(|r| **r == C(0)).count(), 1);
    }

    #[test]
    fn comments_outside_the_shown_hunks_are_counted_not_lost() {
        let comments = [comment(Side::New, 500, "far away"), comment(Side::New, 2, "here")];
        let rows = display_rows(&parse(DIFF), Mode::Unified, &comments, None);
        assert_eq!(rows[0], N(Notice::HiddenComments(1)));
        assert!(rows.contains(&C(1)));
    }

    #[test]
    fn binary_empty_and_truncated_diffs_say_so() {
        assert_eq!(display_rows(&parse("Binary files a/x and b/x differ\n"), Mode::Unified, &[], None), [N(Notice::Binary)]);
        assert_eq!(display_rows(&parse(""), Mode::Split, &[], None), [N(Notice::NoChanges)]);

        let mut text = String::from("@@ -0,0 +1,25000 @@\n");
        for i in 0..25_000 {
            text.push_str(&format!("+l{i}\n"));
        }
        let rows = display_rows(&parse(&text), Mode::Unified, &[], None);
        assert_eq!(rows.last(), Some(&N(Notice::Truncated)));
    }

    #[test]
    fn anchors_follow_the_line_kind() {
        let diff = parse(DIFF);
        let lines = &diff.hunks[0].lines;
        assert_eq!(anchor_of(&lines[0]), Some(Anchor { side: Side::New, line: 1 }));
        assert_eq!(anchor_of(&lines[1]), Some(Anchor { side: Side::Old, line: 2 }));
        assert_eq!(anchor_of(&lines[2]), Some(Anchor { side: Side::New, line: 2 }));
    }
}
