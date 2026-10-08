//! Parses `git diff` output into hunks and lines, and pairs lines for a side-by-side view.

/// Past this many lines a diff is cut short, so one huge generated file cannot freeze the UI.
pub const MAX_LINES: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line number in the old file; `None` for an added line.
    pub old_no: Option<u32>,
    /// Line number in the new file; `None` for a removed line.
    pub new_no: Option<u32>,
    pub text: String,
    /// Git printed `\ No newline at end of file` after this line.
    pub no_newline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@ section` line.
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub hunks: Vec<Hunk>,
    /// Git reported `Binary files ... differ`; there are no lines to show.
    pub binary: bool,
    /// The diff was cut at [`MAX_LINES`].
    pub truncated: bool,
}

impl FileDiff {
    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|hunk| hunk.lines.len()).sum()
    }
}

/// Parses the output of `git diff` (or `git show`) for one file.
pub fn parse(output: &str) -> FileDiff {
    let mut diff = FileDiff::default();
    let mut current: Option<Hunk> = None;
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let mut seen = 0usize;

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("@@ ") {
            diff.hunks.extend(current.take());
            let Some((old_start, new_start)) = parse_hunk_ranges(rest) else { continue };
            (old_no, new_no) = (old_start, new_start);
            current = Some(Hunk { header: line.to_owned(), old_start, new_start, lines: Vec::new() });
            continue;
        }
        let Some(hunk) = current.as_mut() else {
            // Before the first hunk: file headers, or the marker for a binary file.
            if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                diff.binary = true;
            }
            continue;
        };
        if line.starts_with('\\') {
            if let Some(last) = hunk.lines.last_mut() {
                last.no_newline = true;
            }
            continue;
        }
        if seen >= MAX_LINES {
            diff.truncated = true;
            break;
        }
        let (kind, text) = match line.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &line[1..]),
            Some(b'-') => (LineKind::Removed, &line[1..]),
            Some(b' ') => (LineKind::Context, &line[1..]),
            // An empty line inside a hunk is a context line whose leading space was trimmed.
            None => (LineKind::Context, ""),
            // The next file's `diff --git` header, or anything else that is not part of the hunk.
            Some(_) => continue,
        };
        let (old, new) = match kind {
            LineKind::Context => (Some(old_no), Some(new_no)),
            LineKind::Removed => (Some(old_no), None),
            LineKind::Added => (None, Some(new_no)),
        };
        if old.is_some() {
            old_no += 1;
        }
        if new.is_some() {
            new_no += 1;
        }
        hunk.lines.push(DiffLine { kind, old_no: old, new_no: new, text: text.to_owned(), no_newline: false });
        seen += 1;
    }
    diff.hunks.extend(current);
    diff
}

/// `-12,5 +14,7 @@ section` → (12, 14). A missing length means 1 and does not matter here.
fn parse_hunk_ranges(rest: &str) -> Option<(u32, u32)> {
    let mut parts = rest.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let start = |range: &str| range.split(',').next()?.parse().ok();
    Some((start(old)?, start(new)?))
}

// ---- showing more of the unchanged text ---------------------------------------------------------

/// How many unchanged lines one click on an arrow shows.
pub const REVEAL_STEP: u32 = 20;

/// How much of the hidden text in one gap (the unchanged lines between two hunks, or before the first, or after the
/// last) is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reveal {
    /// Lines shown under the hunk before the gap.
    pub below: u32,
    /// Lines shown over the hunk after the gap.
    pub above: u32,
}

/// A gap that still has hidden lines: which one (0 is before the first hunk of the diff git gave) and how many are left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    pub index: usize,
    pub left: u32,
}

/// A diff with more unchanged lines in it, and where lines are still hidden.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Revealed {
    pub diff: FileDiff,
    /// For each hunk of `diff`: the gap above it, when lines are still hidden there.
    pub above: Vec<Option<Gap>>,
    /// The gap after the last hunk, when lines are still hidden there.
    pub below: Option<Gap>,
}

/// Where a hunk sits in both files: the first and last line number on each side.
struct Extent {
    new: (u32, u32),
    old: (u32, u32),
}

fn extent(hunk: &Hunk) -> Option<Extent> {
    let numbers = |side: fn(&DiffLine) -> Option<u32>| {
        let first = hunk.lines.iter().find_map(side)?;
        let last = hunk.lines.iter().rev().find_map(side)?;
        Some((first, last))
    };
    Some(Extent { new: numbers(|line| line.new_no)?, old: numbers(|line| line.old_no)? })
}

/// The lines of a file as they are shown: without the line break at the end of each.
fn file_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// The diff with `reveals[g]` more unchanged lines in gap `g` (missing ones show nothing more), taken from `new_text`, the
/// file after the change. A gap shown in full joins the two hunks around it. A diff that cannot be read this way (a
/// hunk with no context at all, one cut short, a file that is not what the diff is of) is given back as it is.
pub fn reveal(diff: &FileDiff, new_text: &str, reveals: &[Reveal]) -> Revealed {
    let as_is = || Revealed { diff: diff.clone(), above: diff.hunks.iter().map(|_| None).collect(), below: None };
    let file = file_lines(new_text);
    let Some(extents) = diff.hunks.iter().map(extent).collect::<Option<Vec<_>>>() else { return as_is() };
    if extents.is_empty() || diff.truncated || diff.binary {
        return as_is();
    }
    // The file must be the one the diff is of: every line the diff says is on the new side is there.
    let matches = diff.hunks.iter().flat_map(|h| &h.lines).all(|line| {
        line.new_no.is_none_or(|n| n >= 1 && file.get(n as usize - 1).is_some_and(|text| *text == line.text))
    });
    if !matches {
        return as_is();
    }
    let total = file.len() as u32;
    let count = extents.len();
    // Hidden lines in each gap, and the offset between the two files' numbers there.
    let mut hidden = Vec::with_capacity(count + 1);
    let mut delta = Vec::with_capacity(count + 1);
    for gap in 0..=count {
        let (start, shift) = match gap {
            0 => (0, 0),
            g => (extents[g - 1].new.1, i64::from(extents[g - 1].new.1) - i64::from(extents[g - 1].old.1)),
        };
        let end = if gap == count { total + 1 } else { extents[gap].new.0 };
        let upto = if gap == count { total } else { end - 1 };
        // The same gap on the old side must be as long, or the diff is not of this file.
        if gap < count && i64::from(extents[gap].old.0) - 1 - (i64::from(start) - shift) != i64::from(upto.saturating_sub(start)) {
            return as_is();
        }
        hidden.push(upto.saturating_sub(start));
        delta.push(shift);
    }
    if hidden[count] > 0 && extents[count - 1].new.1 > total {
        return as_is();
    }
    let line_at = |new_no: u32, gap: usize| DiffLine {
        kind: LineKind::Context,
        old_no: Some((i64::from(new_no) - delta[gap]) as u32),
        new_no: Some(new_no),
        text: file.get(new_no as usize - 1).copied().unwrap_or("").to_owned(),
        no_newline: false,
    };
    let share = |gap: usize| reveals.get(gap).copied().unwrap_or_default();

    let mut out = Revealed::default();
    let mut current: Option<(Hunk, Option<Gap>)> = None;
    for (h, hunk) in diff.hunks.iter().enumerate() {
        // The gap above this hunk: when all of it is shown the hunk joins the one before.
        let (above, below) = (share(h).above, if h == 0 { 0 } else { share(h).below });
        let joined = h > 0 && above + below >= hidden[h];
        let ext = &extents[h];
        let mut head: Vec<DiffLine> = Vec::new();
        if !joined {
            let shown = above.min(hidden[h]);
            head.extend(((ext.new.0 - shown)..ext.new.0).map(|n| line_at(n, h)));
        }
        if joined {
            let (done, _) = current.as_mut().expect("a hunk before it");
            let from = extents[h - 1].new.1 + 1;
            let lines = (from..ext.new.0).map(|n| line_at(n, h));
            done.lines.extend(lines);
            done.lines.extend(hunk.lines.iter().cloned());
        } else {
            if let Some((done, gap)) = current.take() {
                out.above.push(gap);
                out.diff.hunks.push(done);
            }
            let left = hidden[h].saturating_sub(above.min(hidden[h]) + below.min(hidden[h]));
            let gap = (left > 0).then_some(Gap { index: h, left });
            let mut lines = head;
            lines.extend(hunk.lines.iter().cloned());
            current = Some((Hunk { header: hunk.header.clone(), old_start: hunk.old_start, new_start: hunk.new_start, lines }, gap));
        }
        // What is shown under this hunk from the gap after it, when that gap is not shown in full.
        let next = h + 1;
        let (next_above, next_below) = if next == count { (0, share(next).below) } else { (share(next).above, share(next).below) };
        if next == count || next_above + next_below < hidden[next] {
            let shown = next_below.min(hidden[next]);
            let (open, _) = current.as_mut().expect("this hunk");
            let from = ext.new.1 + 1;
            open.lines.extend((from..from + shown).map(|n| line_at(n, next)));
        }
    }
    if let Some((done, gap)) = current.take() {
        out.above.push(gap);
        out.diff.hunks.push(done);
    }
    let last = count;
    let left = hidden[last].saturating_sub(share(last).below.min(hidden[last]));
    out.below = (left > 0).then_some(Gap { index: last, left });
    for hunk in &mut out.diff.hunks {
        renumber(hunk);
    }
    out
}

/// Makes a hunk's `@@ -a,b +c,d @@ section` line say what is in it.
fn renumber(hunk: &mut Hunk) {
    let first = |side: fn(&DiffLine) -> Option<u32>, fallback: u32| hunk.lines.iter().find_map(side).unwrap_or(fallback);
    let (old_start, new_start) = (first(|l| l.old_no, hunk.old_start), first(|l| l.new_no, hunk.new_start));
    let old_count = hunk.lines.iter().filter(|l| l.old_no.is_some()).count();
    let new_count = hunk.lines.iter().filter(|l| l.new_no.is_some()).count();
    let section = hunk.header.strip_prefix("@@ ").and_then(|rest| rest.split_once(" @@")).map_or("", |(_, section)| section);
    hunk.header = format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@{section}");
    (hunk.old_start, hunk.new_start) = (old_start, new_start);
}

/// One row of a side-by-side view: indices into a hunk's `lines`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    pub left: Option<usize>,
    pub right: Option<usize>,
}

/// Lays a hunk out in two columns. Context lines appear on both sides. A run of removals followed
/// by a run of additions is paired line by line, so a changed line sits across from its old self;
/// whatever is left over stands alone against an empty cell.
pub fn pairs(hunk: &Hunk) -> Vec<Pair> {
    let lines = &hunk.lines;
    let mut rows = Vec::with_capacity(lines.len());
    let mut at = 0;
    while at < lines.len() {
        if lines[at].kind == LineKind::Context {
            rows.push(Pair { left: Some(at), right: Some(at) });
            at += 1;
            continue;
        }
        let removed_end = at + lines[at..].iter().take_while(|l| l.kind == LineKind::Removed).count();
        let added_end = removed_end + lines[removed_end..].iter().take_while(|l| l.kind == LineKind::Added).count();
        let (removed, added) = (at..removed_end, removed_end..added_end);
        for i in 0..removed.len().max(added.len()) {
            rows.push(Pair {
                left: (i < removed.len()).then(|| removed.start + i),
                right: (i < added.len()).then(|| added.start + i),
            });
        }
        at = added_end;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@ fn main
 keep one
-old two
+new two
+new three
 keep four
@@ -20,2 +21,2 @@
 tail
-last
\\ No newline at end of file
+last!
";

    #[test]
    fn parses_hunks_with_line_numbers_on_both_sides() {
        let diff = parse(SAMPLE);
        assert_eq!(diff.hunks.len(), 2);
        assert!(!diff.binary && !diff.truncated);

        let first = &diff.hunks[0];
        assert_eq!(first.header, "@@ -1,4 +1,5 @@ fn main");
        assert_eq!((first.old_start, first.new_start), (1, 1));
        let numbers: Vec<(Option<u32>, Option<u32>)> = first.lines.iter().map(|l| (l.old_no, l.new_no)).collect();
        assert_eq!(
            numbers,
            [(Some(1), Some(1)), (Some(2), None), (None, Some(2)), (None, Some(3)), (Some(3), Some(4))]
        );
        assert_eq!(first.lines[1].kind, LineKind::Removed);
        assert_eq!(first.lines[1].text, "old two");

        let second = &diff.hunks[1];
        assert_eq!((second.old_start, second.new_start), (20, 21));
        assert_eq!(second.lines[1].old_no, Some(21));
        assert!(second.lines[1].no_newline, "the marker belongs to the line before it");
        assert!(!second.lines[2].no_newline);
    }

    #[test]
    fn a_binary_file_has_no_hunks() {
        let diff = parse("diff --git a/x.png b/x.png\nindex 1..2 100644\nBinary files a/x.png and b/x.png differ\n");
        assert!(diff.binary);
        assert!(diff.hunks.is_empty());
    }

    #[test]
    fn an_empty_diff_is_empty() {
        assert_eq!(parse(""), FileDiff::default());
    }

    #[test]
    fn a_blank_context_line_with_its_space_trimmed_stays_in_the_hunk() {
        let diff = parse("@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n");
        let kinds: Vec<LineKind> = diff.hunks[0].lines.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, [LineKind::Context, LineKind::Context, LineKind::Removed, LineKind::Added]);
        assert_eq!(diff.hunks[0].lines[1].text, "");
    }

    #[test]
    fn a_new_file_starts_at_line_one_with_an_empty_old_side() {
        let diff = parse("@@ -0,0 +1,2 @@\n+a\n+b\n");
        let lines = &diff.hunks[0].lines;
        assert_eq!((lines[0].old_no, lines[0].new_no), (None, Some(1)));
        assert_eq!((lines[1].old_no, lines[1].new_no), (None, Some(2)));
    }

    #[test]
    fn a_huge_diff_is_cut_and_flagged() {
        let mut text = String::from("@@ -0,0 +1,25000 @@\n");
        for i in 0..25_000 {
            text.push_str(&format!("+line {i}\n"));
        }
        let diff = parse(&text);
        assert!(diff.truncated);
        assert_eq!(diff.line_count(), MAX_LINES);
    }

    fn hunk(kinds: &str) -> Hunk {
        let lines = kinds
            .chars()
            .map(|c| DiffLine {
                kind: match c {
                    '-' => LineKind::Removed,
                    '+' => LineKind::Added,
                    _ => LineKind::Context,
                },
                old_no: None,
                new_no: None,
                text: String::new(),
                no_newline: false,
            })
            .collect();
        Hunk { header: String::new(), old_start: 1, new_start: 1, lines }
    }

    fn rows(kinds: &str) -> Vec<(Option<usize>, Option<usize>)> {
        pairs(&hunk(kinds)).iter().map(|p| (p.left, p.right)).collect()
    }

    #[test]
    fn context_appears_on_both_sides() {
        assert_eq!(rows(" "), [(Some(0), Some(0))]);
    }

    #[test]
    fn a_changed_line_sits_across_from_its_old_self() {
        assert_eq!(rows("-+"), [(Some(0), Some(1))]);
    }

    #[test]
    fn extra_removals_and_additions_stand_alone() {
        assert_eq!(rows("--+"), [(Some(0), Some(2)), (Some(1), None)]);
        assert_eq!(rows("-++"), [(Some(0), Some(1)), (None, Some(2))]);
        assert_eq!(rows("++"), [(None, Some(0)), (None, Some(1))]);
        assert_eq!(rows("--"), [(Some(0), None), (Some(1), None)]);
    }

    #[test]
    fn blocks_are_paired_separately() {
        assert_eq!(
            rows("-+ -+"),
            [(Some(0), Some(1)), (Some(2), Some(2)), (Some(3), Some(4))]
        );
    }

    #[test]
    fn every_line_appears_exactly_once_per_side() {
        let h = hunk(" --+++ -  ++-");
        let rows = pairs(&h);
        let lefts: Vec<usize> = rows.iter().filter_map(|p| p.left).collect();
        let rights: Vec<usize> = rows.iter().filter_map(|p| p.right).collect();
        let expect_left: Vec<usize> =
            h.lines.iter().enumerate().filter(|(_, l)| l.kind != LineKind::Added).map(|(i, _)| i).collect();
        let expect_right: Vec<usize> =
            h.lines.iter().enumerate().filter(|(_, l)| l.kind != LineKind::Removed).map(|(i, _)| i).collect();
        assert_eq!(lefts, expect_left);
        assert_eq!(rights, expect_right);
    }
}

#[cfg(test)]
mod reveal_tests {
    use super::*;

    /// A file of 30 lines "l1".."l30" with line 5 changed and line 25 removed.
    fn file() -> (String, FileDiff) {
        // The new file has 29 lines: l25 is gone, so lines after it move up by one.
        let mut lines = Vec::new();
        for n in 1..=30 {
            if n == 25 {
                continue;
            }
            lines.push(if n == 5 { "CHANGED".to_owned() } else { format!("l{n}") });
        }
        let text = lines.join("\n") + "\n";
        let diff = parse(
            "@@ -2,7 +2,7 @@ fn a\n l2\n l3\n l4\n-l5\n+CHANGED\n l6\n l7\n l8\n@@ -22,7 +22,6 @@ fn b\n l22\n l23\n l24\n-l25\n l26\n l27\n l28\n",
        );
        (text, diff)
    }

    fn numbers(hunk: &Hunk) -> Vec<(Option<u32>, Option<u32>)> {
        hunk.lines.iter().map(|l| (l.old_no, l.new_no)).collect()
    }

    #[test]
    fn nothing_revealed_gives_the_diff_back_and_says_what_is_hidden() {
        let (text, diff) = file();
        let shown = reveal(&diff, &text, &[]);
        assert_eq!(shown.diff.hunks.len(), 2);
        assert_eq!(shown.diff.hunks[0].lines, diff.hunks[0].lines);
        // Line 1 above the first hunk, lines 9..21 between (13 of them), and 2 lines (the last two of 29) after.
        assert_eq!(shown.above, vec![Some(Gap { index: 0, left: 1 }), Some(Gap { index: 1, left: 13 })]);
        assert_eq!(shown.below, Some(Gap { index: 2, left: 2 }));
    }

    #[test]
    fn showing_lines_above_a_hunk_adds_the_lines_just_over_it_with_both_numbers() {
        let (text, diff) = file();
        let shown = reveal(&diff, &text, &[Reveal::default(), Reveal { above: 5, below: 0 }]);
        let second = &shown.diff.hunks[1];
        // 17..21 are the five lines over l22; the old numbers are the same there (nothing was added or removed before).
        assert_eq!(numbers(second)[0], (Some(17), Some(17)));
        assert_eq!(second.lines.len(), 7 + 5);
        assert_eq!(second.lines[0].text, "l17");
        assert!(second.header.starts_with("@@ -17,12 +17,11 @@"), "{}", second.header);
        assert!(second.header.ends_with("fn b"), "the section stays: {}", second.header);
        assert_eq!(shown.above[1], Some(Gap { index: 1, left: 8 }));
    }

    #[test]
    fn showing_lines_below_a_hunk_continues_it_and_numbers_both_sides() {
        let (text, diff) = file();
        let shown = reveal(&diff, &text, &[Reveal::default(), Reveal { above: 0, below: 3 }]);
        let first = &shown.diff.hunks[0];
        assert_eq!(first.lines.len(), 8 + 3);
        let last = first.lines.last().unwrap();
        assert_eq!((last.old_no, last.new_no, last.text.as_str()), (Some(11), Some(11), "l11"));
        assert_eq!(shown.above[1], Some(Gap { index: 1, left: 10 }));
    }

    #[test]
    fn the_numbers_after_a_removal_differ_on_the_two_sides() {
        let (text, diff) = file();
        let some = reveal(&diff, &text, &[Reveal::default(), Reveal::default(), Reveal { above: 0, below: 1 }]);
        let last = some.diff.hunks[1].lines.last().unwrap();
        // Under the hunk the next line is l29: it is line 28 now, after the removal, and was line 29.
        assert_eq!((last.old_no, last.new_no, last.text.as_str()), (Some(29), Some(28), "l29"));
        assert_eq!(some.below, Some(Gap { index: 2, left: 1 }));
        let shown = reveal(&diff, &text, &[Reveal::default(), Reveal::default(), Reveal { above: 0, below: 2 }]);
        let last = shown.diff.hunks[1].lines.last().unwrap();
        assert_eq!((last.old_no, last.new_no, last.text.as_str()), (Some(30), Some(29), "l30"));
        assert_eq!(shown.below, None, "all of what was left is shown");
    }

    #[test]
    fn a_gap_shown_in_full_joins_the_hunks_around_it() {
        let (text, diff) = file();
        let shown = reveal(&diff, &text, &[Reveal::default(), Reveal { above: 8, below: 5 }]);
        assert_eq!(shown.diff.hunks.len(), 1);
        let all = numbers(&shown.diff.hunks[0]);
        assert_eq!(all.len(), 8 + 13 + 7);
        // Every number goes up one by one on the new side.
        let new: Vec<u32> = all.iter().filter_map(|(_, n)| *n).collect();
        assert!(new.windows(2).all(|w| w[1] == w[0] + 1), "{new:?}");
        assert_eq!(shown.above, vec![Some(Gap { index: 0, left: 1 })]);
    }

    #[test]
    fn asking_for_more_than_is_hidden_shows_what_is_there() {
        let (text, diff) = file();
        let shown = reveal(&diff, &text, &[Reveal { above: 99, below: 0 }, Reveal { above: 99, below: 99 }, Reveal { above: 0, below: 99 }]);
        assert_eq!(shown.diff.hunks.len(), 1);
        assert_eq!(shown.diff.hunks[0].lines.first().unwrap().new_no, Some(1));
        assert_eq!(shown.diff.hunks[0].lines.last().unwrap().new_no, Some(29));
        assert_eq!((shown.above, shown.below), (vec![None], None));
    }

    #[test]
    fn a_file_that_is_not_what_the_diff_is_of_changes_nothing() {
        let (_, diff) = file();
        let shown = reveal(&diff, "just\none\nline\n", &[Reveal { above: 5, below: 5 }]);
        assert_eq!(shown.diff, diff);
        let cut = FileDiff { truncated: true, ..diff.clone() };
        assert_eq!(reveal(&cut, "x\n", &[]).diff, cut);
    }
}
