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
