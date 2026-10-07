//! The commit table: a lane graph, ref badges and the Description / Date / Author / Commit columns.
//!
//! Every line belongs to a branch line (a lineage) and keeps one color from the branch's tip down to
//! the commit it was branched from. That commit is marked, and names the branches that start there.
//! Selecting a commit brings its branch line forward and dims the rest.

use std::collections::{HashMap, HashSet};

use gitgui_core::{
    Commit, CommitKind, Evidence, Half, Label, LabelKind, LaneLayout, Lineage, MergeClue, Placed, RefKind, Row, Stroke,
    commit_kind, commit_rank, descendants, group_by_parent, labels, lineage_names,
};
use gpui::{
    BorderStyle, Bounds, Context, FontWeight, MouseButton, PathBuilder, Pixels, Rgba, SharedString, Window, canvas,
    div, point, prelude::*, px, quad, rgb, size,
};

use crate::menu::MenuTarget;
use crate::ui::{self, BG, HEAD_ROW, HOVER, MONO, MUTED, SELECTED, TEXT, line_color};
use crate::workspace::Workspace;

pub const ROW_H: f32 = 26.0;
const LANE_W: f32 = 16.0;
const DOT_R: f32 = 4.5;
pub const MAX_DRAWN_LANES: usize = 14;
const GRAY_LINE: u32 = 0x808080;
const DATE_COMPACT_W: f32 = 120.;
/// How much of a branch line's color remains when another line is in front.
const DIMMED: f32 = 0.22;
/// How far each level of a group is pushed in, and the color of the guide beside it.
const INDENT: f32 = 18.;
const GUIDE: u32 = 0x4a4f57;

/// How the dot of a row is drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    Filled,
    /// The commit HEAD is on: a ring around the dot, so the current branch is easy to find.
    Current,
    /// The working tree, not a commit: a hollow ring.
    Uncommitted,
}

/// A line of text beside a commit about a merge: "squash-merged into release/1.0.0", "squash of feat/x".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub text: SharedString,
    /// A sentence for the overview panel, saying how it was found.
    pub detail: SharedString,
    /// A guess from messages, not a fact from history or content.
    pub probable: bool,
    /// A commit to jump to: where the squash landed, or the branch's tip.
    pub jump: Option<String>,
}

/// One table row, ready to draw.
pub struct Entry {
    pub row: Row,
    pub dot: Dot,
    pub merge: bool,
    pub labels: Vec<Label>,
    pub summary: SharedString,
    pub date: SharedString,
    pub author: SharedString,
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
}

pub struct Built {
    pub entries: Vec<Entry>,
    pub widest: usize,
    pub lineages: Vec<Lineage>,
    pub names: Vec<Option<String>>,
}

/// Arranges `commits` (newest first) as table rows, preceded by an "Uncommitted Changes" row when
/// files have changed.
pub fn build_entries(commits: &[Commit], options: &Options) -> Built {
    let changed = options.changed;
    let head = commits.iter().find(|c| c.refs.iter().any(|r| r.kind == RefKind::Head));

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
        rows.push(layout.push_ranked("uncommitted", &parents, head.map_or(0, commit_rank)));
    }
    for commit in &ordered {
        let parents: Vec<&str> =
            commit.parents.iter().map(String::as_str).filter(|p| !hidden.contains(p)).collect();
        rows.push(layout.push_ranked(&commit.id, &parents, commit_rank(commit)));
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
            date: SharedString::default(),
            author: SharedString::from("*"),
            short_id: SharedString::from("*"),
            commit: None,
            notes: Vec::new(),
            depth: 0,
            kind: CommitKind::Commit,
            group_size: 0,
            collapsed: false,
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
            date: SharedString::from(commit.date.clone()),
            author: SharedString::from(commit.author.clone()),
            short_id: SharedString::from(commit.short_id().to_owned()),
            commit: Some(commit.id.clone()),
            notes: Vec::new(),
            depth: placed.depth,
            kind: commit_kind(commit),
            group_size: *size,
            collapsed: *size > 0 && options.collapsed.contains(&commit.id),
        });
    }
    let widest = entries.iter().map(|entry| entry.row.width).max().unwrap_or(1);
    Built { entries, widest, lineages, names }
}

/// The names of the branch lines that end at this row because they were branched from its commit.
fn fork_names(row: &Row, names: &[Option<String>]) -> Vec<String> {
    let mut found: Vec<String> = row.joins.iter().filter_map(|line| names.get(*line).cloned().flatten()).collect();
    found.sort();
    found.dedup();
    found
}

/// Puts what the merge scan found next to the commits it is about: on the branch's tip, "merged into
/// release/1.0.0", and on the commit that carries the branch's changes, "squash of feat/x".
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
                });
            }
            if clue.commit.is_some() && entry.commit == clue.commit {
                entry.notes.push(Note {
                    text: format!("← squash of {}{pr}", clue.branch).into(),
                    detail: format!("This commit carries the changes of {}. {how}", clue.branch).into(),
                    probable: clue.evidence.is_probable(),
                    jump: None,
                });
            }
        }
    }
}

pub fn graph_width(widest_lanes: usize) -> f32 {
    widest_lanes.min(MAX_DRAWN_LANES + 1) as f32 * LANE_W + 8.
}

/// `compact` drops the Author and Commit columns, to leave room beside the file pane.
pub fn columns(graph_width: f32, compact: bool) -> impl IntoElement {
    let cell = |text: &'static str| div().font_weight(FontWeight::SEMIBOLD).child(text);
    div()
        .h(px(28.))
        .flex_none()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .bg(rgb(ui::PANEL))
        .border_b_1()
        .border_color(rgb(ui::BORDER))
        .child(cell("Graph").w(px(graph_width)))
        .child(cell("Description").flex_1())
        .child(cell("Date").w(px(if compact { DATE_COMPACT_W } else { 130. })))
        .when(!compact, |row| row.child(cell("Author").w(px(130.))).child(cell("Commit").w(px(64.))))
}

fn faded(color: Rgba) -> Rgba {
    Rgba { a: DIMMED, ..color }
}

/// A branch, tag, remote branch or stash on a commit. Right-click for its menu.
fn badge(label: &Label, lineage: usize, cx: &mut Context<Workspace>) -> impl IntoElement + use<> {
    let (bg, fg) = match label.kind {
        LabelKind::Branch => (line_color(lineage), rgb(0x111111)),
        LabelKind::RemoteBranch => (rgb(0x3a3d41), rgb(0xcccccc)),
        LabelKind::Tag => (rgb(0x6b5b1e), rgb(0xfff3c4)),
        LabelKind::Stash => (rgb(0x23a455), rgb(0x0b1f12)),
    };
    let name = div()
        .px_1p5()
        .bg(bg)
        .text_color(fg)
        .when(label.head, |name| name.font_weight(FontWeight::BOLD))
        .child(SharedString::from(label.name.clone()));
    let remotes = label.remotes.iter().map(|remote| {
        div()
            .px_1p5()
            .italic()
            .text_color(rgb(0xcccccc))
            .border_l_1()
            .border_color(rgb(BG))
            .bg(rgb(0x3a3d41))
            .child(SharedString::from(remote.clone()))
    });

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
        .when(label.head, |badge| badge.border_2().border_color(rgb(0xffffff)))
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
        .children(remotes)
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

// `use<>`: edition 2024 would otherwise tie the element to the borrowed entry.
pub fn render_entry(
    ix: usize,
    entry: &Entry,
    graph_width: f32,
    selected: bool,
    highlight: Option<usize>,
    compact: bool,
    cx: &mut Context<Workspace>,
) -> impl IntoElement + use<> {
    let strokes = entry.row.strokes.clone();
    let lane = entry.row.lane;
    let lineage = entry.row.lineage;
    let fork_line = entry.row.joins.first().copied();
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
    let toggle = (entry.group_size > 0).then(|| (entry.commit.clone(), entry.collapsed, entry.group_size));
    let mut chips: Vec<gpui::AnyElement> = Vec::new();
    if !entry.forks.is_empty() {
        chips.push(chip(format!("branch point · {}", entry.forks.join(", ")).into(), 0x9aa5b1, false).into_any_element());
    }
    for note in &entry.notes {
        chips.push(chip(note.text.clone(), 0x4ec9b0, note.probable).into_any_element());
    }
    if entry.collapsed {
        chips.push(chip(format!("{} commits folded", entry.group_size).into(), 0x9aa5b1, false).into_any_element());
    }

    let text_color = if entry.merge { rgb(MUTED) } else { rgb(TEXT) };
    let commit = entry.commit.is_some();
    let target = entry.commit.clone().map(MenuTarget::Commit);

    div()
        .id(ix)
        .debug_selector(|| format!("row-{ix}"))
        .h(px(ROW_H))
        .w_full()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .when(current, |row| row.bg(rgb(HEAD_ROW)))
        .when(selected, |row| row.bg(rgb(SELECTED)))
        .hover(|style| style.bg(if selected { rgb(SELECTED) } else { rgb(HOVER) }))
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
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| paint_lanes(bounds, &strokes, lane, lineage, fork_line, dot, highlight, window),
            )
            .w(px(graph_width))
            .h(px(ROW_H)),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h_full()
                .pl(px(depth as f32 * INDENT))
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
                        .bg(rgb(GUIDE))
                }))
                .child(match toggle {
                    Some((commit, collapsed, _)) => div()
                        .id(("fold", ix))
                        .flex_none()
                        .w(px(14.))
                        .text_color(rgb(MUTED))
                        .cursor_pointer()
                        .hover(|style| style.text_color(rgb(0xffffff)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if let Some(commit) = commit.clone() {
                                this.toggle_group(&commit, cx);
                            }
                        }))
                        .child(if collapsed { "▸" } else { "▾" })
                        .into_any_element(),
                    None => div().flex_none().w(px(if depth > 0 { 0. } else { 14. })).into_any_element(),
                })
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
                        .child(entry.summary.clone()),
                )
                .children(chips),
        )
        .child(
            div()
                .w(px(if compact { DATE_COMPACT_W } else { 130. }))
                .flex_none()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(rgb(MUTED))
                .child(entry.date.clone()),
        )
        .when(!compact, |row| {
            row.child(
                div()
                    .w(px(130.))
                    .flex_none()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(rgb(MUTED))
                    .child(entry.author.clone()),
            )
            .child(div().w(px(64.)).flex_none().font_family(MONO).text_color(rgb(MUTED)).child(entry.short_id.clone()))
        })
}

#[allow(clippy::too_many_arguments)]
fn paint_lanes(
    bounds: Bounds<Pixels>,
    strokes: &[Stroke],
    lane: usize,
    lineage: usize,
    fork_line: Option<usize>,
    dot: Dot,
    highlight: Option<usize>,
    window: &mut Window,
) {
    let x = |lane: usize| bounds.origin.x + px(lane.min(MAX_DRAWN_LANES) as f32 * LANE_W + LANE_W / 2.);
    let top = bounds.origin.y;
    let mid = top + bounds.size.height / 2.;
    let bottom = top + bounds.size.height;
    let gray = dot == Dot::Uncommitted;
    // With a line selected, every other line steps back.
    let tone = |line: usize| {
        let color = if gray { rgb(GRAY_LINE) } else { line_color(line) };
        if highlight.is_some_and(|h| h != line) { faded(color) } else { color }
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
        let mut line = PathBuilder::stroke(px(if highlight == Some(stroke.lineage) { 3. } else { 2. }));
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
    let color = tone(lineage);
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
    if let Some(line) = fork_line.filter(|_| dot != Dot::Current) {
        circle(DOT_R + 3.5, rgb(BG), 1.5, tone(line));
    }
    match dot {
        Dot::Filled => circle(DOT_R, color, 0., rgb(BG)),
        Dot::Current => {
            // Ring first, so the filled dot sits inside it.
            circle(DOT_R + 4., rgb(BG), 2., rgb(0xffffff));
            circle(DOT_R, color, 0., rgb(BG));
        }
        Dot::Uncommitted => circle(DOT_R, rgb(BG), 2., color),
    }
}
