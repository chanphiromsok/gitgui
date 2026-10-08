//! The conflict resolver. A file a merge, rebase or cherry-pick left in conflict opens here from the Changes list
//! instead of its diff: each conflict is a card with the two sides named by what they mean (never "ours" and
//! "theirs", which a rebase swaps), who last changed each side, the choices, and what the file will say.
//!
//! Above the cards, the commits that changed the file on each side since the sides split say why each side is the
//! way it is. Blocks that differ only in spacing, or where one side only re-spaced, can be resolved at once, each
//! saying why; anything else is for a person to decide. Nothing is written until "Use this result", and a file is
//! marked resolved only when no conflict in it is left undecided. While the operation is in progress a bar over the
//! main area says so, with Continue (once nothing is left in conflict) and Abort.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gitgui_core::{
    Block, BlockChanges, Conflict, ConflictKind, Error, GitCli, Histories, Labels, LastChange, Operation, OperationState, Reason,
    Resolution, Segment, Side, SideCommit, SideName, TextConflict, Verdict, assemble, classify, indentation_matters,
};
use gpui::{
    AnyElement, Context, FontWeight, KeyDownEvent, ListAlignment, ListOffset, ListState, SharedString, StyledText, Window, canvas, div, list,
    prelude::*, px, rgb,
};

use crate::diff_view::LINE_H;
use crate::icons;
use crate::menu::{Action, Notice, explain};
use crate::rows::Mode;
use crate::syntax::{self, Lines, Span};
use crate::theme::{self, t};
use crate::ui::{self, MONO, button, ghost, segment, segmented, toggle};
use crate::workspace::{FileState, Phase, Workspace};

/// Unchanged lines kept in view beside a conflict; more between two conflicts are folded away.
const CONTEXT: usize = 3;
/// A stretch is folded only when that hides at least this many lines.
const FOLD_AT_LEAST: usize = 4;
/// Narrower than this, a card puts its sides one under the other instead of side by side.
const STACK_BELOW: f32 = 640.;
const HEAD_H: f32 = 36.;
const LABEL_H: f32 = 24.;
/// Commits listed per side before "more".
const STORY_SHOWN: usize = 3;
/// A line longer than this many characters is cut short in the resolver (a minified file would wrap for screens).
const MOST_SHOWN: usize = 600;

/// A part of a conflict card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Current,
    Base,
    Incoming,
}

/// One row of the resolver's list. Rows are made again when a choice, a fold or the layout changes, so drawing a
/// frame costs the rows in view, whatever the file's length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// What to know about the file before choosing (it was edited by hand; what "Resolve the safe ones" did).
    Note,
    /// Unchanged line `line` of clean segment `segment`.
    Clean { segment: usize, line: usize },
    /// Unchanged lines `from..to` of a clean segment, folded away.
    Fold { segment: usize, from: usize, to: usize },
    /// The top of a card: which conflict, where, and the choices.
    Head(usize),
    /// Side by side: each side's name and who last changed it.
    Sides(usize),
    /// Side by side: line `line` of each side.
    Pair { block: usize, line: usize },
    /// One under the other (and the base, always): a part's name.
    Label { block: usize, part: Part },
    Line { block: usize, part: Part, line: usize },
    /// What the file will say there.
    ResultHead(usize),
    Result { block: usize, line: usize },
    Foot(usize),
}

/// A conflicted file open in the resolver.
pub struct Resolver {
    pub path: String,
    pub phase: Phase<()>,
    pub conflict: Option<Conflict>,
    /// A choice for each conflict, or none yet.
    pub choices: Vec<Option<Resolution>>,
    /// What each conflict is, when it can be resolved without guessing.
    pub verdicts: Vec<Option<Verdict>>,
    /// Conflicts "Resolve the safe ones" decided, so their cards say why.
    pub auto: Vec<bool>,
    /// How many it decided last, said at the top.
    pub auto_count: usize,
    /// The conflict the keys act on.
    pub focused: usize,
    pub rows: Vec<Row>,
    pub list: ListState,
    /// Clean stretches shown whole.
    pub unfolded: HashSet<usize>,
    /// Who changed the file on each side since they split; read after the file.
    pub story: Phase<Histories>,
    /// Who last changed each conflict's lines on each side.
    pub changes: BlockChanges,
    /// Show every commit of the story, not the first few.
    pub story_all: bool,
    /// Show what the common ancestor had, in each card.
    pub show_base: bool,
    /// The rows were made one under the other.
    pub stacked: bool,
    /// The list's width, as last drawn, to choose the layout.
    pub width: Rc<Cell<f32>>,
    /// Where each conflict's card starts in `rows`, and its line in the file (counting each conflict as its current side).
    pub block_rows: Vec<usize>,
    pub block_lines: Vec<usize>,
    /// The indentation every line of a conflict shares, left out in its card so the code has the room.
    pub block_indents: Vec<String>,
    /// Syntax colors of the file as each side would have it (every conflict taken from that side), read after the
    /// file; and where each segment starts in each of those, so a line of either side finds its colors.
    pub colors: Option<Box<[Lines; 2]>>,
    view_starts: Vec<[usize; 2]>,
    /// The segment each conflict is.
    block_segments: Vec<usize>,
}

impl Resolver {
    fn loading(path: String) -> Self {
        Self {
            path,
            phase: Phase::Loading,
            conflict: None,
            choices: Vec::new(),
            verdicts: Vec::new(),
            auto: Vec::new(),
            auto_count: 0,
            focused: 0,
            rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            unfolded: HashSet::new(),
            story: Phase::Loading,
            changes: Vec::new(),
            story_all: false,
            show_base: true,
            stacked: false,
            width: Rc::new(Cell::new(0.)),
            block_rows: Vec::new(),
            block_lines: Vec::new(),
            block_indents: Vec::new(),
            colors: None,
            view_starts: Vec::new(),
            block_segments: Vec::new(),
        }
    }

    pub fn text(&self) -> Option<&TextConflict> {
        match &self.conflict.as_ref()?.kind {
            ConflictKind::Text(text) => Some(text),
            _ => None,
        }
    }

    pub fn block(&self, block: usize) -> Option<&Block> {
        match self.text()?.segments.get(*self.block_segments.get(block)?)? {
            Segment::Block(found) => Some(found),
            Segment::Clean(_) => None,
        }
    }

    /// How many conflicts the file has.
    pub fn blocks(&self) -> usize {
        self.block_segments.len()
    }

    /// How many have a choice.
    pub fn decided(&self) -> usize {
        self.choices.iter().filter(|c| c.is_some()).count()
    }

    /// How many undecided ones "Resolve the safe ones" would decide.
    pub fn safe_left(&self) -> usize {
        self.verdicts.iter().zip(&self.choices).filter(|(verdict, choice)| choice.is_none() && verdict.is_some_and(|v| v.certain)).count()
    }

    /// Takes the file as just read. Choices made are kept when it is the same conflict as before (read again with
    /// nothing changed).
    fn set_conflict(&mut self, conflict: Conflict) {
        let same = self.conflict.as_ref().is_some_and(|old| old.kind == conflict.kind);
        self.conflict = Some(conflict);
        let segments: &[Segment] = self.text().map_or(&[], |text| &text.segments);
        let block_segments: Vec<usize> =
            segments.iter().enumerate().filter(|(_, s)| matches!(s, Segment::Block(_))).map(|(i, _)| i).collect();
        let indentation = indentation_matters(&self.path);
        let verdicts = gitgui_core::conflict::blocks(segments).map(|block| classify(block, indentation)).collect();
        let indents = gitgui_core::conflict::blocks(segments).map(shared_indent).collect();
        let mut at = [0, 0];
        let starts = segments
            .iter()
            .map(|segment| {
                let start = at;
                let (current, incoming) = match segment {
                    Segment::Clean(lines) => (lines.len(), lines.len()),
                    Segment::Block(block) => (block.current.len(), block.incoming.len()),
                };
                at = [at[0] + current, at[1] + incoming];
                start
            })
            .collect();
        (self.block_segments, self.verdicts, self.block_indents, self.view_starts) = (block_segments, verdicts, indents, starts);
        if !same {
            let n = self.block_segments.len();
            // Another conflict: nothing measured or scrolled in the old one applies.
            self.rows.clear();
            self.list.reset(0);
            self.choices = vec![None; n];
            self.auto = vec![false; n];
            self.auto_count = 0;
            self.focused = 0;
            self.unfolded.clear();
            self.changes.clear();
            self.colors = None;
        }
        self.phase = Phase::Ready(());
    }

    /// The colors of line `line` (from 0) of segment `segment`, as `side` has it.
    fn spans(&self, segment: usize, side: Side, line: usize) -> &[Span] {
        let (Some(colors), Some(starts)) = (self.colors.as_deref(), self.view_starts.get(segment)) else { return &[] };
        let view = side as usize;
        colors[view].line((starts[view] + line + 1) as u32)
    }

    fn choose(&mut self, block: usize, choice: Option<Resolution>) {
        if let Some(slot) = self.choices.get_mut(block) {
            *slot = choice;
            self.auto[block] = false;
        }
    }

    /// The next conflict after `after` with no choice yet, going round.
    fn next_undecided(&self, after: usize) -> Option<usize> {
        let n = self.blocks();
        (1..=n).map(|step| (after + step) % n).find(|&b| self.choices[b].is_none())
    }

    /// Scrolls the focused conflict's card into view, a few lines under the top, unless all of it is in view already.
    fn reveal_focused(&self) {
        let Some(&head) = self.block_rows.get(self.focused) else { return };
        let foot = self.rows[head..].iter().position(|row| matches!(row, Row::Foot(_))).map_or(head, |at| head + at);
        let view = self.list.viewport_bounds();
        let shown = |ix: usize| self.list.bounds_for_item(ix).is_some_and(|b| b.top() >= view.top() && b.bottom() <= view.bottom());
        if shown(head) && shown(foot) {
            return;
        }
        self.list.scroll_to(ListOffset { item_ix: head.saturating_sub(CONTEXT), offset_in_item: px(0.) });
    }

    /// Makes the rows again from the file, the choices and the folds. Only the rows that changed are handed to the list
    /// again, so it keeps the heights it measured and the place it is scrolled to.
    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let (mut block_rows, mut block_lines) = (Vec::new(), Vec::new());
        if let Some(text) = self.text() {
            if self.note().is_some() {
                rows.push(Row::Note);
            }
            let last = text.segments.len().saturating_sub(1);
            let (mut block, mut line) = (0, 1);
            for (s, segment) in text.segments.iter().enumerate() {
                match segment {
                    Segment::Clean(lines) => {
                        let len = lines.len();
                        // The start and the end of the file need no lines kept beside a conflict.
                        let (keep_top, keep_bottom) = (if s == 0 { 0 } else { CONTEXT }, if s == last { 0 } else { CONTEXT });
                        if self.unfolded.contains(&s) || len < keep_top + keep_bottom + FOLD_AT_LEAST {
                            rows.extend((0..len).map(|l| Row::Clean { segment: s, line: l }));
                        } else {
                            rows.extend((0..keep_top).map(|l| Row::Clean { segment: s, line: l }));
                            rows.push(Row::Fold { segment: s, from: keep_top, to: len - keep_bottom });
                            rows.extend((len - keep_bottom..len).map(|l| Row::Clean { segment: s, line: l }));
                        }
                        line += len;
                    }
                    Segment::Block(conflict) => {
                        block_rows.push(rows.len());
                        block_lines.push(line);
                        rows.push(Row::Head(block));
                        let base = conflict.base.as_ref().filter(|_| self.show_base);
                        // An empty base says so on its label: no row of its own for "nothing here".
                        let push_part = |rows: &mut Vec<Row>, part: Part, lines: &[String]| {
                            rows.push(Row::Label { block, part });
                            let count = if part == Part::Base { lines.len() } else { lines.len().max(1) };
                            rows.extend((0..count).map(|l| Row::Line { block, part, line: l }));
                        };
                        if self.stacked {
                            push_part(&mut rows, Part::Current, &conflict.current);
                            if let Some(base) = base {
                                push_part(&mut rows, Part::Base, base);
                            }
                            push_part(&mut rows, Part::Incoming, &conflict.incoming);
                        } else {
                            rows.push(Row::Sides(block));
                            rows.extend((0..conflict.current.len().max(conflict.incoming.len())).map(|l| Row::Pair { block, line: l }));
                            if let Some(base) = base {
                                push_part(&mut rows, Part::Base, base);
                            }
                        }
                        rows.push(Row::ResultHead(block));
                        if let Some(choice) = self.choices[block] {
                            rows.extend((0..choice.len(conflict).max(1)).map(|l| Row::Result { block, line: l }));
                        }
                        rows.push(Row::Foot(block));
                        line += conflict.current.len();
                        block += 1;
                    }
                }
            }
        }
        let old = std::mem::replace(&mut self.rows, rows);
        (self.block_rows, self.block_lines) = (block_rows, block_lines);
        if self.list.item_count() != old.len() {
            return self.list.reset(self.rows.len());
        }
        let same_start = old.iter().zip(&self.rows).take_while(|(a, b)| a == b).count();
        let same_end = old[same_start..].iter().rev().zip(self.rows[same_start..].iter().rev()).take_while(|(a, b)| a == b).count();
        self.list.splice(same_start..old.len() - same_end, self.rows.len() - same_start - same_end);
    }

    /// What to say at the top of the file, if anything.
    fn note(&self) -> Option<String> {
        let text = self.text()?;
        if text.segments.iter().all(|s| matches!(s, Segment::Clean(_))) {
            return Some("No conflict markers are left in this file: it was resolved by hand. Mark it resolved to stage it as it is.".into());
        }
        let mut said = Vec::new();
        if !text.pristine {
            said.push(
                "This file was changed since git left the conflict (edited in an editor, or written here with conflicts left), so it is \
                 shown as it is on disk. Start over puts back git's conflict."
                    .to_owned(),
            );
        }
        if self.auto_count > 0 {
            let n = self.auto_count;
            said.push(format!(
                "Resolved {n} conflict{} safely; each says why under it. Start over undoes {}.",
                if n == 1 { "" } else { "s" },
                if n == 1 { "it" } else { "them" }
            ));
        }
        (!said.is_empty()).then(|| said.join("  "))
    }
}

/// A line as it is drawn: no line ending, less `indent` (or, for a line that does not have it, a blank one, all its
/// leading space), tabs as four spaces, and no more than a screen or two of it. Also gives how many bytes were left
/// off the front, how many are shown, and where the tabs were in those, to place colors read from the whole line.
fn display_line(line: &str, indent: &str) -> (String, usize, usize, Vec<usize>) {
    let raw = line.trim_end_matches(['\n', '\r']);
    let skip = if raw.starts_with(indent) { indent.len() } else { raw.len() - raw.trim_start().len() };
    let body = &raw[skip..];
    let (body, cut) = match body.char_indices().nth(MOST_SHOWN) {
        Some((at, _)) => (&body[..at], true),
        None => (body, false),
    };
    let tabs = body.match_indices('\t').map(|(at, _)| at).collect();
    let mut text = body.replace('\t', "    ");
    if cut {
        text.push('…');
    }
    (text, skip, body.len(), tabs)
}

/// A line of code with its syntax colors (`spans`, byte ranges of the whole line).
fn styled(line: &str, indent: &str, spans: &[Span]) -> StyledText {
    let (text, skip, shown, tabs) = display_line(line, indent);
    let body = &line[skip..skip + shown];
    let at = |byte: usize| byte + 3 * tabs.partition_point(|&tab| tab < byte);
    let theme = t();
    let highlights: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> = spans
        .iter()
        .filter_map(|(range, name)| {
            let from = (range.start as usize).saturating_sub(skip);
            let to = (range.end as usize).saturating_sub(skip).min(shown);
            if from >= to || !body.is_char_boundary(from) || !body.is_char_boundary(to) {
                return None;
            }
            Some((at(from)..at(to), theme.syntax_style(*name)?))
        })
        .collect();
    StyledText::new(text).with_highlights(highlights)
}

/// Plain words where a line of code would be.
fn words(text: &'static str) -> StyledText {
    StyledText::new(text)
}

/// Where line `line` of what `choice` gives a block came from: a side, and the line there.
fn result_source(choice: Resolution, block: &Block, line: usize) -> Option<(Side, usize)> {
    let split = |first: Side| {
        let n = block.side(first).len();
        if line < n { (first, line) } else { (first.other(), line - n) }
    };
    match choice {
        Resolution::Current => Some((Side::Current, line)),
        Resolution::Incoming => Some((Side::Incoming, line)),
        Resolution::CurrentThenIncoming => Some(split(Side::Current)),
        Resolution::IncomingThenCurrent => Some(split(Side::Incoming)),
        Resolution::Base => None,
    }
}

/// The leading spaces and tabs every non-blank line of a conflict has.
fn shared_indent(block: &Block) -> String {
    let lines = block.current.iter().chain(block.base.iter().flatten()).chain(&block.incoming).filter(|l| !l.trim().is_empty());
    let mut shared: Option<&str> = None;
    for line in lines {
        let lead = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
        shared = Some(match shared {
            None => lead,
            Some(so_far) => {
                let same = so_far.bytes().zip(lead.bytes()).take_while(|(a, b)| a == b).count();
                &so_far[..same]
            }
        });
    }
    shared.unwrap_or("").to_owned()
}

/// A name cut to `most` characters with an ellipsis, for a button.
fn clipped(name: &str, most: usize) -> String {
    if name.chars().count() <= most {
        return name.to_owned();
    }
    let mut cut: String = name.chars().take(most.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The syntax colors of a text conflict as each side would have the whole file; none for a language not known.
fn side_colors(conflict: &Conflict) -> Option<Box<[Lines; 2]>> {
    let ConflictKind::Text(text) = &conflict.kind else { return None };
    let none = Labels { current: String::new(), base: String::new(), incoming: String::new() };
    let n = gitgui_core::conflict::block_count(&text.segments);
    let view = |side| syntax::highlight(&conflict.path, &assemble(&text.segments, &vec![Some(side); n], &none, text.marker_size).text);
    Some(Box::new([view(Resolution::Current)?, view(Resolution::Incoming)?]))
}

/// The color that marks a side: the theme's first two line colors.
fn side_color(side: Side) -> u32 {
    match side {
        Side::Current => t().lane(0),
        Side::Incoming => t().lane(1),
    }
}

fn tint(side: Side) -> u32 {
    theme::mix(t().editor_bg, side_color(side), if t().dark { 0.13 } else { 0.10 })
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Why "Resolve the safe ones" decided a conflict, in words.
pub(crate) fn reason_text(reason: Reason, names: &(SideName, SideName)) -> String {
    let name = |side: Side| if side == Side::Current { &names.0.short } else { &names.1.short };
    match reason {
        Reason::SameChange => format!("both sides made the same change, spaced differently; kept {}'s", names.0.short),
        Reason::OnlySpacing(side) => format!("{} only re-spaced these lines; took {}'s change", name(side), name(side.other())),
        Reason::Untouched(side) => format!("{} left these lines as they were; took {}'s change", name(side), name(side.other())),
        Reason::BothAdded => "both sides added lines here".to_owned(),
    }
}

/// What a choice puts in the file, in words.
pub(crate) fn choice_text(choice: Resolution, names: &(SideName, SideName)) -> String {
    match choice {
        Resolution::Current => format!("{}'s version", names.0.short),
        Resolution::Incoming => format!("{}'s version", names.1.short),
        Resolution::CurrentThenIncoming => format!("both, {} first", names.0.short),
        Resolution::IncomingThenCurrent => format!("both, {} first", names.1.short),
        Resolution::Base => "neither: the lines as they were before both changes".to_owned(),
    }
}

impl Workspace {
    pub(crate) fn resolver(&self) -> Option<&Resolver> {
        self.repo.as_ref()?.file.as_ref()?.resolver.as_deref()
    }

    fn resolver_mut(&mut self) -> Option<&mut Resolver> {
        self.repo.as_mut()?.file.as_mut()?.resolver.as_deref_mut()
    }

    /// The merge, rebase or cherry-pick in progress, as last read.
    pub(crate) fn operation(&self) -> Option<&OperationState> {
        match &self.repo.as_ref()?.phase {
            Phase::Ready(view) => view.operation.as_ref(),
            _ => None,
        }
    }

    /// The sides' names, from the operation in progress; plain words when git says of none.
    pub(crate) fn side_names(&self) -> (SideName, SideName) {
        match self.operation() {
            Some(op) => (op.current.clone(), op.incoming.clone()),
            None => (
                SideName { title: "What HEAD has".into(), short: "HEAD".into(), place: "on HEAD".into() },
                SideName { title: "What is being applied".into(), short: "incoming".into(), place: "in what is being applied".into() },
            ),
        }
    }

    /// The words on conflict markers the app writes.
    fn labels(&self) -> Labels {
        let (current, incoming) = self.side_names();
        Labels { current: current.title, base: "Before either change".into(), incoming: incoming.title }
    }

    /// How many files still have conflicts.
    pub(crate) fn conflicts_left(&self) -> usize {
        self.repo.as_ref().map_or(0, |repo| repo.work.iter().filter(|f| f.conflicted).count())
    }

    /// Opens the first file with conflicts, if there is one, with the graph out of the way until the operation ends:
    /// the conflicts need the room.
    pub fn open_first_conflict(&mut self, cx: &mut Context<Self>) {
        let index = self.repo.as_ref().and_then(|repo| repo.work.iter().position(|f| f.conflicted));
        let Some(index) = index else { return };
        if !self.graph_hidden {
            (self.graph_hidden, self.graph_hidden_for_conflicts) = (true, true);
        }
        self.open_work_file(index, cx);
    }

    /// Opens the working tree's file `index`, which has conflicts, in the resolver.
    pub fn open_conflict(&mut self, index: usize, cx: &mut Context<Self>) {
        self.cancel_file_load();
        self.hover_line = None;
        let Some(repo) = self.repo.as_mut() else { return };
        let Some(path) = repo.work.get(index).map(|f| f.change.path.clone()) else { return };
        let mut file = FileState::new(index);
        file.resolver = Some(Box::new(Resolver::loading(path)));
        repo.file = Some(file);
        cx.notify();
        self.load_conflict(cx);
    }

    /// Reads the open conflicted file (again), then who changed it on each side.
    fn load_conflict(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.repo.as_ref().map(|repo| repo.project.path.clone()) else { return };
        let Some(path) = self.resolver().map(|r| r.path.clone()) else { return };
        let read_path = path.clone();
        self.spawn_load(
            cx,
            move || {
                let git = GitCli::new(root);
                let conflict = git.conflict(&read_path)?;
                // A picture git cannot merge is chosen by looking at both.
                let pictures = (conflict.kind == ConflictKind::Whole && crate::preview::is_image(&read_path)).then(|| {
                    // `:2:path` is what HEAD has, `:3:path` what is being applied.
                    let stage = |n: u8| crate::preview::preview(&read_path, &git.file_bytes_at(&format!(":{n}"), &read_path).ok()??);
                    crate::preview::Images { old: stage(2), new: stage(3) }
                });
                Ok((conflict, pictures))
            },
            move |this, result: Result<(Conflict, Option<crate::preview::Images>), Error>, cx| {
                let unified = this.repo.as_ref().is_some_and(|repo| repo.mode == Mode::Unified);
                if let (Ok((_, pictures)), Some(file)) = (&result, this.repo.as_mut().and_then(|repo| repo.file.as_mut())) {
                    file.images = pictures.clone();
                }
                let Some(resolver) = this.resolver_mut().filter(|r| r.path == path) else { return };
                match result.map(|(conflict, _)| conflict) {
                    Ok(conflict) => {
                        resolver.set_conflict(conflict);
                        let narrow = resolver.width.get() > 0. && resolver.width.get() < STACK_BELOW;
                        resolver.stacked = narrow || unified;
                        resolver.rebuild();
                        this.load_story(cx);
                    }
                    Err(error) => resolver.phase = Phase::Failed(explain(&error).into()),
                }
                cx.notify();
            },
        );
    }

    /// Reads, in the background, the commits that changed the open file on each side, and who last changed each
    /// conflict's lines.
    fn load_story(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.repo.as_ref().map(|repo| repo.project.path.clone()) else { return };
        let incoming = self.operation().and_then(|op| op.incoming_commit.clone());
        let Some(resolver) = self.resolver_mut() else { return };
        let Some(conflict) = resolver.conflict.clone() else { return };
        let Some(incoming) = incoming else {
            resolver.story = Phase::Failed("No commit is being applied.".into());
            resolver.colors = side_colors(&conflict);
            return;
        };
        let path = resolver.path.clone();
        let kind = conflict.kind.clone();
        self.spawn_load(
            cx,
            move || {
                let git = GitCli::new(root);
                let histories = git.conflict_histories(&conflict.path, &incoming);
                (histories, git.block_changes(&conflict, &incoming).unwrap_or_default(), side_colors(&conflict))
            },
            move |this, (histories, changes, colors), cx| {
                let Some(resolver) = this.resolver_mut().filter(|r| r.path == path) else { return };
                resolver.story = match histories {
                    Ok(histories) => Phase::Ready(histories),
                    Err(error) => Phase::Failed(explain(&error).into()),
                };
                if resolver.conflict.as_ref().is_some_and(|c| c.kind == kind) {
                    (resolver.changes, resolver.colors) = (changes, colors);
                }
                cx.notify();
            },
        );
    }

    /// Makes `choice` (or none) for conflict `block`. `advance` goes on to the next conflict with no choice yet, as the
    /// keys do; a click stays.
    pub fn choose_block(&mut self, block: usize, choice: Option<Resolution>, advance: bool, cx: &mut Context<Self>) {
        let Some(resolver) = self.resolver_mut() else { return };
        if block >= resolver.blocks() {
            return;
        }
        resolver.choose(block, choice);
        resolver.focused = block;
        let next = (advance && choice.is_some()).then(|| resolver.next_undecided(block)).flatten();
        resolver.rebuild();
        if let Some(next) = next {
            resolver.focused = next;
            resolver.reveal_focused();
        }
        cx.notify();
    }

    /// Goes to the next (`1`) or previous (`-1`) conflict, round the end.
    pub fn step_block(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(resolver) = self.resolver_mut() else { return };
        let n = resolver.blocks() as isize;
        if n == 0 {
            return;
        }
        resolver.focused = (resolver.focused as isize + delta).rem_euclid(n) as usize;
        resolver.reveal_focused();
        cx.notify();
    }

    /// Decides every undecided conflict that can be decided without guessing, each with its reason.
    pub fn resolve_safe(&mut self, cx: &mut Context<Self>) {
        let Some(resolver) = self.resolver_mut() else { return };
        let mut count = 0;
        for block in 0..resolver.blocks() {
            if let (None, Some(verdict)) = (resolver.choices[block], resolver.verdicts[block])
                && verdict.certain
            {
                resolver.choices[block] = Some(verdict.resolution);
                resolver.auto[block] = true;
                count += 1;
            }
        }
        if count == 0 {
            return;
        }
        resolver.auto_count = count;
        resolver.rebuild();
        cx.notify();
    }

    /// Unfolds a stretch of unchanged lines.
    pub fn unfold(&mut self, segment: usize, cx: &mut Context<Self>) {
        if let Some(resolver) = self.resolver_mut() {
            resolver.unfolded.insert(segment);
            resolver.rebuild();
            cx.notify();
        }
    }

    pub fn toggle_conflict_base(&mut self, cx: &mut Context<Self>) {
        if let Some(resolver) = self.resolver_mut() {
            resolver.show_base = !resolver.show_base;
            resolver.rebuild();
            cx.notify();
        }
    }

    pub fn toggle_story_all(&mut self, cx: &mut Context<Self>) {
        if let Some(resolver) = self.resolver_mut() {
            resolver.story_all = !resolver.story_all;
            cx.notify();
        }
    }

    /// Start over for the file: forgets the choices made here. A file that was changed on disk is put back as git
    /// made the conflict, after asking, since that replaces what is in it.
    pub fn start_over(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(resolver) = self.resolver_mut() else { return };
        if resolver.text().is_some_and(|text| text.pristine) {
            let n = resolver.blocks();
            (resolver.choices, resolver.auto, resolver.auto_count, resolver.focused) = (vec![None; n], vec![false; n], 0, 0);
            resolver.rebuild();
            return cx.notify();
        }
        let path = resolver.path.clone();
        self.open_dialog(Action::StartOver(path), window, cx);
    }

    /// Puts the file's conflict back as the index has it (the dialog's yes).
    pub(crate) fn restart_conflict(&mut self, path: String, cx: &mut Context<Self>) {
        let labels = self.labels();
        self.conflict_job(
            move |git| git.restart_conflict(&path, &labels),
            |this, result, cx| match result {
                Ok(()) => {
                    if let Some(resolver) = this.resolver_mut() {
                        // A new conflict: nothing chosen in the old one carries over.
                        resolver.conflict = None;
                    }
                    this.load_conflict(cx);
                }
                Err(error) => this.fail(explain(&error), cx),
            },
            cx,
        );
    }

    /// Runs file work for the resolver off the UI thread, then `done`.
    fn conflict_job<T: Send + 'static>(
        &mut self,
        job: impl FnOnce(GitCli) -> Result<T, Error> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, Error>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.repo.as_ref().map(|repo| repo.project.path.clone()) else { return };
        self.spawn_load(cx, move || job(GitCli::new(root)), done);
    }

    /// Writes the file as chosen. With every conflict decided it is also marked resolved and the next file with
    /// conflicts opens; with some left, they are written with markers (named by meaning) to finish here or elsewhere.
    pub fn use_result(&mut self, cx: &mut Context<Self>) {
        let labels = self.labels();
        let Some(resolver) = self.resolver() else { return };
        let (Some(text), Some(conflict)) = (resolver.text(), resolver.conflict.as_ref()) else { return };
        let path = resolver.path.clone();
        if resolver.blocks() == 0 {
            return self.mark_file_resolved(cx);
        }
        if resolver.decided() == 0 {
            return self.say(Notice::warn("Choose a side for a conflict first: nothing would change."), cx);
        }
        let assembled = assemble(&text.segments, &resolver.choices, &labels, text.marker_size);
        let resolved = assembled.unresolved.is_empty();
        let left = assembled.unresolved.len();
        let expected = conflict.disk.clone();
        let name = file_name(&path).to_owned();
        let write_path = path.clone();
        self.conflict_job(
            move |git| git.write_resolution(&write_path, &assembled.text, expected.as_deref(), resolved),
            move |this, result, cx| match result {
                Ok(()) if resolved => {
                    this.notice = Some(Notice::info(format!("Resolved {name}.")));
                    this.after_resolving(path, cx);
                }
                Ok(()) => {
                    this.notice = Some(Notice::info(format!(
                        "Wrote {name}; {left} conflict{} still marked in it.",
                        if left == 1 { " is" } else { "s are" }
                    )));
                    this.load_conflict(cx);
                }
                Err(error) => this.fail(explain(&error), cx),
            },
            cx,
        );
    }

    /// Marks the open file resolved as it is on disk (it has no markers left).
    pub fn mark_file_resolved(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.resolver().map(|r| r.path.clone()) else { return };
        let mark = path.clone();
        self.conflict_job(
            move |git| git.mark_resolved(&mark),
            move |this, result, cx| match result {
                Ok(()) => {
                    this.notice = Some(Notice::info(format!("Marked {} resolved.", file_name(&path))));
                    this.after_resolving(path, cx);
                }
                Err(error) => this.fail(explain(&error), cx),
            },
            cx,
        );
    }

    /// Resolves the open file with one side's whole version: a binary file, or the changed file of a delete/modify
    /// conflict.
    pub fn take_whole(&mut self, side: Side, cx: &mut Context<Self>) {
        let Some(resolver) = self.resolver() else { return };
        let Some(conflict) = resolver.conflict.as_ref() else { return };
        let (path, expected) = (resolver.path.clone(), conflict.disk.clone());
        let take = path.clone();
        self.conflict_job(
            move |git| git.take_side(&take, side, expected.as_deref()),
            move |this, result, cx| match result {
                Ok(()) => {
                    this.notice = Some(Notice::info(format!("Resolved {}.", file_name(&path))));
                    this.after_resolving(path, cx);
                }
                Err(error) => this.fail(explain(&error), cx),
            },
            cx,
        );
    }

    /// Resolves the open file by deleting it (the dialog's yes).
    pub(crate) fn delete_conflicted(&mut self, path: String, cx: &mut Context<Self>) {
        let expected = self.resolver().filter(|r| r.path == path).and_then(|r| r.conflict.as_ref()).and_then(|c| c.disk.clone());
        let delete = path.clone();
        self.conflict_job(
            move |git| git.delete_conflicted(&delete, expected.as_deref()),
            move |this, result, cx| match result {
                Ok(()) => {
                    this.notice = Some(Notice::info(format!("Deleted {}.", file_name(&path))));
                    this.after_resolving(path, cx);
                }
                Err(error) => this.fail(explain(&error), cx),
            },
            cx,
        );
    }

    /// A file was resolved: the changes are read again and the next file with conflicts opens.
    fn after_resolving(&mut self, path: String, cx: &mut Context<Self>) {
        self.reload_work(crate::changes::AfterWork::NextConflict(path), cx);
    }

    /// Opens the file in the program the system opens it with, to edit by hand; Re-read picks the edit up.
    pub fn edit_in_editor(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.repo.as_ref().map(|repo| repo.project.path.clone()) else { return };
        let Some(resolver) = self.resolver() else { return };
        let (path, decided) = (resolver.path.clone(), resolver.decided());
        // Tests never open other programs.
        if !cfg!(test) {
            cx.open_with_system(&root.join(&path));
        }
        let unsaved = if decided > 0 { " The choices made here are not in the file: Use this result first to keep them." } else { "" };
        self.say(Notice::info(format!("Opened {} in your editor. Save it there, then Re-read.{unsaved}", file_name(&path))), cx);
    }

    /// Reads the open file again (after an edit in an editor), keeping the choices if it is the same conflict.
    pub fn reread_conflict(&mut self, cx: &mut Context<Self>) {
        self.load_conflict(cx);
    }

    /// Continue: finishes the merge, rebase or cherry-pick once nothing is left in conflict.
    pub fn continue_operation(&mut self, cx: &mut Context<Self>) {
        let Some(operation) = self.operation().map(|op| op.operation) else { return };
        if self.busy.is_some() {
            return self.say(Notice::warn("Another operation is still running."), cx);
        }
        let left = self.conflicts_left();
        if left > 0 {
            return self.say(Notice::warn(format!("Resolve the {left} file{} with conflicts first.", if left == 1 { "" } else { "s" })), cx);
        }
        let (busy, done) = match operation {
            Operation::Merge => ("Finishing the merge…", "Finished the merge."),
            Operation::Rebase => ("Continuing the rebase…", "Finished the rebase."),
            Operation::CherryPick => ("Finishing the cherry-pick…", "Finished the cherry-pick."),
        };
        self.run(busy.into(), done.into(), None, move |git| git.continue_operation(operation), cx);
    }

    /// The keys of the resolver, while a conflicted file is open and no field or window has the keyboard:
    /// N and P (or J and K) next and previous conflict, 1 the current side, 2 the incoming side, 3 both (current
    /// first), 4 both (incoming first), 0 neither (the base), Backspace undo the choice, Cmd-Enter use the result.
    pub fn resolver_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.resolver().is_none_or(|r| r.text().is_none())
            || self.dialog.is_some()
            || self.menu.is_some()
            || self.new_branch.is_some()
            || self.settings_open
            || self.typing(window, cx)
        {
            return;
        }
        let keys = &event.keystroke;
        let focused = self.resolver().map_or(0, |r| r.focused);
        if keys.modifiers.secondary() && keys.key == "enter" {
            self.use_result(cx);
            return cx.stop_propagation();
        }
        if keys.modifiers.control || keys.modifiers.alt || keys.modifiers.platform || keys.modifiers.function {
            return;
        }
        match keys.key.as_str() {
            "n" | "j" => self.step_block(1, cx),
            "p" | "k" => self.step_block(-1, cx),
            "1" => self.choose_block(focused, Some(Resolution::Current), true, cx),
            "2" => self.choose_block(focused, Some(Resolution::Incoming), true, cx),
            "3" => self.choose_block(focused, Some(Resolution::CurrentThenIncoming), true, cx),
            "4" => self.choose_block(focused, Some(Resolution::IncomingThenCurrent), true, cx),
            "0" => {
                if self.resolver().and_then(|r| r.block(focused)).is_some_and(|b| b.base.is_some()) {
                    self.choose_block(focused, Some(Resolution::Base), true, cx);
                }
            }
            "backspace" | "delete" => self.choose_block(focused, None, false, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    // ---- drawing ------------------------------------------------------------------------------

    /// The file pane while a conflicted file is open.
    pub fn render_resolver(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.repo.as_ref().map_or(Mode::Split, |repo| repo.mode);
        let names = self.side_names();
        let file_number = {
            let conflicted: Vec<&str> = self
                .repo
                .as_ref()
                .map(|repo| repo.work.iter().filter(|f| f.conflicted).map(|f| f.change.path.as_str()).collect())
                .unwrap_or_default();
            let path = self.resolver().map(|r| r.path.as_str()).unwrap_or("");
            conflicted.iter().position(|p| *p == path).map(|at| (at + 1, conflicted.len()))
        };
        // The layout follows the width: side by side when there is room and Split is chosen.
        if let Some(resolver) = self.resolver_mut() {
            let width = resolver.width.get();
            let stacked = mode == Mode::Unified || (width > 0. && width < STACK_BELOW);
            if stacked != resolver.stacked && resolver.text().is_some() {
                resolver.stacked = stacked;
                resolver.rebuild();
            }
        }
        let Some(resolver) = self.resolver() else { return div().into_any_element() };
        let path = resolver.path.clone();
        let (folder, name) = path.rsplit_once('/').map_or(("", path.as_str()), |(folder, name)| (folder, name));

        let progress = match (resolver.text(), file_number) {
            (Some(_), Some((at, of))) if resolver.blocks() > 0 => {
                format!("{} of {} resolved · file {at} of {of}", resolver.decided(), resolver.blocks())
            }
            (_, Some((at, of))) => format!("file {at} of {of}"),
            _ => String::new(),
        };
        let has_base = resolver.text().is_some_and(|text| gitgui_core::conflict::blocks(&text.segments).any(|b| b.base.is_some()));
        let toolbar = div()
            .h(px(32.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .overflow_hidden()
            .border_b_1()
            .border_color(rgb(t().border))
            .child(ghost("overview", "‹ Overview").on_click(cx.listener(|this, _, _, cx| this.close_file(cx))))
            .child(ui::file_icon(icons::file(name)))
            .child(
                div()
                    .flex_none()
                    .max_w(px(300.))
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(t().text_strong))
                    .child(SharedString::from(name.to_owned())),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_shrink()
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .text_xs()
                    .text_color(rgb(t().muted))
                    .child(SharedString::from(folder.to_owned())),
            )
            .child(
                div()
                    .debug_selector(|| "conflict-progress".to_owned())
                    .flex_none()
                    .px_2()
                    .text_xs()
                    .text_color(rgb(t().muted))
                    .child(progress),
            )
            .child(div().flex_1())
            .when(resolver.blocks() > 1, |bar| {
                bar.child(
                    ghost("conflict-previous", "↑ Previous")
                        .debug_selector(|| "conflict-previous".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.step_block(-1, cx))),
                )
                .child(
                    ghost("conflict-next", "↓ Next")
                        .debug_selector(|| "conflict-next".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.step_block(1, cx))),
                )
            })
            .when(has_base, |bar| {
                bar.child(segmented(vec![
                    toggle("conflict-base", "Base", resolver.show_base)
                        .debug_selector(|| "conflict-base".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_conflict_base(cx))),
                ]))
            });

        let body: AnyElement = match (&resolver.phase, resolver.conflict.as_ref().map(|c| &c.kind)) {
            (Phase::Loading, _) => centered("Reading the conflict…", t().muted),
            (Phase::Failed(message), _) => centered(message.clone(), t().removed),
            (Phase::Ready(()), Some(ConflictKind::Text(_))) => {
                let width = resolver.width.clone();
                let story = self.render_story(&names, cx);
                let rows = list(resolver.list.clone(), cx.processor(|this, ix: usize, _window, cx| this.render_resolver_row(ix, cx))).size_full();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .children(story)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .bg(rgb(t().editor_bg))
                            .child(canvas(move |b, _, _| width.set(f32::from(b.size.width)), |_, _, _, _| {}).absolute().size_full())
                            .child(div().debug_selector(|| "conflict-list".to_owned()).size_full().child(rows)),
                    )
                    .child(self.render_resolver_footer(&names, cx))
                    .into_any_element()
            }
            (Phase::Ready(()), Some(kind)) => self.render_whole_file(kind.clone(), &names, cx),
            (Phase::Ready(()), None) => div().into_any_element(),
        };
        div().size_full().flex().flex_col().child(toolbar).child(div().flex_1().min_h_0().child(body)).into_any_element()
    }

    /// Why each side is the way it is: the commits that changed the file on each side since they split.
    fn render_story(&self, names: &(SideName, SideName), cx: &mut Context<Self>) -> Option<AnyElement> {
        let resolver = self.resolver()?;
        let path = resolver.path.as_str();
        let name = file_name(path);
        let (stacked, all) = (resolver.stacked, resolver.story_all);
        let column = |side: Side, commits: &[SideCommit], more: usize, cx: &mut Context<Self>| -> AnyElement {
            let title = if side == Side::Current { &names.0.title } else { &names.1.title };
            let shown = if all { commits.len() } else { commits.len().min(STORY_SHOWN) };
            let hidden = commits.len() - shown + more;
            let count = match commits.len() + more {
                0 => format!("no commit changed {name} since the sides split"),
                1 => format!("1 commit changed {name}"),
                n => format!("{n} commits changed {name}"),
            };
            let now = now();
            let rows: Vec<AnyElement> = commits[..shown]
                .iter()
                .enumerate()
                .map(|(i, commit)| {
                    let id = commit.id.clone();
                    div()
                        .id(SharedString::from(format!("story-{}-{i}", if side == Side::Current { "c" } else { "i" })))
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_1()
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(t().hover)))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_commit_id(&id, cx)))
                        .child(ui::avatar(&commit.author, &commit.email, self.avatar_for(&commit.email, cx), 16.))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .line_clamp(1)
                                .text_ellipsis()
                                .text_xs()
                                .text_color(rgb(t().text))
                                .child(SharedString::from(commit.summary.clone())),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(rgb(t().muted))
                                .child(format!("{} · {}", commit.author, ui::ago(now - commit.time))),
                        )
                        .into_any_element()
                })
                .collect();
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .child(div().flex_none().size(px(8.)).rounded_full().bg(rgb(side_color(side))))
                        .child(
                            div()
                                .min_w_0()
                                .flex_shrink()
                                .overflow_hidden()
                                .line_clamp(1)
                                .text_ellipsis()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(t().text_strong))
                                .child(SharedString::from(title.clone())),
                        )
                        .child(div().flex_none().text_color(rgb(t().muted)).child(count)),
                )
                .children(rows)
                .when(hidden > 0 || all && commits.len() > STORY_SHOWN, |col| {
                    col.child(
                        div()
                            .id(SharedString::from(format!("story-more-{}", if side == Side::Current { "c" } else { "i" })))
                            .pl(px(28.))
                            .text_xs()
                            .text_color(rgb(t().link))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_story_all(cx)))
                            .child(if all { "Show fewer".to_owned() } else { format!("{hidden} more") }),
                    )
                })
                .into_any_element()
        };
        let content: AnyElement = match &resolver.story {
            Phase::Loading => div().text_xs().text_color(rgb(t().muted)).child("Reading who changed this file on each side…").into_any_element(),
            Phase::Failed(message) => {
                div().text_xs().text_color(rgb(t().muted)).child(format!("Who changed each side is not known: {message}")).into_any_element()
            }
            Phase::Ready(story) => {
                let current = column(Side::Current, &story.current, story.more_current, cx);
                let incoming = column(Side::Incoming, &story.incoming, story.more_incoming, cx);
                // One under the other as plain blocks: in a flex column, text cut to one line is measured at no width.
                if stacked {
                    div().child(current).child(div().h(px(8.))).child(incoming).into_any_element()
                } else {
                    div().flex().gap(px(24.)).child(current).child(incoming).into_any_element()
                }
            }
        };
        Some(
            div()
                .debug_selector(|| "conflict-story".to_owned())
                .flex_none()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(rgb(t().border))
                .bg(rgb(t().panel))
                .child(content)
                .into_any_element(),
        )
    }

    /// The bar under the conflicts: the keys, and what can be done with the file.
    fn render_resolver_footer(&self, names: &(SideName, SideName), cx: &mut Context<Self>) -> AnyElement {
        let Some(resolver) = self.resolver() else { return div().into_any_element() };
        let blocks = resolver.blocks();
        let safe = resolver.safe_left();
        let decided = resolver.decided();
        let keys = format!(
            "N / P next, previous   1 {}   2 {}   3 both   0 base   ⌫ undo   ⌘↩ use result",
            clipped(&names.0.short, 14),
            clipped(&names.1.short, 14)
        );
        let primary = if blocks == 0 { "Mark resolved" } else { "Use this result" };
        let ready = blocks == 0 || decided > 0;
        let all = blocks > 0 && decided == blocks;
        let what = if blocks == 0 {
            String::new()
        } else if all {
            "writes the file and marks it resolved".to_owned()
        } else if decided > 0 {
            format!("writes the file; {} left marked", blocks - decided)
        } else {
            String::new()
        };
        div()
            .h(px(40.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .overflow_hidden()
            .border_t_1()
            .border_color(rgb(t().border))
            .bg(rgb(t().panel))
            .child(
                div()
                    .debug_selector(|| "conflict-keys".to_owned())
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .text_xs()
                    .text_color(rgb(t().muted))
                    .when(blocks > 0, |hint| hint.child(keys)),
            )
            .child(
                ghost("conflict-edit", "Edit in editor")
                    .debug_selector(|| "conflict-edit".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.edit_in_editor(cx))),
            )
            .child(
                ghost("conflict-reread", "Re-read")
                    .debug_selector(|| "conflict-reread".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.reread_conflict(cx))),
            )
            .child(
                ghost("conflict-start-over", "Start over")
                    .debug_selector(|| "conflict-start-over".to_owned())
                    .on_click(cx.listener(|this, _, window, cx| this.start_over(window, cx))),
            )
            .when(safe > 0, |bar| {
                bar.child(
                    button("conflict-safe", if safe == 1 { "Resolve the 1 safe one".to_owned() } else { format!("Resolve the {safe} safe ones") })
                        .debug_selector(|| "conflict-safe".to_owned())
                        .bg(rgb(theme::mix(t().element, t().added, 0.25)))
                        .text_color(rgb(t().text_strong))
                        .on_click(cx.listener(|this, _, _, cx| this.resolve_safe(cx))),
                )
            })
            .when(!what.is_empty(), |bar| bar.child(div().flex_none().text_xs().text_color(rgb(t().muted)).child(what)))
            .child(
                button("conflict-use", primary)
                    .debug_selector(|| "conflict-use".to_owned())
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(ready, |b| b.bg(rgb(t().accent)).text_color(rgb(t().on_accent)))
                    .when(!ready, |b| b.text_color(rgb(t().muted)).cursor_default())
                    .on_click(cx.listener(|this, _, _, cx| this.use_result(cx))),
            )
            .into_any_element()
    }

    /// One row of the conflict list.
    fn render_resolver_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let names = self.side_names();
        let Some(resolver) = self.resolver() else { return div().into_any_element() };
        let (Some(text), Some(row)) = (resolver.text(), resolver.rows.get(ix).copied()) else { return div().into_any_element() };
        let card = |block: usize| {
            let focused = resolver.focused == block;
            div()
                .w_full()
                .bg(rgb(t().card))
                .border_l_1()
                .border_r_1()
                .border_color(rgb(if focused { t().accent } else { t().border }))
        };
        // Every row of a card sits inside the same margins.
        let inset = |inner: gpui::Div| div().w_full().px_3().child(inner).into_any_element();
        // A card's lines wrap, and lose the indentation they share: what decides a conflict is never cut off.
        let indent = |block: usize| resolver.block_indents.get(block).map_or("", String::as_str);
        let wrapped = |line: StyledText, color: u32| {
            div()
                .min_w_0()
                .flex_1()
                .min_h(px(LINE_H))
                .px_3()
                .py(px(2.))
                .font_family(MONO)
                .text_xs()
                .line_height(px(LINE_H - 4.))
                .text_color(rgb(color))
                .child(line)
        };
        let code = |line: StyledText, color: u32| {
            div()
                .min_w_0()
                .flex_1()
                .h(px(LINE_H))
                .px_3()
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .font_family(MONO)
                .text_xs()
                .text_color(rgb(color))
                .child(line)
        };
        match row {
            Row::Note => {
                let note = resolver.note().unwrap_or_default();
                div()
                    .w_full()
                    .px_3()
                    .pt_3()
                    .child(
                        div()
                            .debug_selector(|| "conflict-note".to_owned())
                            .w_full()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(theme::mix(t().bg, t().warning, 0.10)))
                            .border_1()
                            .border_color(rgb(theme::mix(t().bg, t().warning, 0.30)))
                            .text_xs()
                            .text_color(rgb(t().text))
                            .child(note),
                    )
                    .into_any_element()
            }
            Row::Clean { segment, line } => {
                let Some(Segment::Clean(lines)) = text.segments.get(segment) else { return div().into_any_element() };
                // Quiet: the lines with no conflict are only there to say where the conflicts are.
                let spans = resolver.spans(segment, Side::Current, line);
                div().w_full().px_3().flex().opacity(0.6).child(code(styled(&lines[line], "", spans), t().editor_fg)).into_any_element()
            }
            Row::Fold { segment, from, to } => {
                let hidden = to - from;
                div()
                    .id(("conflict-fold", segment))
                    .debug_selector(move || format!("conflict-fold-{segment}"))
                    .w_full()
                    .h(px(22.))
                    .px_6()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(t().muted))
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgb(t().text_strong)).bg(rgb(t().hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.unfold(segment, cx)))
                    .child(format!("⋯  {hidden} unchanged line{}", if hidden == 1 { "" } else { "s" }))
                    .into_any_element()
            }
            Row::Head(block) => self.render_card_head(block, &names, cx),
            Row::Sides(block) => {
                let half = |side: Side| self.side_label(block, side, &names);
                inset(card(block).flex().child(half(Side::Current)).child(div().w(px(1.)).h_full().bg(rgb(t().border))).child(half(Side::Incoming)))
            }
            Row::Pair { block, line } => {
                let Some(conflict) = resolver.block(block) else { return div().into_any_element() };
                let segment = resolver.block_segments[block];
                let half = |side: Side| match conflict.side(side).get(line) {
                    Some(text) => {
                        wrapped(styled(text, indent(block), resolver.spans(segment, side, line)), t().editor_fg).bg(rgb(tint(side))).into_any_element()
                    }
                    None if line == 0 => wrapped(words("(nothing here: removed on this side)"), t().muted).italic().into_any_element(),
                    // The same padding as a full half, or the other half would take its room.
                    None => div().min_w_0().flex_1().px_3().into_any_element(),
                };
                inset(card(block).flex().child(half(Side::Current)).child(div().w(px(1.)).h_full().bg(rgb(t().border))).child(half(Side::Incoming)))
            }
            Row::Label { block, part } => {
                let label: AnyElement = match part {
                    Part::Current => self.side_label(block, Side::Current, &names),
                    Part::Incoming => self.side_label(block, Side::Incoming, &names),
                    Part::Base => {
                        let empty = resolver.block(block).is_some_and(|b| b.base.as_ref().is_some_and(Vec::is_empty));
                        div()
                            .h(px(LABEL_H))
                            .px_3()
                            .flex()
                            .items_center()
                            .text_xs()
                            .text_color(rgb(t().muted))
                            .child(if empty {
                                "Before either change: nothing was here, both sides added these lines"
                            } else {
                                "Before either change (what both sides started from)"
                            })
                            .into_any_element()
                    }
                };
                inset(card(block).flex().border_t_1().border_color(rgb(t().border)).child(label))
            }
            Row::Line { block, part, line } => {
                let Some(conflict) = resolver.block(block) else { return div().into_any_element() };
                let (lines, side): (&[String], Option<Side>) = match part {
                    Part::Current => (&conflict.current, Some(Side::Current)),
                    Part::Incoming => (&conflict.incoming, Some(Side::Incoming)),
                    Part::Base => (conflict.base.as_deref().unwrap_or(&[]), None),
                };
                let segment = resolver.block_segments[block];
                let cell = match (lines.get(line), side) {
                    (Some(text), Some(side)) => {
                        wrapped(styled(text, indent(block), resolver.spans(segment, side, line)), t().editor_fg).bg(rgb(tint(side)))
                    }
                    (Some(text), None) => wrapped(styled(text, indent(block), &[]), t().muted),
                    (None, _) => wrapped(words("(nothing here: removed on this side)"), t().muted).italic(),
                };
                inset(card(block).flex().child(cell))
            }
            Row::ResultHead(block) => self.render_result_head(block, &names, cx),
            Row::Result { block, line } => {
                let Some(conflict) = resolver.block(block) else { return div().into_any_element() };
                let Some(choice) = resolver.choices[block] else { return div().into_any_element() };
                let segment = resolver.block_segments[block];
                let spans = result_source(choice, conflict, line).map_or(&[][..], |(side, at)| resolver.spans(segment, side, at));
                let cell = match choice.line(conflict, line) {
                    Some((text, _)) => wrapped(styled(text, indent(block), spans), t().editor_fg),
                    None => wrapped(words("(nothing: these lines are removed)"), t().muted).italic(),
                };
                inset(card(block).flex().child(cell))
            }
            Row::Foot(block) => div()
                .w_full()
                .px_3()
                .pb_3()
                .child(card(block).h(px(8.)).rounded_b_md().border_b_1())
                .into_any_element(),
        }
    }

    /// A side's name in a card, with who last changed its lines there.
    fn side_label(&self, block: usize, side: Side, names: &(SideName, SideName)) -> AnyElement {
        let title = if side == Side::Current { &names.0.title } else { &names.1.title };
        let change: Option<&LastChange> = self.resolver().and_then(|r| r.changes.get(block)).and_then(|pair| pair[side as usize].as_ref());
        let now = now();
        div()
            .min_w_0()
            .flex_1()
            .h(px(LABEL_H))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .overflow_hidden()
            .text_xs()
            .child(div().flex_none().size(px(8.)).rounded_full().bg(rgb(side_color(side))))
            .child(
                div()
                    .flex_shrink()
                    .min_w(px(60.))
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(side_color(side)))
                    .child(SharedString::from(title.clone())),
            )
            .children(change.map(|change| {
                div()
                    .debug_selector(|| "conflict-last-change".to_owned())
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .text_color(rgb(t().muted))
                    .child(format!("{} · {} · {}", change.author, ui::ago(now - change.time), change.summary))
            }))
            .into_any_element()
    }

    /// The top of a card: which conflict, where it is, and the choices.
    fn render_card_head(&self, block: usize, names: &(SideName, SideName), cx: &mut Context<Self>) -> AnyElement {
        let Some(resolver) = self.resolver() else { return div().into_any_element() };
        let Some(conflict) = resolver.block(block) else { return div().into_any_element() };
        let chosen = resolver.choices[block];
        let focused = resolver.focused == block;
        let line = resolver.block_lines.get(block).copied().unwrap_or(1);
        let choice = |key: &'static str, label: String, this: Resolution, lit: bool, cx: &mut Context<Self>| {
            segment(SharedString::from(format!("choose-{block}-{key}")), label, lit)
                .debug_selector(move || format!("choose-{block}-{key}"))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    // Clicking the lit choice again takes it back; Both again swaps the order.
                    let next = match (chosen, this) {
                        (Some(Resolution::CurrentThenIncoming), Resolution::CurrentThenIncoming) => Some(Resolution::IncomingThenCurrent),
                        (Some(Resolution::IncomingThenCurrent), Resolution::CurrentThenIncoming) => Some(Resolution::CurrentThenIncoming),
                        (Some(now), _) if now == this => None,
                        _ => Some(this),
                    };
                    workspace.choose_block(block, next, false, cx);
                }))
        };
        let both = matches!(chosen, Some(Resolution::CurrentThenIncoming | Resolution::IncomingThenCurrent));
        let mut choices = vec![
            choice("current", clipped(&names.0.short, 18), Resolution::Current, chosen == Some(Resolution::Current), cx),
            choice("incoming", clipped(&names.1.short, 18), Resolution::Incoming, chosen == Some(Resolution::Incoming), cx),
            choice("both", "Both".into(), Resolution::CurrentThenIncoming, both, cx),
        ];
        if conflict.base.is_some() {
            choices.push(choice("base", "Neither".into(), Resolution::Base, chosen == Some(Resolution::Base), cx));
        }
        let mark = match chosen {
            Some(_) => div().flex_none().text_color(rgb(t().added)).child("✓"),
            None => div().flex_none().size(px(8.)).rounded_full().border_1().border_color(rgb(t().muted)),
        };
        div()
            .id(("conflict-card", block))
            .w_full()
            .px_3()
            .pt_1()
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(resolver) = this.resolver_mut() {
                    resolver.focused = block;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .debug_selector(move || format!("conflict-head-{block}"))
                    .w_full()
                    .h(px(HEAD_H))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_t_md()
                    .border_t_1()
                    .border_l_1()
                    .border_r_1()
                    .border_color(rgb(if focused { t().accent } else { t().border }))
                    .bg(rgb(if focused { theme::mix(t().card, t().accent, 0.06) } else { t().card }))
                    .child(mark)
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(t().text_strong))
                            .child(format!("Conflict {} of {}", block + 1, resolver.blocks())),
                    )
                    .child(div().min_w_0().flex_1().overflow_hidden().line_clamp(1).text_ellipsis().text_xs().text_color(rgb(t().muted)).child(format!("line {line}")))
                    .child(segmented(choices)),
            )
            .into_any_element()
    }

    /// The label over what the file will say: the choice and why, a suggestion to take, or that nothing is chosen.
    fn render_result_head(&self, block: usize, names: &(SideName, SideName), cx: &mut Context<Self>) -> AnyElement {
        let Some(resolver) = self.resolver() else { return div().into_any_element() };
        let chosen = resolver.choices[block];
        let verdict = resolver.verdicts.get(block).copied().flatten();
        let focused = resolver.focused == block;
        let row = div()
            .w_full()
            .h(px(LABEL_H))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .overflow_hidden()
            .bg(rgb(t().card))
            .border_t_1()
            .border_l_1()
            .border_r_1()
            .border_color(rgb(if focused { t().accent } else { t().border }))
            .text_xs();
        let content = match (chosen, verdict) {
            (Some(choice), _) => {
                let why = (resolver.auto[block]).then(|| verdict.map(|v| reason_text(v.reason, names))).flatten();
                let swap = matches!(choice, Resolution::CurrentThenIncoming | Resolution::IncomingThenCurrent);
                row.child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(rgb(t().added)).child("Result"))
                    .child(div().flex_none().text_color(rgb(t().text)).child(choice_text(choice, names)))
                    .when(swap, |row| {
                        let other = if choice == Resolution::CurrentThenIncoming { Resolution::IncomingThenCurrent } else { Resolution::CurrentThenIncoming };
                        row.child(
                            div()
                                .id(("conflict-swap", block))
                                .debug_selector(move || format!("conflict-swap-{block}"))
                                .flex_none()
                                .text_color(rgb(t().link))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| this.choose_block(block, Some(other), false, cx)))
                                .child("swap order"),
                        )
                    })
                    .children(why.map(|why| {
                        div()
                            .debug_selector(move || format!("conflict-why-{block}"))
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .line_clamp(1)
                            .text_ellipsis()
                            .text_color(rgb(t().muted))
                            .child(format!("· resolved safely: {why}"))
                    }))
            }
            (None, Some(verdict)) if !verdict.certain => row
                .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(rgb(t().warning)).child("Suggestion"))
                .child(
                    div()
                        .min_w_0()
                        .flex_shrink()
                        .overflow_hidden()
                        .line_clamp(1)
                        .text_ellipsis()
                        .text_color(rgb(t().text))
                        .child(format!("{}: keep both? Only you can say if the order is right.", reason_text(verdict.reason, names))),
                )
                .child(
                    button(("conflict-suggest", block), "Keep both")
                        .debug_selector(move || format!("conflict-suggest-{block}"))
                        .on_click(cx.listener(move |this, _, _, cx| this.choose_block(block, Some(verdict.resolution), false, cx))),
                ),
            (None, Some(verdict)) => row
                .child(div().flex_none().text_color(rgb(t().muted)).child("Not chosen yet"))
                .child(div().min_w_0().overflow_hidden().line_clamp(1).text_ellipsis().text_color(rgb(t().added)).child(format!("safe: {}", reason_text(verdict.reason, names)))),
            (None, None) => row.child(div().text_color(rgb(t().muted)).child("Not chosen yet: pick a side above")),
        };
        div().w_full().px_3().child(content).into_any_element()
    }

    /// A conflict that is not lines of text: one whole version is chosen, or the file is kept or deleted.
    fn render_whole_file(&self, kind: ConflictKind, names: &(SideName, SideName), cx: &mut Context<Self>) -> AnyElement {
        let Some(resolver) = self.resolver() else { return div().into_any_element() };
        let name = file_name(&resolver.path).to_owned();
        let path = resolver.path.clone();
        let named = |side: Side| if side == Side::Current { &names.0 } else { &names.1 };
        let primary = |id: &'static str, label: String| {
            button(id, label).debug_selector(move || id.to_owned()).bg(rgb(t().accent)).text_color(rgb(t().on_accent)).font_weight(FontWeight::SEMIBOLD)
        };
        let delete = |label: &'static str, cx: &mut Context<Self>| {
            let path = path.clone();
            button("conflict-delete", label)
                .debug_selector(|| "conflict-delete".to_owned())
                .on_click(cx.listener(move |this, _, window, cx| this.open_dialog(Action::DeleteConflicted(path.clone()), window, cx)))
        };
        let (title, body, buttons): (String, String, Vec<AnyElement>) = match kind {
            ConflictKind::Whole => (
                format!("{name} cannot be merged line by line"),
                "It is not text (a binary file or a link), so one whole version has to win.".to_owned(),
                vec![
                    primary("conflict-take-current", format!("Use {}'s version", clipped(&names.0.short, 24)))
                        .on_click(cx.listener(|this, _, _, cx| this.take_whole(Side::Current, cx)))
                        .into_any_element(),
                    primary("conflict-take-incoming", format!("Use {}'s version", clipped(&names.1.short, 24)))
                        .on_click(cx.listener(|this, _, _, cx| this.take_whole(Side::Incoming, cx)))
                        .into_any_element(),
                ],
            ),
            ConflictKind::Deleted { by } => {
                let kept = by.other();
                (
                    format!("{name} was deleted {}", named(by).place),
                    format!(
                        "It was changed {} since the sides split. Keep the changed file, or delete it as {} did.",
                        named(kept).place,
                        named(by).short
                    ),
                    vec![
                        primary("conflict-keep", format!("Keep the file ({}'s version)", clipped(&named(kept).short, 24)))
                            .on_click(cx.listener(move |this, _, _, cx| this.take_whole(kept, cx)))
                            .into_any_element(),
                        delete("Delete the file", cx).into_any_element(),
                    ],
                )
            }
            ConflictKind::OnlyOn(side) => (
                format!("Only {} has {name}", named(side).short),
                "Git could not pair it with a rename on the other side, so it asks whether it stays.".to_owned(),
                vec![
                    primary("conflict-keep", "Keep the file".into()).on_click(cx.listener(move |this, _, _, cx| this.take_whole(side, cx))).into_any_element(),
                    delete("Delete the file", cx).into_any_element(),
                ],
            ),
            ConflictKind::BothDeleted => (
                format!("{name} was deleted on both sides"),
                "Each side deleted it, or renamed it to a different name.".to_owned(),
                vec![delete("Mark it deleted", cx).into_any_element()],
            ),
            ConflictKind::Submodule => (
                format!("{name} is a submodule"),
                "Choose its commit in a terminal (check it out inside the submodule, then git add it here), then Re-read.".to_owned(),
                Vec::new(),
            ),
            ConflictKind::Broken { line } => (
                format!("The conflict markers in {name} are unfinished"),
                format!(
                    "Line {line} breaks them, probably an edit left half done. Fix it in your editor, then Re-read; or Start over to put \
                     back git's conflict."
                ),
                vec![
                    primary("conflict-edit-broken", "Edit in editor".into()).on_click(cx.listener(|this, _, _, cx| this.edit_in_editor(cx))).into_any_element(),
                    button("conflict-start-over-broken", "Start over")
                        .on_click(cx.listener(|this, _, window, cx| this.start_over(window, cx)))
                        .into_any_element(),
                ],
            ),
            ConflictKind::Text(_) => return div().into_any_element(),
        };
        let story = self.render_story(names, cx);
        let pictures = self.repo.as_ref().and_then(|repo| repo.file.as_ref()).and_then(|file| file.images.clone()).map(|images| {
            let side = |side: Side, picture: Option<&crate::preview::Preview>| {
                let title = if side == Side::Current { &names.0.title } else { &names.1.title };
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .child(div().flex_none().size(px(8.)).rounded_full().bg(rgb(side_color(side))))
                            .child(div().min_w_0().overflow_hidden().line_clamp(1).text_ellipsis().text_color(rgb(t().text_strong)).child(SharedString::from(title.clone())))
                            .children(picture.map(|p| {
                                div().flex_none().text_color(rgb(t().muted)).child(format!("{} × {} · {}", p.width, p.height, crate::preview::human_size(p.bytes)))
                            })),
                    )
                    .child(
                        div()
                            .h(px(160.))
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(t().border))
                            .bg(rgb(t().empty_bg))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(match picture {
                                Some(p) => gpui::img(p.source.clone()).size_full().object_fit(gpui::ObjectFit::ScaleDown).into_any_element(),
                                None => div().text_xs().text_color(rgb(t().muted)).child("not there").into_any_element(),
                            }),
                    )
            };
            div().debug_selector(|| "conflict-pictures".to_owned()).flex().gap_3().child(side(Side::Current, images.old.as_ref())).child(side(Side::Incoming, images.new.as_ref()))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .children(story)
            .child(
                div().flex_1().flex().items_start().justify_center().px_4().pt_8().child(
                    div()
                        .debug_selector(|| "conflict-whole".to_owned())
                        .w(px(560.))
                        .max_w_full()
                        .p_4()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(t().border))
                        .bg(rgb(t().card))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().text_base().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child(title))
                        .child(div().text_color(rgb(t().text)).child(body))
                        .children(pictures)
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .children(buttons)
                                .child(button("conflict-reread-whole", "Re-read").on_click(cx.listener(|this, _, _, cx| this.reread_conflict(cx)))),
                        ),
                ),
            )
            .into_any_element()
    }

    /// The bar over the main area while a merge, rebase or cherry-pick is in progress: what it is, how many files are
    /// left, and Continue (once none are) and Abort.
    pub(crate) fn render_operation_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let op = self.operation()?.clone();
        let left = self.conflicts_left();
        let word = op.operation.name();
        let open_is_conflicted = self.resolver().is_some_and(|r| self.repo.as_ref().is_some_and(|repo| repo.work.iter().any(|f| f.conflicted && f.change.path == r.path)));
        let ready = left == 0;
        let status = if ready {
            div().text_color(rgb(t().added)).child("every conflict is resolved")
        } else {
            div().text_color(rgb(t().warning)).child(format!("{left} file{} to resolve", if left == 1 { "" } else { "s" }))
        };
        let continue_label = format!("Continue {word}");
        let abort_label = format!("Abort {word}");
        let operation = op.operation;
        Some(
            div()
                .debug_selector(|| "operation-bar".to_owned())
                .flex_none()
                .h(px(36.))
                .px_3()
                .flex()
                .items_center()
                .gap_3()
                .overflow_hidden()
                .bg(rgb(theme::mix(t().bg, t().warning, 0.10)))
                .border_b_1()
                .border_color(rgb(theme::mix(t().bg, t().warning, 0.35)))
                .text_sm()
                .child(div().flex_none().size(px(8.)).rounded_full().bg(rgb(if ready { t().added } else { t().warning })))
                .child(
                    div()
                        .debug_selector(|| "operation-headline".to_owned())
                        .min_w_0()
                        .flex_shrink()
                        .overflow_hidden()
                        .line_clamp(1)
                        .text_ellipsis()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(t().text_strong))
                        .child(op.headline()),
                )
                .child(status.flex_none().text_xs())
                .child(div().flex_1())
                .when(left > 0 && !open_is_conflicted, |bar| {
                    bar.child(
                        button("operation-resolve", "Resolve next file")
                            .debug_selector(|| "operation-resolve".to_owned())
                            .on_click(cx.listener(|this, _, _, cx| this.open_first_conflict(cx))),
                    )
                })
                .when(operation != Operation::Merge, |bar| {
                    bar.child(
                        ghost("operation-skip", "Skip commit")
                            .debug_selector(|| "operation-skip".to_owned())
                            .on_click(cx.listener(move |this, _, window, cx| this.open_dialog(Action::SkipCommit(operation), window, cx))),
                    )
                })
                .child(
                    button("operation-abort", abort_label)
                        .debug_selector(|| "operation-abort".to_owned())
                        .on_click(cx.listener(move |this, _, window, cx| this.open_dialog(Action::AbortOperation(operation), window, cx))),
                )
                .child(
                    button("operation-continue", continue_label)
                        .debug_selector(|| "operation-continue".to_owned())
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(ready, |b| b.bg(rgb(t().accent)).text_color(rgb(t().on_accent)))
                        .when(!ready, |b| b.text_color(rgb(t().muted)).cursor_default())
                        .on_click(cx.listener(|this, _, _, cx| this.continue_operation(cx))),
                )
                .into_any_element(),
        )
    }

    /// The file pane for the working tree when no file is open: during an operation, where it stands.
    pub(crate) fn render_work_overview(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(op) = self.operation() else { return centered("Pick a changed file under the project in the sidebar.", t().muted) };
        let left = self.conflicts_left();
        let word = op.operation.name();
        let (title, body) = if left == 0 {
            ("Every conflict is resolved".to_owned(), format!("Continue the {word} to finish it, or look over the files first."))
        } else {
            (
                format!("{left} file{} to resolve", if left == 1 { "" } else { "s" }),
                "Each opens in the resolver: the two sides next to each other, who changed each and why, and what the file will say.".to_owned(),
            )
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .debug_selector(|| "work-overview".to_owned())
                    .max_w(px(460.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(div().text_base().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child(title))
                    .child(div().text_color(rgb(t().muted)).child(body))
                    .child(if left == 0 {
                        button("overview-continue", format!("Continue {word}"))
                            .bg(rgb(t().accent))
                            .text_color(rgb(t().on_accent))
                            .on_click(cx.listener(|this, _, _, cx| this.continue_operation(cx)))
                    } else {
                        button("overview-resolve", "Resolve the first file").on_click(cx.listener(|this, _, _, cx| this.open_first_conflict(cx)))
                    }),
            )
            .into_any_element()
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn centered(text: impl Into<SharedString>, color: u32) -> AnyElement {
    div().size_full().flex().items_center().justify_center().text_color(rgb(color)).child(text.into()).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> (SideName, SideName) {
        (
            SideName { title: "On main".into(), short: "main".into(), place: "on main".into() },
            SideName { title: "Coming in from feature".into(), short: "feature".into(), place: "on feature".into() },
        )
    }

    #[test]
    fn a_card_leaves_out_only_the_indentation_all_its_lines_share() {
        let lines = |text: &str| gitgui_core::conflict::split_lines(text);
        let block = Block { current: lines("        a(1);\n\n          b();\n"), base: Some(lines("        a(0);\n")), incoming: lines("\t  x\n") };
        assert_eq!(shared_indent(&block), "", "a tab against spaces shares nothing");
        let block = Block { current: lines("        a(1);\n\n          b();\n"), base: Some(lines("        a(0);\n")), incoming: lines("        c();\n") };
        assert_eq!(shared_indent(&block), "        ");
        assert_eq!(display_line("          b();\n", "        "), ("  b();".to_owned(), 8, 6, vec![]));
        assert_eq!(display_line("\n", "        ").0, "", "a blank line has nothing to lose");
        assert_eq!(display_line("\tx\ty\r\n", ""), ("    x    y".to_owned(), 0, 4, vec![0, 2]));
        assert_eq!(display_line(&"x".repeat(700), "").0.chars().count(), MOST_SHOWN + 1);
    }

    #[test]
    fn a_result_line_knows_which_side_it_came_from() {
        let lines = |text: &str| gitgui_core::conflict::split_lines(text);
        let block = Block { current: lines("a\nb\n"), base: None, incoming: lines("c\n") };
        assert_eq!(result_source(Resolution::CurrentThenIncoming, &block, 2), Some((Side::Incoming, 0)));
        assert_eq!(result_source(Resolution::IncomingThenCurrent, &block, 2), Some((Side::Current, 1)));
        assert_eq!(result_source(Resolution::Base, &block, 0), None);
    }

    #[test]
    fn reasons_and_choices_are_said_with_the_sides_names() {
        assert_eq!(reason_text(Reason::OnlySpacing(Side::Incoming), &names()), "feature only re-spaced these lines; took main's change");
        assert_eq!(reason_text(Reason::SameChange, &names()), "both sides made the same change, spaced differently; kept main's");
        assert_eq!(choice_text(Resolution::IncomingThenCurrent, &names()), "both, feature first");
        assert_eq!(clipped("feature/a-very-long-branch-name", 12), "feature/a-v…");
    }
}
