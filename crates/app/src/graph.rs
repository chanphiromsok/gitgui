//! The commit table: a lane graph, ref badges and the Description / Date / Author / Commit columns.
//!
//! Every line belongs to a branch line (a lineage) and keeps one color from the branch's tip down to
//! the commit it was branched from. That commit is marked, and names the branches that start there.
//! Selecting a commit brings its branch line forward and dims the rest.

use std::collections::{HashMap, HashSet};

use gitgui_core::{
    Commit, CommitKind, Evidence, Half, Label, LabelKind, LaneLayout, Lineage, MergeClue, Placed, RefKind, Row, Stroke,
    People, Person, WebRemote, subject_pr,
    commit_branches, commit_kind, commit_rank, conventional_prefix, descendants, group_by_parent, labels, lineage_names,
};
use gpui::{
    BorderStyle, Bounds, Context, FontWeight, MouseButton, PathBuilder, Pixels, Rgba, SharedString, StyledText, Window, canvas,
    div, point, prelude::*, px, quad, rgb, size,
};

use crate::avatars::Avatar;
use crate::icons;
use crate::layout::{Columns, DateStyle, short_date};
use gitgui_store::GraphFaces;
use crate::menu::MenuTarget;
use crate::ui::{self, MONO, line_color};
use crate::workspace::Workspace;
use crate::theme::t;

pub const ROW_H: f32 = 26.0;
/// How tightly the graph is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Density {
    /// Distance between lanes.
    pub lane_w: f32,
    pub dot_r: f32,
    /// How wide a commit's node is: a circle with its author's initials.
    pub node: f32,
    /// Thickness of a line; the selected line is a step thicker.
    pub line: f32,
    /// Height of a row.
    pub row_h: f32,
    /// The scale everything above was multiplied by (the Graph size setting).
    pub scale: f32,
}

impl Density {
    const ROOMY: Density = Density { lane_w: 20., dot_r: 4.5, line: 2., node: 17., row_h: ROW_H, scale: 1. };
    /// Narrow lanes and thin lines, so a busy history leaves room for the messages.
    const COMPACT: Density = Density { lane_w: 14., dot_r: 3.2, line: 1.5, node: 13., row_h: ROW_H, scale: 1. };

    /// `scale` is the Graph size setting as a factor: 1.0 is the default, 1.5 draws everything half as large again.
    pub fn of(compact: bool, scale: f32) -> Self {
        let base = if compact { Self::COMPACT } else { Self::ROOMY };
        Self {
            lane_w: base.lane_w * scale,
            dot_r: base.dot_r * scale,
            line: base.line * scale,
            node: base.node * scale,
            row_h: base.row_h * scale,
            scale,
        }
    }

    /// Where a lane's center is, from the graph's left edge.
    fn x(&self, lane: usize) -> f32 {
        lane.min(MAX_DRAWN_LANES) as f32 * self.lane_w + self.lane_w / 2.
    }
}
pub const MAX_DRAWN_LANES: usize = 14;
/// How much of a branch line's color remains when another line is in front.
const DIMMED: f32 = 0.22;
/// How much remains of a commit, and a line, that is not part of the current branch's history.
const OFF_BRANCH: f32 = 0.45;
/// How far each level of a group is pushed in, and the color of the guide beside it.
const INDENT: f32 = 18.;
/// A branch badge is cut short beyond this width; the whole name is in its right-click menu.
const BADGE_MAX_W: f32 = 240.;

/// How the dot of a row is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    Filled,
    /// The commit HEAD is on: a ring around the dot, so the current branch is easy to find.
    Current,
    /// The working tree, not a commit: a hollow ring.
    Uncommitted,
}

/// A line of text beside a commit about a merge: "squash-merged into release/1.0.0", "squash of branch feat/x".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub text: SharedString,
    /// A sentence for the overview panel, saying how it was found.
    pub detail: SharedString,
    /// A guess from messages, not a fact from history or content.
    pub probable: bool,
    /// A commit to jump to: where the squash landed, or the branch's tip.
    pub jump: Option<String>,
    /// Says a branch tip is merged (a soft green tag); otherwise it is the quiet "squash of" line.
    pub merged: bool,
}

/// One table row, ready to draw.
pub struct Entry {
    pub row: Row,
    pub dot: Dot,
    pub merge: bool,
    pub labels: Vec<Label>,
    pub summary: SharedString,
    /// The pull request this commit merged or squashed, and its page.
    pub pr: Option<(u32, SharedString)>,
    /// How much of the summary is a conventional-commit prefix (`fix(ui):`), drawn bold; 0 for none.
    pub prefix: usize,
    pub date: SharedString,
    pub author: SharedString,
    /// The person behind the author, however many identities they commit under: one picture, one
    /// set of initials and one color for all of them.
    pub person: Person,
    pub short_id: SharedString,
    /// The full commit id; `None` for the working-tree row.
    pub commit: Option<String>,
    /// Names of the branch lines that were branched from this commit.
    pub forks: Vec<String>,
    pub notes: Vec<Note>,
    /// How many groups deep this row is listed: 0 on its own, 1 under a pull request.
    pub depth: usize,
    pub kind: CommitKind,
    /// How many commits are listed under this one.
    pub group_size: usize,
    /// Those commits are hidden.
    pub collapsed: bool,
    /// The commit is not in the current branch's history: it is drawn softer.
    pub off_branch: bool,
    /// Branch lines through this row that have nothing in the current branch's history.
    pub off_lines: Vec<usize>,
    /// A search is on and did not find this commit.
    pub search_miss: bool,
}

/// Where the current branch stands against the branch it was cut from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Base {
    pub name: String,
    /// The commit it was branched from.
    pub fork: String,
    /// Commits on the current branch that the base does not have.
    pub ahead: usize,
    /// Commits on the base that the current branch does not have.
    pub behind: usize,
}

/// How the commits are arranged.
pub struct Options<'a> {
    /// Rows for the working tree: how many files changed (none when 0).
    pub changed: usize,
    /// List a pull request's commits under it.
    pub group: bool,
    /// Tip commit of a squash-merged branch → the commit that carries its changes.
    pub squashed: &'a HashMap<String, String>,
    /// Commits whose group is folded away.
    pub collapsed: &'a HashSet<String>,
    /// The repository's web home, to link pull requests to.
    pub web: Option<&'a WebRemote>,
    /// Who is who among the authors.
    pub people: Option<&'a People>,
}

pub struct Built {
    pub entries: Vec<Entry>,
    pub widest: usize,
    pub lineages: Vec<Lineage>,
    pub names: Vec<Option<String>>,
    pub base: Option<Base>,
}

/// `start` and every commit it comes from, among `commits`.
fn history<'a>(start: &'a str, by_id: &HashMap<&'a str, &'a Commit>) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut todo = vec![start];
    while let Some(id) = todo.pop() {
        let Some(commit) = by_id.get(id) else { continue };
        if seen.insert(commit.id.as_str()) {
            todo.extend(commit.parents.iter().map(String::as_str));
        }
    }
    seen
}

/// Arranges `commits` (newest first) as table rows, preceded by an "Uncommitted Changes" row when
/// files have changed.
pub fn build_entries(commits: &[Commit], options: &Options) -> Built {
    let changed = options.changed;
    let head = commits.iter().find(|c| c.refs.iter().any(|r| r.kind == RefKind::Head));
    let by_id: HashMap<&str, &Commit> = commits.iter().map(|c| (c.id.as_str(), c)).collect();
    let head_history = head.map(|head| history(&head.id, &by_id));

    // The order to show them in, and which are folded away.
    let placed: Vec<Placed> = if options.group {
        group_by_parent(commits, options.squashed)
    } else {
        (0..commits.len()).map(|index| Placed { index, depth: 0, under: None }).collect()
    };
    let mut shown: Vec<(Placed, usize)> = Vec::with_capacity(placed.len()); // (place, commits under it)
    let mut hidden: HashSet<&str> = HashSet::new();
    let mut at = 0;
    while at < placed.len() {
        let size = descendants(&placed, at).len();
        shown.push((placed[at], size));
        if size > 0 && options.collapsed.contains(&commits[placed[at].index].id) {
            let range = descendants(&placed, at);
            hidden.extend(range.clone().map(|i| commits[placed[i].index].id.as_str()));
            at = range.end;
        } else {
            at += 1;
        }
    }
    let ordered: Vec<&Commit> = shown.iter().map(|(p, _)| &commits[p.index]).collect();

    // Lay the lines out in that order. A parent that is folded away is not there to draw a line to.
    let mut layout = LaneLayout::new();
    let offset = usize::from(changed > 0);
    let mut rows: Vec<Row> = Vec::with_capacity(ordered.len() + offset);
    if changed > 0 {
        // The working tree hangs off the commit HEAD is on, like a commit not made yet.
        let parents: Vec<&str> = head.map(|c| c.id.as_str()).into_iter().collect();
        let branches = head.map(commit_branches).unwrap_or_default();
        rows.push(layout.push_branch("uncommitted", &parents, head.map_or(0, commit_rank), &branches));
    }
    for commit in &ordered {
        let parents: Vec<&str> =
            commit.parents.iter().map(String::as_str).filter(|p| !hidden.contains(p)).collect();
        rows.push(layout.push_branch(&commit.id, &parents, commit_rank(commit), &commit_branches(commit)));
    }
    let lineages = layout.lineages().to_vec();
    let names = lineage_names(&lineages, |row| match row.checked_sub(offset) {
        Some(i) => ordered.get(i).copied(),
        None => head,
    });

    let mut entries = Vec::with_capacity(rows.len());
    let mut rows = rows.into_iter();
    if changed > 0 {
        let row = rows.next().expect("a row for the working tree");
        entries.push(Entry {
            forks: fork_names(&row, &names),
            row,
            dot: Dot::Uncommitted,
            merge: false,
            labels: Vec::new(),
            summary: SharedString::from(format!("Uncommitted Changes ({changed})")),
            pr: None,
            prefix: 0,
            date: SharedString::default(),
            author: SharedString::from("*"),
            person: Person { name: String::new(), email: String::new(), avatar_email: String::new() },
            short_id: SharedString::from("*"),
            commit: None,
            notes: Vec::new(),
            depth: 0,
            kind: CommitKind::Commit,
            group_size: 0,
            collapsed: false,
            off_branch: false,
            off_lines: Vec::new(),
            search_miss: false,
        });
    }
    for ((commit, row), (placed, size)) in ordered.iter().zip(rows).zip(&shown) {
        let is_head = commit.refs.iter().any(|r| r.kind == RefKind::Head);
        entries.push(Entry {
            forks: fork_names(&row, &names),
            row,
            dot: if is_head { Dot::Current } else { Dot::Filled },
            merge: commit.is_merge(),
            labels: labels(&commit.refs),
            summary: SharedString::from(commit.summary.clone()),
            pr: subject_pr(&commit.summary)
                .and_then(|n| Some((n, SharedString::from(options.web?.pull_request(n))))),
            prefix: conventional_prefix(&commit.summary).unwrap_or(0),
            date: SharedString::from(commit.date.clone()),
            author: SharedString::from(commit.author.clone()),
            person: options.people.and_then(|p| p.of(&commit.email)).cloned().unwrap_or_else(|| Person {
                name: commit.author.clone(),
                email: commit.email.clone(),
                avatar_email: commit.email.clone(),
            }),
            short_id: SharedString::from(commit.short_id().to_owned()),
            commit: Some(commit.id.clone()),
            notes: Vec::new(),
            depth: placed.depth,
            kind: commit_kind(commit),
            group_size: *size,
            collapsed: *size > 0 && options.collapsed.contains(&commit.id),
            off_branch: head_history.as_ref().is_some_and(|seen| !seen.contains(commit.id.as_str())),
            off_lines: Vec::new(),
            search_miss: false,
        });
    }

    // A line with none of its commits in the current branch's history steps back with them.
    let mut on_lines = vec![false; lineages.len()];
    for entry in entries.iter().filter(|entry| !entry.off_branch) {
        on_lines[entry.row.lineage] = true;
    }
    for entry in &mut entries {
        let mut off: Vec<usize> = std::iter::once(entry.row.lineage)
            .chain(entry.row.strokes.iter().map(|stroke| stroke.lineage))
            .filter(|&line| !on_lines[line])
            .collect();
        off.sort_unstable();
        off.dedup();
        entry.off_lines = off;
    }

    let base = head.zip(head_history.as_ref()).and_then(|(head, head_history)| {
        let line = entries.iter().find(|entry| entry.commit.as_deref() == Some(head.id.as_str()))?.row.lineage;
        let base_line = lineages[line].base?;
        let commit_at = |row: usize| row.checked_sub(offset).and_then(|i| ordered.get(i).copied());
        let fork = commit_at(lineages[line].fork_row?)?;
        let base_tip = commit_at(lineages[base_line].tip_row)?;
        let base_history = history(&base_tip.id, &by_id);
        Some(Base {
            name: names[base_line].clone()?,
            fork: fork.id.clone(),
            ahead: head_history.difference(&base_history).count(),
            behind: base_history.difference(head_history).count(),
        })
    });

    let widest = entries.iter().map(|entry| entry.row.width).max().unwrap_or(1);
    Built { entries, widest, lineages, names, base }
}

/// The names of the branch lines that end at this row because they were branched from its commit.
fn fork_names(row: &Row, names: &[Option<String>]) -> Vec<String> {
    let mut found: Vec<String> = row.joins.iter().filter_map(|line| names.get(*line).cloned().flatten()).collect();
    found.sort();
    found.dedup();
    found
}

/// Puts what the merge scan found next to the commits it is about: on the branch's tip, "merged into
/// release/1.0.0", and on the commit that carries the branch's changes, "squash of branch feat/x".
pub fn apply_clues(entries: &mut [Entry], clues: &[MergeClue]) {
    for entry in entries.iter_mut() {
        entry.notes.clear();
    }
    for clue in clues {
        let short = |id: &str| id.chars().take(7).collect::<String>();
        let pr = clue.pr.map(|n| format!(" (#{n})")).unwrap_or_default();
        let (verb, how) = match clue.evidence {
            Evidence::Contained => ("merged into", "Its commits are already part of that branch's history."),
            Evidence::SamePatch => ("squash-merged into", "A commit there makes exactly the same changes as this whole branch."),
            Evidence::PullRequest => ("probably squash-merged into", "A commit there names the same pull request as this branch's commits."),
            Evidence::Messages => ("probably squash-merged into", "A commit there repeats this branch's commit messages."),
        };
        let landed = clue.commit.as_deref().map(|c| format!(" as {}", short(c))).unwrap_or_default();

        for entry in entries.iter_mut() {
            let tip_of = entry.labels.iter().any(|label| {
                matches!(label.kind, LabelKind::Branch | LabelKind::RemoteBranch) && label.name == clue.branch
            });
            if tip_of {
                entry.notes.push(Note {
                    text: format!("✓ {verb} {}{pr}", clue.into).into(),
                    detail: format!("{} was {verb} {}{landed}. {how}", clue.branch, clue.into).into(),
                    probable: clue.evidence.is_probable(),
                    jump: clue.commit.clone(),
                    merged: true,
                });
            }
            if clue.commit.is_some() && entry.commit == clue.commit {
                // The row already links the pull request; say its number only when the row does not.
                let linked = entry.pr.as_ref().is_some_and(|(n, _)| Some(*n) == clue.pr);
                let pr = if linked { String::new() } else { pr.clone() };
                entry.notes.push(Note {
                    text: format!("squash of branch {}{pr}", short_name(&clue.branch)).into(),
                    detail: format!("This one commit holds all the changes of the branch {} (a squash merge), and the branch was merged into {} through it. {how}", clue.branch, clue.into).into(),
                    probable: clue.evidence.is_probable(),
                    jump: None,
                    merged: false,
                });
            }
        }
    }
}

/// A branch name for a line of text: without the `origin/` in front, and cut at 32 characters.
fn short_name(name: &str) -> String {
    let name = name.strip_prefix("origin/").unwrap_or(name);
    if name.chars().count() <= 32 {
        return name.to_owned();
    }
    let head: String = name.chars().take(31).collect();
    format!("{head}…")
}

pub fn graph_width(widest_lanes: usize, density: Density) -> f32 {
    widest_lanes.min(MAX_DRAWN_LANES + 1) as f32 * density.lane_w + 8.
}

/// The column titles; `cols` says which of the columns after Description fit.
pub fn columns(graph_width: f32, cols: Columns) -> impl IntoElement {
    let cell = |text: &'static str| div().font_weight(FontWeight::SEMIBOLD).child(text);
    div()
        .h(px(28.))
        .flex_none()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .bg(rgb(t().panel))
        .border_b_1()
        .border_color(rgb(t().border))
        // The title is dropped when the drawing is too narrow for it, instead of wrapping letter by letter.
        .child(cell(if graph_width >= 56. { "Graph" } else { "" }).w(px(graph_width)).flex_none().overflow_hidden().whitespace_nowrap())
        .child(cell("Description").flex_1().min_w_0())
        .when(cols.date != DateStyle::Hidden, |row| row.child(cell("Date").w(px(cols.date_width()))))
        .when(cols.author, |row| row.child(cell("Author").w(px(Columns::AUTHOR_W))))
        .when(cols.commit, |row| row.child(cell("Commit").w(px(Columns::COMMIT_W))))
}

fn faded(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: color.a.min(alpha), ..color }
}

/// A branch, tag, remote branch or stash on a commit. Right-click for its menu.
fn badge(label: &Label, lineage: usize, cx: &mut Context<Workspace>) -> impl IntoElement + use<> {
    let (bg, fg) = match label.kind {
        LabelKind::Branch => (line_color(lineage), rgb(ui::text_on(t().lane(lineage)))),
        LabelKind::RemoteBranch => (rgb(t().element), rgb(t().text)),
        LabelKind::Tag => (rgb(0x6b5b1e), rgb(0xfff3c4)),
        LabelKind::Stash => (rgb(0x23a455), rgb(0x0b1f12)),
    };
    // A cloud says the branch is on a remote too (or only there), the way other clients mark it,
    // instead of a separate `origin` tag beside it.
    let on_remote = label.kind == LabelKind::RemoteBranch || !label.remotes.is_empty();
    let fg_color = u32::from(fg) >> 8;
    let name = div()
        .px_1p5()
        .flex()
        .items_center()
        .gap_1()
        .max_w(px(BADGE_MAX_W))
        .bg(bg)
        .text_color(fg)
        .when(label.head, |name| name.font_weight(FontWeight::BOLD))
        .when(on_remote, |name| {
            name.child(gpui::img(icons::remote(fg_color)).flex_none().size(px(11.)))
                .when(label.remotes.len() > 1, |name| name.child(format!("{}", label.remotes.len())))
        })
        .child(
            div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(label.name.clone())),
        );

    let target = MenuTarget::Label(label.clone());
    // A detached HEAD is not a branch; right-clicking it gives the commit's menu, so let the row have it.
    let own_menu = label.name != "HEAD";
    let tag = format!("badge-{}", label.name);
    div()
        .id(SharedString::from(tag.clone()))
        .debug_selector(move || tag)
        .flex()
        .flex_none()
        .items_center()
        .rounded_md()
        .overflow_hidden()
        .text_xs()
        .cursor_pointer()
        .when(label.head, |badge| badge.border_2().border_color(rgb(t().text_strong)))
        // The menu for this badge only; the row's own menu must not also open.
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                if own_menu {
                    cx.stop_propagation();
                    this.open_menu(event.position, target.clone(), cx);
                }
            }),
        )
        .child(name)
}

/// The summary, its conventional-commit prefix (`docs:`, `fix(ui):`) in bold so the kind of change
/// reads at a glance.
fn summary_text(entry: &Entry, gray: bool) -> StyledText {
    let text = StyledText::new(entry.summary.clone());
    if entry.prefix == 0 || entry.merge {
        return text;
    }
    let bold = gpui::HighlightStyle {
        font_weight: Some(FontWeight::BOLD),
        // The prefix is as gray as the rest of the line when another branch is the one in focus.
        color: Some(rgb(if gray { t().muted } else { t().text_strong }).into()),
        ..Default::default()
    };
    text.with_highlights([(0..entry.prefix, bold)])
}

/// What the merge scan found, kept quiet: "merged" is a soft green tag, "squash of" is plain dim text.
fn note_label(note: &Note) -> impl IntoElement + use<> {
    let green = rgb(t().added);
    div()
        .flex_none()
        .text_xs()
        .when(note.probable, |label| label.italic())
        .when(note.merged, |label| {
            label.px_1p5().rounded_sm().bg(Rgba { a: 0.14, ..green }).text_color(Rgba { a: 0.95, ..green })
        })
        .when(!note.merged, |label| label.text_color(rgb(t().muted)))
        .child(note.text.clone())
}

fn chip(text: SharedString, color: u32, probable: bool) -> impl IntoElement {
    div()
        .flex_none()
        .px_1p5()
        .rounded_sm()
        .border_1()
        .border_color(rgb(color))
        .text_color(rgb(color))
        .text_xs()
        .when(probable, |chip| chip.italic().opacity(0.85))
        .child(text)
}

/// Whether this row's commit is drawn with its author's picture, rather than as a plain dot.
/// `highlight` is the selected commit's branch line.
pub fn shows_face(mode: GraphFaces, entry: &Entry, highlight: Option<usize>) -> bool {
    if entry.dot == Dot::Current {
        return true;
    }
    match mode {
        GraphFaces::All => true,
        GraphFaces::Tips => entry.labels.iter().any(|l| matches!(l.kind, LabelKind::Branch | LabelKind::RemoteBranch)),
        GraphFaces::Selected => highlight == Some(entry.row.lineage),
    }
}

/// The color of a row's words. With a commit selected, `in_focus` says whether this row is on the selected
/// branch line (strongest text) or not (gray); with none selected a merge is gray and the rest normal.
fn row_text_color(in_focus: Option<bool>, merge: bool) -> Rgba {
    match in_focus {
        Some(true) => rgb(t().text_strong),
        Some(false) => rgb(t().muted),
        None if merge => rgb(t().muted),
        None => rgb(t().text),
    }
}

// `use<>`: edition 2024 would otherwise tie the element to the borrowed entry.
#[allow(clippy::too_many_arguments)]
pub fn render_entry(
    ix: usize,
    entry: &Entry,
    avatar: Avatar,
    graph_width: f32,
    selected: bool,
    highlight: Option<usize>,
    cols: Columns,
    faces: GraphFaces,
    density: Density,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let strokes = entry.row.strokes.clone();
    let lane = entry.row.lane;
    let lineage = entry.row.lineage;
    let fork_line = entry.row.joins.first().copied();
    let off_lines = entry.off_lines.clone();
    let off_branch = entry.off_branch || entry.search_miss;
    let search_miss = entry.search_miss;
    let dot = entry.dot;
    let current = dot == Dot::Current;
    let uncommitted = dot == Dot::Uncommitted;

    let badges: Vec<_> = entry
        .labels
        .iter()
        .map(|label| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .when(label.head, |item| item.child(ui::ring(line_color(lineage))))
                .child(badge(label, lineage, cx))
        })
        .collect();

    let depth = entry.depth;
    let kind = entry.kind;
    // A commit with commits listed under it folds them with a chevron drawn on its dot.
    let fold = (entry.group_size > 0).then_some(entry.collapsed);
    // Any other commit is drawn as its author's initials; it steps back like its line does.
    let face = shows_face(faces, entry, highlight);
    let plain = fold.is_none() && dot != Dot::Uncommitted && !face;
    let node = (fold.is_none() && dot != Dot::Uncommitted && face).then(|| {
        let alpha = match highlight {
            Some(h) if h != lineage => DIMMED,
            Some(_) => 1.,
            None if off_branch => OFF_BRANCH,
            None => 1.,
        };
        // The same picture, or initials and color, as the Author column shows for the person.
        let face = ui::avatar(&entry.person.name, &entry.person.email, avatar.clone(), density.node - if current { 8. } else { 4. });
        (face, t().lane(lineage), alpha)
    });
    let fold_commit = entry.commit.clone();
    let mut chips: Vec<gpui::AnyElement> = Vec::new();
    for note in &entry.notes {
        chips.push(note_label(note).into_any_element());
    }
    if entry.collapsed {
        chips.push(chip(format!("{} commits folded", entry.group_size).into(), t().muted, false).into_any_element());
    }

    // With a commit selected, the commits of its branch line read in the strongest text color and all the
    // others in gray. Only the color of the words changes: nothing else is dimmed.
    let in_focus = highlight.map(|h| entry.row.lineage == h);
    let gray = in_focus == Some(false) && !uncommitted;
    let text_color = row_text_color(in_focus, entry.merge);
    let commit = entry.commit.is_some();
    let target = entry.commit.clone().map(MenuTarget::Commit);

    div()
        .id(ix)
        .debug_selector(|| format!("row-{ix}"))
        .h(px(density.row_h))
        .w_full()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .when(current, |row| row.bg(rgb(t().head_row)))
        .when(selected, |row| row.bg(rgb(t().selected)))
        .hover(|style| style.bg(if selected { rgb(t().selected) } else { rgb(t().hover) }))
        .on_click(cx.listener(move |this, _event, _window, cx| this.select_entry(ix, cx)))
        .when(commit, |row| {
            row.on_mouse_down(
                MouseButton::Right,
                // Only the menu: a right-click never selects the commit, so it never opens the file pane.
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    if let Some(target) = target.clone() {
                        this.open_menu(event.position, target, cx);
                    }
                }),
            )
        })
        .child(
            div()
                .relative()
                .flex_none()
                .w(px(graph_width))
                .h(px(density.row_h))
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let lines = Lines { lineage, fork_line, off_lines: &off_lines, off_branch, highlight };
                            paint_lanes(bounds, &strokes, lane, &lines, dot, fold, plain, density, window)
                        },
                    )
                    .size_full(),
                )
                .when_some(node, |cell, (face, lane_color, alpha)| {
                    // The commit's node: its author's face, ringed in its branch's color (and in white
                    // for the commit HEAD is on).
                    let d = density.node;
                    cell.child(
                        div()
                            .absolute()
                            .left(px(density.x(lane) - d / 2.))
                            .top(px((density.row_h - d) / 2.))
                            .size(px(d))
                            .rounded_full()
                            .bg(rgb(lane_color))
                            .opacity(alpha)
                            .when(current, |node| node.border_2().border_color(rgb(t().text_strong)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(face),
                    )
                })
                .when(fold.is_some(), |cell| {
                    let hit = 18. * density.scale;
                    cell.child(
                        div()
                            .id(("fold", ix))
                            .debug_selector(move || format!("fold-{ix}"))
                            .absolute()
                            .left(px(density.x(lane) - hit / 2.))
                            .top(px((density.row_h - hit) / 2.))
                            .size(px(hit))
                            .rounded_full()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(commit) = fold_commit.clone() {
                                    this.toggle_group(&commit, cx);
                                }
                            })),
                    )
                }),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h_full()
                .pl(px(depth as f32 * INDENT))
                .when(off_branch && !search_miss, |cell| cell.opacity(0.6))
                .when(search_miss, |cell| cell.opacity(0.3))
                .flex()
                .items_center()
                .gap_2()
                .overflow_hidden()
                // One thin guide per level, so a group reads as belonging to the commit above it.
                .children((1..=depth).map(|level| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px((level as f32 - 1.) * INDENT + 6.))
                        .w(px(1.))
                        .bg(rgb(t().guide))
                }))
                .child(ui::kind_icon(kind))
                .children(badges)
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(text_color)
                        .when(uncommitted || current, |text| text.font_weight(FontWeight::BOLD))
                        .child(summary_text(entry, gray)),
                )
                .children(entry.pr.clone().map(|(number, url)| {
                    // The pull request's page, in the browser.
                    div()
                        .id(("pr", ix))
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(t().link))
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            cx.open_url(&url);
                        })
                        .child(format!("#{number} ↗"))
                }))
                .children(chips),
        )
        .when(cols.date != DateStyle::Hidden, |row| {
            row.child(
                div()
                    .w(px(cols.date_width()))
                    .flex_none()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(rgb(t().muted))
                    .child(if cols.date == DateStyle::Short { SharedString::from(short_date(&entry.date)) } else { entry.date.clone() }),
            )
        })
        .when(cols.author, |row| {
            row.child(
                div()
                    .w(px(Columns::AUTHOR_W))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .overflow_hidden()
                    .text_color(rgb(t().muted))
                    .when(entry.commit.is_some(), |cell| cell.child(ui::avatar(&entry.person.name, &entry.person.email, avatar, 16. * density.scale)))
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(entry.author.clone())),
            )
        })
        .when(cols.commit, |row| {
            row.child(div().w(px(Columns::COMMIT_W)).flex_none().font_family(MONO).text_color(rgb(t().muted)).child(entry.short_id.clone()))
        })
}

/// Which branch lines a row draws, and how strongly.
struct Lines<'a> {
    /// The line of the row's commit.
    lineage: usize,
    /// The first line branched from this commit.
    fork_line: Option<usize>,
    /// Lines that are not part of the current branch's history.
    off_lines: &'a [usize],
    /// The commit itself is not part of it.
    off_branch: bool,
    /// The selected commit's line.
    highlight: Option<usize>,
}

/// `fold` is set for a commit that has commits listed under it: whether they are folded away.
#[allow(clippy::too_many_arguments)]
fn paint_lanes(
    bounds: Bounds<Pixels>,
    strokes: &[Stroke],
    lane: usize,
    lines: &Lines,
    dot: Dot,
    fold: Option<bool>,
    plain: bool,
    density: Density,
    window: &mut Window,
) {
    let Lines { lineage, fork_line, off_lines, off_branch, highlight } = *lines;
    let dot_r = density.dot_r;
    let x = |lane: usize| bounds.origin.x + px(density.x(lane));
    let top = bounds.origin.y;
    let mid = top + bounds.size.height / 2.;
    let bottom = top + bounds.size.height;
    let gray = dot == Dot::Uncommitted;
    // With a line selected, every other line steps back; otherwise what the current branch does
    // not have steps back a little.
    let tone = |line: usize| {
        let color = if gray { rgb(t().muted) } else { line_color(line) };
        match highlight {
            Some(h) if h != line => faded(color, DIMMED),
            Some(_) => color,
            None if off_lines.contains(&line) => faded(color, OFF_BRANCH),
            None => color,
        }
    };

    // Dimmed lines first, so the selected line is drawn over any crossing.
    let mut order: Vec<&Stroke> = strokes.iter().collect();
    order.sort_by_key(|s| highlight == Some(s.lineage));
    for stroke in order {
        let (from, to, ctrl) = match stroke.half {
            // Comes down the lane, then bends into the dot.
            Half::Top => {
                let (from, to) = (point(x(stroke.from), top), point(x(stroke.to), mid));
                (from, to, point(from.x, mid))
            }
            // Leaves the dot sideways, then bends down into its lane.
            Half::Bottom => {
                let (from, to) = (point(x(stroke.from), mid), point(x(stroke.to), bottom));
                (from, to, point(to.x, mid))
            }
            Half::Through => {
                let (from, to) = (point(x(stroke.from), top), point(x(stroke.to), bottom));
                (from, to, from)
            }
        };
        let mut line = PathBuilder::stroke(px(density.line + if highlight == Some(stroke.lineage) { 1. } else { 0. }));
        line.move_to(from);
        if from.x == to.x {
            line.line_to(to);
        } else {
            line.curve_to(to, ctrl);
        }
        if let Ok(path) = line.build() {
            window.paint_path(path, tone(stroke.lineage));
        }
    }

    let center = point(x(lane), mid);
    let color = if off_branch && highlight.is_none() { faded(tone(lineage), OFF_BRANCH) } else { tone(lineage) };
    let mut circle = |radius: f32, fill: Rgba, border: f32, border_color: Rgba| {
        window.paint_quad(quad(
            Bounds { origin: point(center.x - px(radius), center.y - px(radius)), size: size(px(radius * 2.), px(radius * 2.)) },
            px(radius),
            fill,
            border,
            border_color,
            BorderStyle::default(),
        ));
    };
    // A commit other branches were branched from gets a ring in the color of the first of them.
    if let Some(line) = fork_line.filter(|_| dot != Dot::Current && fold.is_none()) {
        circle(density.node / 2. + 2.5, rgb(t().bg), 1.5, tone(line));
    }
    if let Some(folded) = fold {
        // A ring with a chevron in it: down while the commits under it show, right while folded.
        let r = dot_r + 3.;
        if dot == Dot::Current {
            circle(r + 2.5, rgb(t().bg), 2., rgb(t().text_strong));
        }
        circle(r, rgb(t().bg), 1.5, color);
        let a = r * 0.45;
        let points = if folded {
            [(-a * 0.5, -a), (a * 0.6, 0.), (-a * 0.5, a)]
        } else {
            [(-a, -a * 0.5), (0., a * 0.6), (a, -a * 0.5)]
        };
        let mut chevron = PathBuilder::stroke(px(1.5));
        chevron.move_to(point(center.x + px(points[0].0), center.y + px(points[0].1)));
        for (dx, dy) in &points[1..] {
            chevron.line_to(point(center.x + px(*dx), center.y + px(*dy)));
        }
        if let Ok(path) = chevron.build() {
            window.paint_path(path, color);
        }
        return;
    }
    match dot {
        // A commit's node, with its author's picture, is drawn over the lines by the row; a commit that
        // does not show one is a plain dot.
        Dot::Filled | Dot::Current if plain => circle(dot_r, color, 0., color),
        Dot::Filled | Dot::Current => {}
        Dot::Uncommitted => circle(dot_r, rgb(t().bg), 2., color),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_selected_branch_reads_in_the_strongest_color_and_the_rest_in_gray() {
        let theme = t();
        assert_eq!(row_text_color(Some(true), false), rgb(theme.text_strong));
        assert_eq!(row_text_color(Some(true), true), rgb(theme.text_strong), "a merge on the selected line is not gray");
        assert_eq!(row_text_color(Some(false), false), rgb(theme.muted));
        assert_eq!(row_text_color(None, false), rgb(theme.text), "nothing selected: as before");
        assert_eq!(row_text_color(None, true), rgb(theme.muted));
        assert_ne!(theme.text_strong, theme.muted);
    }
    use gitgui_core::Ref;

    fn commit(id: &str, parents: &[&str], refs: &[(&str, RefKind)]) -> Commit {
        Commit {
            id: id.into(),
            parents: parents.iter().map(|p| (*p).into()).collect(),
            author: "A".into(),
            email: "a@b".into(),
            time: 0,
            date: String::new(),
            summary: id.into(),
            refs: refs.iter().map(|(name, kind)| Ref { name: (*name).into(), kind: *kind }).collect(),
            stash: None,
            committer: String::new(),
            committer_email: String::new(),
        }
    }

    #[test]
    fn the_current_branch_knows_its_base_and_what_it_lacks_steps_back() {
        // docs: d1 on top of r1. feat (HEAD): f1 on r1. release: r1 -> r0.
        let commits = [
            commit("d1", &["r1"], &[("docs", RefKind::LocalBranch)]),
            commit("f1", &["r1"], &[("HEAD", RefKind::Head), ("feat", RefKind::LocalBranch)]),
            commit("r1", &["r0"], &[("release/1.0.0", RefKind::LocalBranch)]),
            commit("r0", &[], &[]),
        ];
        let built = build_entries(
            &commits,
            &Options { changed: 0, group: false, squashed: &HashMap::new(), collapsed: &HashSet::new(), web: None, people: None },
        );
        let off: Vec<bool> = built.entries.iter().map(|e| e.off_branch).collect();
        assert_eq!(off, [true, false, false, false]);
        assert_eq!(built.entries[0].off_lines, [built.entries[0].row.lineage]);
        assert!(built.entries[1].off_lines.iter().all(|&line| line != built.entries[1].row.lineage));
        assert_eq!(
            built.base,
            Some(Base { name: "release/1.0.0".into(), fork: "r1".into(), ahead: 1, behind: 0 })
        );
    }
}
