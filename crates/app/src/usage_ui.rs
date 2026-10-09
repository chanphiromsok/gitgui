//! Where a word is used. Picked in the code (a double-click, then Find usages in the right-click menu, or Cmd/Ctrl-click),
//! the answer is a list in the file pane's left column, where the changed files were: the lines that declare the word
//! first, then the files that use it, each line a click away from the whole file, read-only, as it was in that commit.

use std::ops::Range;
use std::path::Path;

use gitgui_core::{DiffLine, FileDiff, GitCli, Hunk, LineKind, MAX_MATCHES, Usages, occurrences};
use gpui::{AnyElement, Context, FontWeight, ListOffset, SharedString, StyledText, div, prelude::*, px, rgb, uniform_list};

use crate::changes::WORKTREE;
use crate::icons;
use crate::menu::Notice;
use crate::syntax;
use crate::theme::t;
use crate::ui::{self, MONO, ghost};
use crate::workspace::{FileState, Phase, Workspace};

const ROW_H: f32 = 22.;
/// Biggest file shown whole here: bigger ones are not read, and the person is told to open them in their editor.
const VIEW_MAX: usize = 2_000_000;

/// The text picked in the code of the open file, for copying and for searching: a word or a line of one row, on one
/// side of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The row of the open file (`FileState::rows`), and which side of a split row (1 left, 2 right; 0 for a unified one).
    pub row: usize,
    pub side: u8,
    /// Bytes of the line's code (not of the row's text, which has the numbers and the sign before it).
    pub range: Range<usize>,
}

/// A file read whole instead of as a change: what a usage points to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Viewing {
    pub path: String,
    /// The commit it is read from; `None` is the working folder.
    pub rev: Option<String>,
    /// The line to scroll to and mark.
    pub line: Option<u32>,
}

/// What the left column shows while a word's usages are asked for.
pub struct UsagesState {
    pub word: String,
    /// Any part of a word, any case.
    pub loose: bool,
    /// The commit searched; `None` is the working folder.
    pub rev: Option<String>,
    pub phase: Phase<Usages>,
    pub rows: Vec<UsageRow>,
    /// Which search this is, so a slow answer for an earlier one is ignored.
    pub ticket: u64,
}

/// One line of the results list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageRow {
    Heading(&'static str),
    /// A file's name, over its matches in the group, and how many there are.
    File { file: usize, count: usize },
    /// A match: the file's, and the match's place in it.
    Match { file: usize, at: usize },
}

/// The rows of the list: the files that declare the word, then the files that use it, each with its lines.
pub fn usage_rows(found: &Usages) -> Vec<UsageRow> {
    let mut rows = Vec::new();
    for (heading, declared) in [("Declared in", true), ("Used in", false)] {
        let mut titled = false;
        for (file, owner) in found.files.iter().enumerate() {
            let these: Vec<usize> = owner.matches.iter().enumerate().filter(|(_, m)| m.definition == declared).map(|(at, _)| at).collect();
            if these.is_empty() {
                continue;
            }
            if !titled {
                rows.push(UsageRow::Heading(heading));
                titled = true;
            }
            rows.push(UsageRow::File { file, count: these.len() });
            rows.extend(these.into_iter().map(|at| UsageRow::Match { file, at }));
        }
    }
    rows
}

/// Where `word` is in `text`, for coloring a preview: the whole name, or (loose) any part in any case.
fn word_ranges(text: &str, word: &str, loose: bool) -> Vec<Range<usize>> {
    if !loose {
        return occurrences(text, word);
    }
    let (lower, find) = (text.to_lowercase(), word.to_lowercase());
    // Lowercasing can change a text's length; then the offsets would not fit, so nothing is marked.
    if lower.len() != text.len() || find.is_empty() {
        return Vec::new();
    }
    lower.match_indices(&find).map(|(at, _)| at..at + find.len()).collect()
}

/// A file as the diff view takes it: one hunk of unchanged lines, numbered on the new side only.
fn whole_file(text: &str) -> FileDiff {
    let mut lines: Vec<DiffLine> = text
        .split('\n')
        .enumerate()
        .map(|(n, line)| DiffLine {
            kind: LineKind::Context,
            old_no: None,
            new_no: Some(n as u32 + 1),
            text: line.trim_end_matches('\r').to_owned(),
            no_newline: false,
        })
        .collect();
    // A file that ends with a newline has no line after it.
    if lines.last().is_some_and(|line| line.text.is_empty()) && lines.len() > 1 {
        lines.pop();
    }
    FileDiff { hunks: vec![Hunk { header: String::new(), old_start: 1, new_start: 1, lines }], binary: false, truncated: false }
}

impl FileState {
    /// A file opened to be read, not reviewed: from a usage.
    pub(crate) fn viewer(viewing: Viewing) -> Self {
        let mut file = Self::new(usize::MAX);
        file.single_column = true;
        file.viewing = Some(viewing);
        file
    }
}

impl Workspace {
    /// The commit being reviewed, as `git grep` takes it: its id; `None` for the uncommitted changes.
    fn reviewed_rev(&self) -> Option<Option<String>> {
        let commit = self.repo.as_ref()?.commit.as_ref()?;
        Some(Some(commit.id.clone()).filter(|id| id != WORKTREE))
    }

    /// Asks where `word` is used, as the files were in the commit being reviewed.
    pub fn find_usages(&mut self, word: String, cx: &mut Context<Self>) {
        if let Some(problem) = gitgui_core::word_problem(&word) {
            return self.say(Notice::warn(problem), cx);
        }
        let Some(rev) = self.reviewed_rev() else { return };
        self.usage_seq += 1;
        let ticket = self.usage_seq;
        let Some(repo) = self.repo.as_mut() else { return };
        repo.usages = Some(UsagesState { word, loose: false, rev, phase: Phase::Loading, rows: Vec::new(), ticket });
        // The list is the left column; a file read from an earlier word is not this word's.
        if repo.file.as_ref().is_some_and(|file| file.viewing.is_some()) {
            repo.file = None;
        }
        cx.notify();
        self.run_usages(cx);
    }

    /// Reads the usages the column is asking for in the background.
    fn run_usages(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref() else { return };
        let Some(state) = repo.usages.as_ref() else { return };
        let (root, word, loose, rev, ticket) = (repo.project.path.clone(), state.word.clone(), state.loose, state.rev.clone(), state.ticket);
        let generation = repo.generation;
        self.spawn_load(
            cx,
            move || GitCli::new(&root).usages(rev.as_deref(), &word, loose),
            move |this, result, cx| {
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.generation == generation) else { return };
                let Some(state) = repo.usages.as_mut().filter(|state| state.ticket == ticket) else { return };
                match result {
                    Ok(found) => {
                        state.rows = usage_rows(&found);
                        state.phase = Phase::Ready(found);
                    }
                    Err(err) => state.phase = Phase::Failed(err.to_string().into()),
                }
                cx.notify();
            },
        );
    }

    /// Any part of a word, any case; or the whole name as written.
    pub fn set_usage_loose(&mut self, loose: bool, cx: &mut Context<Self>) {
        self.usage_seq += 1;
        let ticket = self.usage_seq;
        let Some(state) = self.repo.as_mut().and_then(|repo| repo.usages.as_mut()) else { return };
        if state.loose == loose {
            return;
        }
        (state.loose, state.ticket, state.phase) = (loose, ticket, Phase::Loading);
        state.rows.clear();
        cx.notify();
        self.run_usages(cx);
    }

    /// Back to the changed files.
    pub fn close_usages(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        if repo.usages.take().is_none() {
            return;
        }
        // The file read from the list goes with it.
        if repo.file.as_ref().is_some_and(|file| file.viewing.is_some()) {
            self.cancel_file_load();
            if let Some(repo) = self.repo.as_mut() {
                repo.file = None;
            }
        }
        cx.notify();
    }

    /// Finds the commits that added or removed the word: it goes in the search box as `code:word`, and the graph
    /// (shown again if it was out of the way) dims what does not have it.
    pub fn find_commits_changing(&mut self, word: String, cx: &mut Context<Self>) {
        if let Some(problem) = gitgui_core::word_problem(&word) {
            return self.say(Notice::warn(problem), cx);
        }
        let text = format!("code:{word}");
        self.search_input.update(cx, |input, cx| input.replace_text(&text, cx));
        self.set_search(text, cx);
        self.graph_hidden = false;
        if let Some(repo) = self.repo.as_mut() {
            repo.expanded = false;
        }
        self.submit_search(cx);
        cx.notify();
    }

    /// Reads the whole of `path` as it was in the commit searched and shows it under the toolbar, scrolled to `line`.
    pub fn open_usage(&mut self, path: String, line: u32, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        let Some(state) = repo.usages.as_ref() else { return };
        let (word, rev, root) = (state.word.clone(), state.rev.clone(), repo.project.path.clone());
        let viewing = Viewing { path: path.clone(), rev: rev.clone(), line: Some(line) };
        let mut file = FileState::viewer(viewing.clone());
        file.word_mark = Some(word);
        repo.file = Some(file);
        let generation = repo.generation;
        self.cancel_file_load();
        cx.notify();
        self.spawn_load(
            cx,
            move || -> Result<(FileDiff, syntax::FileColors), String> {
                let bytes = match &rev {
                    Some(rev) => match GitCli::new(&root).file_bytes_at(rev, &path) {
                        Ok(Some(bytes)) => bytes,
                        Ok(None) => return Err(format!("{path} is not in that commit.")),
                        Err(err) => return Err(err.to_string()),
                    },
                    None => std::fs::read(Path::new(&root).join(&path)).map_err(|err| format!("Could not read {path}: {err}"))?,
                };
                if bytes.len() > VIEW_MAX {
                    return Err(format!("{path} is {} MB: too big to show here. Open it in your editor.", bytes.len() / 1_000_000));
                }
                if bytes.contains(&0) {
                    return Err(format!("{path} is not a text file."));
                }
                let text = String::from_utf8_lossy(&bytes).into_owned();
                let diff = whole_file(&text);
                let colors = if syntax::language_of(&path).is_some() { syntax::for_diff(&path, None, Some(&text), &diff) } else { syntax::FileColors::default() };
                Ok((diff, colors))
            },
            move |this, result, cx| {
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.generation == generation) else { return };
                let mode = repo.mode;
                let Some(file) = repo.file.as_mut().filter(|file| file.viewing.as_ref() == Some(&viewing)) else { return };
                match result {
                    Ok((diff, colors)) => {
                        file.raw = diff.clone();
                        file.diff = diff;
                        file.colors = colors;
                        file.phase = Phase::Ready(());
                        file.rebuild(mode, false);
                        // A few lines of what comes before, so the line is not at the very top.
                        let at = viewing.line.map_or(0, |line| (line as usize).saturating_sub(5));
                        file.list.scroll_to(ListOffset { item_ix: at, offset_in_item: px(0.) });
                    }
                    Err(message) => file.phase = Phase::Failed(message.into()),
                }
                cx.notify();
            },
        );
    }

    /// A development aid: opens the `n`th match of the list (from 0), as a click on it would.
    pub(crate) fn script_open_usage(&mut self, n: usize, cx: &mut Context<Self>) {
        let target = self.repo.as_ref().and_then(|repo| repo.usages.as_ref()).and_then(|state| match &state.phase {
            Phase::Ready(found) => state
                .rows
                .iter()
                .filter_map(|row| match row {
                    UsageRow::Match { file, at, .. } => found.files.get(*file).and_then(|f| f.matches.get(*at).map(|m| (f.path.clone(), m.line))),
                    _ => None,
                })
                .nth(n),
            _ => None,
        });
        match target {
            Some((path, line)) => self.open_usage(path, line, cx),
            None => eprintln!("gitgui: no usage number {n}"),
        }
    }

    /// A development aid: picks the first place of `word` in the open file, as a double-click on it would.
    pub(crate) fn script_pick(&mut self, word: &str, cx: &mut Context<Self>) {
        let found = self.repo.as_ref().and_then(|repo| repo.file.as_ref()).and_then(|file| {
            (0..file.rows.len()).find_map(|row| {
                [0u8, 1, 2].into_iter().find_map(|side| {
                    let code = file.code_of(row, side)?;
                    let at = code.find(word)?;
                    Some((row, side, at))
                })
            })
        });
        let Some((row, side, at)) = found else { return eprintln!("gitgui: {word:?} is not in the open file") };
        let event = gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: gpui::point(px(0.), px(0.)),
            modifiers: gpui::Modifiers::default(),
            click_count: 2,
            first_mouse: false,
        };
        self.press_at(row, side, Some(at), &event, cx);
    }

    /// The index in the commit's changed files of the file being read, if the commit changed it.
    pub(crate) fn viewed_change(&self) -> Option<usize> {
        let file = self.repo.as_ref()?.file.as_ref()?;
        let viewing = file.viewing.as_ref()?;
        self.commit_view()?.files.iter().position(|change| change.path == viewing.path)
    }

    /// The left column while usages are asked for: the word, how many, and the list.
    pub(crate) fn render_usages_column(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let Some(state) = repo.usages.as_ref() else { return div().into_any_element() };

        let at = match &state.rev {
            Some(rev) => format!("in {}", rev.chars().take(7).collect::<String>()),
            None => "in your working folder".to_owned(),
        };
        let summary = match &state.phase {
            Phase::Loading => "Searching…".to_owned(),
            Phase::Failed(_) => String::new(),
            Phase::Ready(found) => match (found.matches, found.files.len()) {
                (0, _) => format!("Nothing {at}"),
                (m, f) => format!("{m}{} match{} in {f} file{} {at}", if found.truncated { "+" } else { "" }, if m == 1 { "" } else { "es" }, if f == 1 { "" } else { "s" }),
            },
        };
        let loose = state.loose;
        let head = div()
            .w_full()
            .flex_none()
            .px_2()
            .py_1p5()
            .flex()
            .flex_col()
            .gap_1()
            .border_b_1()
            .border_color(rgb(t().border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(ghost("usages-back", "‹ Files").debug_selector(|| "usages-back".to_owned()).on_click(cx.listener(|this, _, _, cx| this.close_usages(cx))))
                    .child(div().text_xs().text_color(rgb(t().muted)).child("Usages")),
            )
            .child(
                div()
                    .px_1()
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .font_family(MONO)
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(t().text_strong))
                    .child(SharedString::from(state.word.clone())),
            )
            .child(div().px_1().text_xs().text_color(rgb(t().muted)).child(SharedString::from(summary)))
            .child(
                div().flex().child(
                    ui::toggle("usages-loose", "Any part of a word, any case", loose)
                        .debug_selector(|| "usages-loose".to_owned())
                        .on_click(cx.listener(move |this, _, _, cx| this.set_usage_loose(!loose, cx))),
                ),
            );

        let body: AnyElement = match &state.phase {
            Phase::Loading => div().p_3().text_xs().text_color(rgb(t().muted)).child("Searching…").into_any_element(),
            Phase::Failed(message) => div().p_3().text_xs().text_color(rgb(t().removed)).child(message.clone()).into_any_element(),
            Phase::Ready(found) if found.matches == 0 => {
                let word = state.word.clone();
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(t().muted))
                    .child(SharedString::from(format!("No other uses of {} {at}.", state.word)))
                    .child(
                        ghost("usages-history", "Find commits that changed it")
                            .debug_selector(|| "usages-history".to_owned())
                            .on_click(cx.listener(move |this, _, _, cx| this.find_commits_changing(word.clone(), cx))),
                    )
                    .into_any_element()
            }
            Phase::Ready(found) => {
                let truncated = found.truncated;
                let rows = state.rows.len();
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        uniform_list(
                            "usage-rows",
                            rows,
                            cx.processor(move |this, range: Range<usize>, _window, cx| {
                                let Some(state) = this.repo.as_ref().and_then(|repo| repo.usages.as_ref()) else { return Vec::new() };
                                let Phase::Ready(found) = &state.phase else { return Vec::new() };
                                let opened = this.repo.as_ref().and_then(|repo| repo.file.as_ref()).and_then(|file| file.viewing.as_ref()).map(|v| (v.path.clone(), v.line));
                                range.filter_map(|ix| usage_row(ix, state.rows.get(ix).copied()?, found, state.loose, opened.as_ref(), cx)).collect::<Vec<_>>()
                            }),
                        )
                        .flex_1(),
                    )
                    .when(truncated, |col| {
                        col.child(
                            div()
                                .flex_none()
                                .px_3()
                                .py_1p5()
                                .border_t_1()
                                .border_color(rgb(t().border))
                                .text_xs()
                                .text_color(rgb(t().warning))
                                .child(format!("Showing the first {MAX_MATCHES}. Try a longer word.")),
                        )
                    })
                    .into_any_element()
            }
        };

        div()
            .w(px(self.files_width))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(t().panel))
            .child(head)
            .child(body)
            .into_any_element()
    }
}

/// A row of the results: a heading, a file, or a line with the word colored.
fn usage_row(ix: usize, row: UsageRow, found: &Usages, loose: bool, opened: Option<&(String, Option<u32>)>, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    Some(match row {
        UsageRow::Heading(title) => div()
            .id(("usage-heading", ix))
            .w_full()
            .h(px(ROW_H))
            .px_3()
            .flex()
            .items_center()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(t().muted))
            .child(title)
            .into_any_element(),
        UsageRow::File { file: f, count } => {
            let file = found.files.get(f)?;
            let (folder, name) = file.path.rsplit_once('/').map_or(("", file.path.as_str()), |(folder, name)| (folder, name));
            div()
                .id(("usage-file", ix))
                .w_full()
                .h(px(ROW_H))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .child(ui::file_icon(icons::file(name)))
                .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(rgb(t().text)).child(SharedString::from(name.to_owned())))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .line_clamp(1)
                        .text_ellipsis()
                        .text_xs()
                        .text_color(rgb(t().muted))
                        .child(SharedString::from(folder.to_owned())),
                )
                .child(div().flex_none().text_xs().text_color(rgb(t().muted)).child(count.to_string()))
                .into_any_element()
        }
        UsageRow::Match { file, at } => {
            let owner = found.files.get(file)?;
            let m = owner.matches.get(at)?;
            let path = owner.path.clone();
            let line = m.line;
            let is_open = opened.is_some_and(|(open, at_line)| *open == path && *at_line == Some(line));
            // The word is what was asked for: the preview starts a little before it, so it is not off the edge of a narrow column.
            let (text, _) = lead_in(&m.text, word_ranges(&m.text, &found.word, loose).first().map(|r| r.start));
            let marks: Vec<(Range<usize>, gpui::HighlightStyle)> = word_ranges(&text, &found.word, loose)
                .into_iter()
                .map(|range| (range, gpui::HighlightStyle { color: Some(rgb(t().accent).into()), font_weight: Some(FontWeight::BOLD), ..Default::default() }))
                .collect();
            div()
                .id(("usage", ix))
                .debug_selector(move || format!("usage-{ix}"))
                .w_full()
                .h(px(ROW_H))
                .pl_3()
                .pr_2()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .when(is_open, |row| row.bg(rgb(t().selected)))
                .hover(move |style| style.bg(rgb(if is_open { t().selected } else { t().hover })))
                .on_click(cx.listener(move |this, _, _, cx| this.open_usage(path.clone(), line, cx)))
                .child(div().flex_none().w(px(34.)).text_xs().font_family(MONO).text_color(rgb(t().line_number)).child(SharedString::from(line.to_string())))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .font_family(MONO)
                        .text_xs()
                        .text_color(rgb(t().editor_fg))
                        .child(StyledText::new(text).with_highlights(marks)),
                )
                .into_any_element()
        }
    })
}

/// How far before the word a preview starts, in characters.
const LEAD: usize = 12;

/// `text` from a little before byte `word_at` (with an ellipsis where it was cut), so the word is in view; and how
/// many bytes that moved it.
fn lead_in(text: &str, word_at: Option<usize>) -> (String, usize) {
    let Some(at) = word_at.filter(|at| text[..*at].chars().count() > LEAD + 2) else { return (text.to_owned(), 0) };
    let start = text[..at].char_indices().rev().nth(LEAD - 1).map_or(0, |(i, _)| i);
    (format!("…{}", &text[start..]), start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitgui_core::{UsageFile, UsageMatch};

    fn m(line: u32, definition: bool) -> UsageMatch {
        UsageMatch { line, text: format!("line {line}"), definition }
    }

    #[test]
    fn the_files_that_declare_the_word_come_first_and_the_files_that_use_it_follow_each_with_its_lines() {
        let found = Usages {
            word: "x".into(),
            files: vec![
                UsageFile { path: "a.ts".into(), matches: vec![m(1, true), m(5, false)] },
                UsageFile { path: "b.ts".into(), matches: vec![m(2, false)] },
                UsageFile { path: "c.ts".into(), matches: vec![m(9, true)] },
            ],
            matches: 4,
            truncated: false,
        };
        assert_eq!(
            usage_rows(&found),
            [
                UsageRow::Heading("Declared in"),
                UsageRow::File { file: 0, count: 1 },
                UsageRow::Match { file: 0, at: 0 },
                UsageRow::File { file: 2, count: 1 },
                UsageRow::Match { file: 2, at: 0 },
                UsageRow::Heading("Used in"),
                UsageRow::File { file: 0, count: 1 },
                UsageRow::Match { file: 0, at: 1 },
                UsageRow::File { file: 1, count: 1 },
                UsageRow::Match { file: 1, at: 0 },
            ],
            "a file can be in both groups; one with only a declaration is in the first only"
        );
        assert!(usage_rows(&Usages::default()).is_empty());
        let none_declared = Usages { files: vec![UsageFile { path: "b.ts".into(), matches: vec![m(2, false)] }], ..Usages::default() };
        assert_eq!(usage_rows(&none_declared)[0], UsageRow::Heading("Used in"), "no heading for a group with nothing in it");
    }

    #[test]
    fn a_long_preview_starts_a_little_before_the_word_so_it_is_in_view() {
        let text = "export function refetchManifestQueries({ manifestId }) {";
        let at = text.find("refetch").unwrap();
        let (shown, moved) = lead_in(text, Some(at));
        assert!(shown.starts_with('…') && shown.contains("refetchManifestQueries") && moved > 0, "{shown}");
        assert!(shown.find("refetch").unwrap() < 20);
        // A word already near the start, or none, leaves the text as it is.
        assert_eq!(lead_in("import { x }", Some(9)), ("import { x }".to_owned(), 0));
        assert_eq!(lead_in("anything", None), ("anything".to_owned(), 0));
    }

    #[test]
    fn a_file_is_shown_whole_as_unchanged_lines_numbered_on_one_side() {
        let diff = whole_file("one\ntwo\r\nthree\n");
        let lines = &diff.hunks[0].lines;
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["one", "two", "three"]);
        assert!(lines.iter().all(|l| l.kind == LineKind::Context && l.old_no.is_none()));
        assert_eq!(lines.iter().map(|l| l.new_no.unwrap()).collect::<Vec<_>>(), [1, 2, 3]);
        // Without a last newline the last line is kept, and an empty file has the one empty line git would count.
        assert_eq!(whole_file("a\nb").hunks[0].lines.len(), 2);
        assert_eq!(whole_file("").hunks[0].lines.len(), 1);
    }

    #[test]
    fn a_preview_marks_the_word_whole_or_loose() {
        assert_eq!(word_ranges("load loadAll Load", "load", false), vec![0..4]);
        assert_eq!(word_ranges("load loadAll Load", "load", true), [0..4, 5..9, 13..17]);
        assert!(word_ranges("anything", "", true).is_empty());
    }
}
