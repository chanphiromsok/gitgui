//! The app's state and actions: projects in the sidebar, the open repository, the selected commit,
//! the open file, and its comments.
//!
//! Git work runs on a background thread. Each result is checked against what is selected when it
//! arrives, so a slow answer for a commit you have already left is dropped, not shown.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gitgui_core::{
    Backend, Blame, BranchTip, Commit, CommitDetail, Evidence, FileChange, FileDiff, FileStatus, GitCli, Host, Layout, Lineage, LogOptions, People, WorkFile,
    MergeClue, OperationState, Query, REVEAL_STEP, Reveal, ScanCache, Scope, TreeRow, Upstream, WebRemote, Gap, fetch_due, filter_with_focus, matches_text, reveal,
    scan_inputs, stash_count, visible_rows, people, web_remote,
};
use gitgui_store::{Comment, DiffMode, FileLayout, GraphFaces, ReviewLayout, NewComment, Project, Settings, Store};
use gpui::{
    AnyElement, App, Context, CursorStyle, Entity, FontWeight, ListAlignment, ListState, MouseButton, MouseMoveEvent,
    PathPromptOptions, SharedString, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};

use crate::graph::{self, Entry};
use crate::layout;
use crate::menu::{Dialog, MenuState, Notice};
use crate::rows::{Anchor, DisplayRow, Mode, display_rows};
use crate::text_input::{TextInput, TextInputEvent};
use crate::ui::{self, button, ghost};
use crate::avatars::{self, Avatar, Avatars};
use crate::changes::WORKTREE;
use crate::icons::{self, IconTheme};
use crate::preview;
use crate::syntax;
use crate::theme::{self, Theme, t};

const LOAD_LIMIT: usize = 20_000;
const DIFF_CONTEXT: u32 = 3;
/// "Whole file": more context than any file has lines.
pub const WHOLE_FILE: u32 = 1_000_000;
/// A file bigger than this is not read to show more of the lines around the changes.
const REVEAL_MAX_BYTES: usize = 8 << 20;

pub enum Phase<T> {
    Loading,
    Failed(SharedString),
    Ready(T),
}

pub struct RepoView {
    /// The commits as read, newest first. The rows are made from these, and made again when the
    /// grouping, a fold, or the merge scan changes what to show. Shared with the last read, which a
    /// refresh that finds nothing new reuses.
    pub commits: Arc<Vec<Commit>>,
    pub changed: usize,
    pub entries: Vec<Entry>,
    pub graph_width: f32,
    /// How many merges only bring a trunk into a branch, and so can be left out of the graph.
    pub sync_count: usize,
    /// How many lanes of the widest row are folded into the last one drawn; 0 when none are.
    pub folded_lanes: usize,
    pub current_branch: Option<SharedString>,
    pub timing: SharedString,
    read: Duration,
    pub lineages: Vec<Lineage>,
    pub names: Vec<Option<String>>,
    /// The branch the current one was cut from, and how far apart they are.
    pub base: Option<graph::Base>,
    /// The repository's web home, for pull request and commit links.
    pub web: Option<WebRemote>,
    /// Who is who among the authors.
    pub people: Arc<People>,
    /// How many commits the filters leave in the graph.
    pub shown: usize,
    /// How many stashes there are, shown or not.
    pub stashes: usize,
    /// Rows the search found, in order.
    pub matches: Vec<usize>,
    /// Branches found to be merged already, once the scan has run.
    pub clues: Vec<MergeClue>,
    /// Branch name → the commit it pointed at when scanned.
    pub tips: HashMap<String, String>,
    /// The scan is still running.
    pub scanning: bool,
    /// Branches the scan ran out of time before checking.
    pub unchecked: usize,
    /// The branches and trunks the merge scan is about, as they were read.
    scan_inputs: ScanInputs,
    /// Each local branch's upstream and how far apart they are, as of the last fetch.
    pub upstreams: HashMap<String, Upstream>,
    /// The repository's remotes; none means there is nothing to fetch, pull or push.
    pub remotes: Vec<String>,
    /// A merge, rebase or cherry-pick that stopped and is waiting, with its sides named by meaning.
    pub operation: Option<OperationState>,
    /// The current branch was just rebased: where it was before, to go back to.
    pub rebased: Option<gitgui_core::Rebased>,
    /// Commits that make the same changes as another commit that is only on the other side (rebased or
    /// cherry-picked copies), each pointing at the other, once the search for them has run.
    pub twins: HashMap<String, String>,
}

/// The branches to check for merges and the trunks to check them against.
type ScanInputs = (Vec<BranchTip>, Vec<BranchTip>);
/// What reading a file for the diff view gives: its diff, the syntax colors, the pictures of an image, and the file's text.
type FileRead = (FileDiff, syntax::FileColors, Option<preview::Images>, Option<Arc<String>>);

/// The merge scan that is running, so a refresh that finds the same branches waits for it instead of
/// starting another, and one that finds different branches stops it.
struct RunningScan {
    path: PathBuf,
    inputs: ScanInputs,
    cancel: Arc<AtomicBool>,
}

impl RepoView {
    /// Where the current branch stands against its upstream; `None` on a detached HEAD.
    pub fn upstream(&self) -> Option<&Upstream> {
        let branch = self.current_branch.as_ref()?;
        Some(self.upstreams.get(branch.as_ref()).unwrap_or(&Upstream::None))
    }

    /// Tip commit of each squash-merged branch → the commit that carries its changes.
    fn squashed(&self) -> HashMap<String, String> {
        self.clues
            .iter()
            .filter(|clue| clue.evidence != Evidence::Contained)
            .filter_map(|clue| Some((self.tips.get(&clue.branch)?.clone(), clue.commit.clone()?)))
            .collect()
    }

    /// The branches the Focus view shows besides HEAD's: its upstream, and the branch it was cut from (the one the
    /// project says new work starts from, else the one its line forks off in the whole history), by local name.
    fn focus_names(&self, workflow_base: Option<&str>, whole: Option<&graph::Base>) -> Vec<String> {
        let tail = |name: &str| name.split_once('/').map_or(name.to_owned(), |(_, rest)| rest.to_owned());
        let mut names: Vec<String> = Vec::new();
        if let Some(branch) = self.current_branch.as_ref().map(|branch| branch.to_string()) {
            names.push(branch.clone());
            if let Some(upstream) = self.upstreams.get(&branch).and_then(Upstream::name) {
                names.push(tail(upstream));
            }
        }
        let base = workflow_base.map(str::to_owned).or_else(|| whole.map(|base| base.name.clone()));
        names.extend(base);
        names.sort();
        names.dedup();
        names
    }

    /// The pairs of tips whose two sides may hold the same changes under different ids: the current branch against
    /// its upstream and against the branch it was cut from, when each has commits the other lacks.
    fn twin_inputs(&self) -> Vec<(String, String)> {
        let tip_of = |name: &str| {
            self.commits.iter().find(|c| c.refs.iter().any(|r| r.name == name && matches!(r.kind, gitgui_core::RefKind::LocalBranch | gitgui_core::RefKind::RemoteBranch))).map(|c| c.id.clone())
        };
        let Some(branch) = self.current_branch.as_ref().map(|b| b.to_string()) else { return Vec::new() };
        let Some(head) = tip_of(&branch) else { return Vec::new() };
        let mut pairs = Vec::new();
        if let Some(Upstream::Tracking { name, ahead, behind, .. }) = self.upstreams.get(&branch)
            && *ahead > 0
            && *behind > 0
            && let Some(theirs) = tip_of(name)
        {
            pairs.push((head.clone(), theirs));
        }
        if let Some(base) = &self.base
            && base.ahead > 0
            && base.behind > 0
        {
            let tip = tip_of(&base.name).or_else(|| self.commits.iter().find(|c| c.refs.iter().any(|r| r.kind == gitgui_core::RefKind::RemoteBranch && r.name.split_once('/').is_some_and(|(_, n)| n == base.name))).map(|c| c.id.clone()));
            if let Some(tip) = tip {
                pairs.push((head, tip));
            }
        }
        pairs
    }

    /// Which commits are only here and which only on the remote, for the branches that track one that moved apart
    /// from them, and for the branch HEAD is on when it is not on a remote yet.
    fn divergence(&self) -> gitgui_core::Divergence {
        let tracked: Vec<(String, String)> = self
            .upstreams
            .iter()
            .filter_map(|(branch, upstream)| match upstream {
                Upstream::Tracking { name, ahead, behind, .. } if ahead + behind > 0 => Some((branch.clone(), name.clone())),
                _ => None,
            })
            .collect();
        let unpublished = self
            .current_branch
            .as_ref()
            .map(|branch| branch.to_string())
            .filter(|branch| !matches!(self.upstreams.get(branch), Some(Upstream::Tracking { .. })) && !self.remotes.is_empty());
        gitgui_core::divergence(&self.commits, &tracked, unpublished.as_deref())
    }

    /// Makes the rows again from the commits, narrowed by `filter`. `workflow_base` is the branch the project
    /// says new work starts from, if it says.
    fn rebuild(&mut self, settings: &Settings, collapsed: &HashSet<String>, filter: &GraphFilter, workflow_base: Option<&str>) {
        let started = Instant::now();
        let squashed = self.squashed();
        // Merges that only bring a trunk into a branch: marked, and left out unless asked for (one that a branch,
        // tag or HEAD is on stays: it is where that branch is).
        let sync: HashSet<String> = gitgui_core::sync_merges(&self.commits);
        let folded_sync: HashSet<String> = self
            .commits
            .iter()
            .filter(|commit| sync.contains(&commit.id) && commit.refs.is_empty())
            .map(|commit| commit.id.clone())
            .collect();
        self.sync_count = folded_sync.len();
        let folded_sync = if filter.show_sync { HashSet::new() } else { folded_sync };
        // Which commits a merge commit brought in as its second parent: a branch tip among them was merged, not just reached.
        let merged_in: HashSet<String> =
            self.commits.iter().filter(|c| c.is_merge()).flat_map(|c| c.parents.iter().skip(1).cloned()).collect();
        let hidden: HashSet<String> =
            if filter.hide_merged { self.clues.iter().map(|clue| clue.branch.clone()).collect() } else { HashSet::new() };
        // `author:` and `date:` in the search limit the graph itself, joining what is left (see `narrow`).
        let constraints = self.with_every_identity(filter.parsed().0);
        let narrowed =
            filter.scope != Scope::All
                || !hidden.is_empty()
                || filter.hide_stashes
                || !constraints.is_empty()
                || !folded_sync.is_empty()
                || filter.isolate.is_some();
        // Where the current branch stands against its base is counted over the whole history, not what is shown;
        // the Focus view also needs it to know which branch the base is.
        let whole = narrowed.then(|| {
            let none = HashSet::new();
            let options =
                graph::Options { changed: 0, group: false, squashed: &squashed, collapsed: &none, web: None, people: None, sync: &sync };
            graph::build_entries(&self.commits, &options)
        });
        let whole_base = whole.as_ref().map(|built| built.base.clone());
        let shown = if narrowed {
            let (scope, focus) = match (&filter.isolate, &whole) {
                (Some(branch), Some(whole)) => (Scope::Only, graph::with_its_base(branch, whole, &self.remotes)),
                _ if filter.scope == Scope::Focus => {
                    (Scope::Focus, self.focus_names(workflow_base, whole_base.as_ref().and_then(Option::as_ref)))
                }
                _ => (filter.scope, Vec::new()),
            };
            let base = filter_with_focus(&self.commits, scope, &hidden, !filter.hide_stashes, &focus);
            let base = gitgui_core::without_commits(&base, &folded_sync);
            if constraints.is_empty() { base } else { gitgui_core::narrow(&base, |commit| constraints.matches(commit)) }
        } else {
            Vec::new()
        };
        self.stashes = stash_count(&self.commits);
        let commits: &[Commit] = if narrowed { &shown } else { &self.commits };
        let built = graph::build_entries(
            commits,
            &graph::Options {
                changed: self.changed,
                group: settings.group_by_parent,
                squashed: &squashed,
                collapsed,
                web: self.web.as_ref(),
                people: Some(&self.people),
                sync: &sync,
            },
        );
        // Ahead and behind count against the whole history, not just what is shown.
        let base = whole_base.unwrap_or_else(|| built.base.clone());
        self.shown = commits.len();
        self.entries = built.entries;
        graph::apply_clues(&mut self.entries, &self.clues, &self.tips, &merged_in);
        let divergence = self.divergence();
        graph::apply_divergence(&mut self.entries, &divergence);
        graph::apply_twins(&mut self.entries, &self.twins);
        let density = graph::Density::from_settings(settings);
        self.graph_width = graph::graph_width(built.widest, density);
        // Counted as if they were all drawn: the button that unfolds them says how many there are either way.
        self.folded_lanes = graph::folded_lanes(built.widest, graph::LANE_CAP);
        self.lineages = built.lineages;
        self.names = built.names;
        self.base = base;
        let count = if narrowed {
            format!("{} of {} commits", self.shown, self.commits.len())
        } else {
            format!("{} commits", self.commits.len())
        };
        // The timings are for whoever is working on the app, not for the person reading history.
        self.timing = SharedString::from(if std::env::var_os("GITGUI_PERF").is_some() {
            format!(
                "{count} · {} lanes · read {:.0} ms · layout {:.1} ms",
                built.widest,
                self.read.as_secs_f64() * 1000.,
                started.elapsed().as_secs_f64() * 1000.,
            )
        } else {
            count
        });
        self.apply_search(filter);
    }

    /// An author asked for by name or email stands for the person, so every email they commit under counts.
    fn with_every_identity(&self, mut constraints: gitgui_core::Constraints) -> gitgui_core::Constraints {
        for asked in constraints.authors.clone() {
            for (person, emails) in self.people.everyone() {
                let is_them = person.name.to_lowercase().contains(&asked)
                    || emails.iter().any(|email| *email == asked || email.contains(asked.as_str()));
                if is_them {
                    for email in emails {
                        if !constraints.authors.iter().any(|a| a == email) {
                            constraints.authors.push(email.to_owned());
                        }
                    }
                }
            }
        }
        constraints
    }

    /// Marks the rows the search does not find, and lists the ones it does.
    fn apply_search(&mut self, filter: &GraphFilter) {
        let query = filter.query();
        let by_id: HashMap<&str, &Commit> = self.commits.iter().map(|c| (c.id.as_str(), c)).collect();
        let found = filter.found_now();
        self.matches.clear();
        for (ix, entry) in self.entries.iter_mut().enumerate() {
            let commit = entry.commit.as_deref();
            let hit = match &query {
                Query::None => None,
                Query::Text(text) => Some(commit.and_then(|id| by_id.get(id)).is_some_and(|c| matches_text(c, text))),
                // Until git has answered, nothing is dimmed.
                Query::Path(_) | Query::Code(_) => found.map(|ids| commit.is_some_and(|id| ids.contains(id))),
            };
            entry.search_miss = hit == Some(false);
            if hit == Some(true) {
                self.matches.push(ix);
            }
        }
    }
}

/// What the filter bar above the graph narrows it to.
#[derive(Clone, Debug, Default)]
pub struct GraphFilter {
    pub scope: Scope,
    /// Leave out branches the merge scan found already merged.
    pub hide_merged: bool,
    /// Leave out stashes.
    pub hide_stashes: bool,
    /// Show the merges that only bring a trunk into a branch; left out by default.
    pub show_sync: bool,
    /// One branch picked out of the graph (by the name on its label), shown with the branch it was cut from.
    pub isolate: Option<String>,
    /// The search box's text.
    pub search: String,
    /// Today, for `date:today` and `date:7d` in the search; set when the repository is read.
    pub today: gitgui_core::Day,
    /// What git found for a `path:` or `code:` search, and the text it was asked for.
    pub found: Option<(String, HashSet<String>)>,
    /// A `path:` or `code:` search is running.
    pub searching: bool,
}

impl GraphFilter {
    /// The search split into the author and date it limits the graph to, and the words that are left.
    pub fn parsed(&self) -> (gitgui_core::Constraints, String) {
        gitgui_core::parse_constraints(&self.search, self.today)
    }

    pub fn query(&self) -> Query {
        Query::parse(&self.parsed().1)
    }

    /// What git found for the search as it reads now; `None` when it has not been asked.
    fn found_now(&self) -> Option<&HashSet<String>> {
        let text = self.parsed().1;
        self.found.as_ref().filter(|(asked, _)| *asked == text.trim()).map(|(_, ids)| ids)
    }
}

pub struct CommitView {
    pub detail: CommitDetail,
    pub files: Vec<FileChange>,
    pub additions: u32,
    pub deletions: u32,
}

pub struct CommitState {
    pub id: String,
    pub phase: Phase<CommitView>,
}

pub struct FileState {
    /// Index into the commit's files.
    pub index: usize,
    pub phase: Phase<()>,
    /// What is shown: `raw` with the unchanged lines `reveals` ask for put in.
    pub diff: FileDiff,
    /// The diff as git gave it.
    pub raw: FileDiff,
    /// The file after the change, for showing more of the lines around the changes; none when it is too big or not text.
    pub text: Option<Arc<String>>,
    /// How much is shown of each gap of `raw`.
    pub reveals: Vec<Reveal>,
    /// For each hunk of `diff`, the gap over it that still has lines hidden; and the one after the last hunk.
    pub above: Vec<Option<Gap>>,
    pub below: Option<Gap>,
    /// Syntax colors for the diff's lines; empty until read, or for a language not known.
    pub colors: syntax::FileColors,
    /// The picture before and after, for an image file.
    pub images: Option<preview::Images>,
    pub comments: Vec<Comment>,
    pub composing: Option<Anchor>,
    pub rows: Vec<DisplayRow>,
    pub list: ListState,
    /// Unchanged lines shown around each change; more after "Show more lines".
    pub context: u32,
    /// Where the changes are, for the strip beside the diff.
    pub minimap: crate::minimap::Minimap,
    /// Where that strip was drawn last, so a click on it knows which row it means.
    pub minimap_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    /// The longest line, in columns (a tab is four), to make the diff wide enough to scroll sideways.
    pub max_cols: usize,
    /// How far the code is scrolled sideways, and the widths it was last laid out with.
    pub scroll_x: std::cell::Cell<f32>,
    pub content_w: std::cell::Cell<f32>,
    pub diff_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    /// The file is new or deleted, so there is only one side to show: it is drawn as one column whatever
    /// the diff mode is, instead of beside an empty half.
    pub single_column: bool,
    /// Who last changed each line, read the first time the pointer rests on one.
    pub blame: BlameState,
    /// A file with conflicts opens in the resolver instead of as a diff.
    pub resolver: Option<Box<crate::resolver::Resolver>>,
}

/// Blame for the open file: not asked for until a line is pointed at, then read in the background.
pub enum BlameState {
    NotAsked,
    Loading,
    /// The file as of the commit (for added and unchanged lines), and as of its parent (for removed lines).
    Ready(Option<Arc<Blame>>, Option<Arc<Blame>>),
    Failed,
}

impl FileState {
    pub(crate) fn new(index: usize) -> Self {
        Self {
            index,
            phase: Phase::Loading,
            diff: FileDiff::default(),
            raw: FileDiff::default(),
            text: None,
            reveals: Vec::new(),
            above: Vec::new(),
            below: None,
            colors: syntax::FileColors::default(),
            images: None,
            comments: Vec::new(),
            composing: None,
            rows: Vec::new(),
            // Rows this far past the edge are built ahead of the scroll; more only costs frame time.
            list: ListState::new(0, ListAlignment::Top, px(120.)),
            context: DIFF_CONTEXT,
            minimap: Default::default(),
            minimap_bounds: Default::default(),
            max_cols: 0,
            scroll_x: Default::default(),
            content_w: Default::default(),
            diff_bounds: Default::default(),
            single_column: false,
            blame: BlameState::NotAsked,
            resolver: None,
        }
    }

    /// How this file is laid out: the mode asked for, except one column for a new or deleted file.
    pub fn mode(&self, wanted: Mode) -> Mode {
        if self.single_column { Mode::Unified } else { wanted }
    }

    /// Puts `raw` and what `reveals` ask to be shown of the unchanged lines together as `diff`.
    pub fn apply_reveals(&mut self) {
        match self.text.as_deref().filter(|_| !self.single_column) {
            Some(text) => {
                let shown = reveal(&self.raw, text, &self.reveals);
                (self.diff, self.above, self.below) = (shown.diff, shown.above, shown.below);
            }
            None => {
                self.diff = self.raw.clone();
                self.above = vec![None; self.diff.hunks.len()];
                self.below = None;
            }
        }
    }

    /// Shows `REVEAL_STEP` more unchanged lines in gap `gap`: over the hunk after it (`up`), or under the one before it.
    /// The view stays where it is: lines shown over a hunk come in under its header, which does not move, and lines shown
    /// under the hunk before it come in above the header, which goes down with them.
    pub fn show_more(&mut self, mode: Mode, gap: usize, up: bool) {
        if self.reveals.len() <= gap {
            self.reveals.resize(gap + 1, Reveal::default());
        }
        let step = &mut self.reveals[gap];
        if up {
            step.above += REVEAL_STEP;
        } else {
            step.below += REVEAL_STEP;
        }
        self.apply_reveals();
        self.rebuild(mode, true);
    }

    /// Rebuilds the rows from the diff, comments and composer. Scroll stays where it was unless
    /// the whole layout changed (a different mode).
    pub fn rebuild(&mut self, mode: Mode, keep_scroll: bool) {
        let top = self.list.logical_scroll_top();
        self.rows = display_rows(&self.diff, self.mode(mode), &self.comments, self.composing);
        if self.below.is_some() {
            self.rows.push(DisplayRow::Tail);
        }
        self.minimap = crate::minimap::build(&self.diff, &self.rows, crate::diff_view::LINE_H);
        self.max_cols = self
            .diff
            .hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter())
            .map(|line| line.text.chars().count() + 3 * line.text.matches('\t').count())
            .max()
            .unwrap_or(0);
        self.list.reset(self.rows.len());
        if keep_scroll {
            self.list.scroll_to(top);
        }
    }
}

pub struct RepoState {
    pub project: Project,
    pub phase: Phase<RepoView>,
    /// Index into the entries of the selected commit row.
    pub selected: Option<usize>,
    pub commit: Option<CommitState>,
    pub file: Option<FileState>,
    /// Unified or split diff.
    pub mode: Mode,
    /// The file pane fills the whole area and the graph is hidden.
    pub expanded: bool,
    /// The file list is shown as a tree or as a flat list.
    pub layout: Layout,
    /// The file list column is showing.
    pub files_visible: bool,
    pub filter: String,
    /// The rows of the file list for the selected commit, after the filter.
    pub file_rows: Vec<TreeRow>,
    /// Commits whose group of commits is folded away.
    pub collapsed: HashSet<String>,
    /// Folders folded in the changed-files tree, by path. Kept from commit to commit.
    pub folded_dirs: HashSet<String>,
    /// What has changed in the working tree, staged first.
    pub work: Vec<WorkFile>,
    /// Which load this is, so a slow answer for an earlier one is ignored.
    pub generation: u64,
    pub graph_filter: GraphFilter,
}

impl RepoState {
    fn loading(project: Project, generation: u64, settings: &Settings) -> Self {
        Self {
            collapsed: HashSet::new(),
            folded_dirs: HashSet::new(),
            work: Vec::new(),
            generation,
            graph_filter: GraphFilter::default(),
            project,
            phase: Phase::Loading,
            selected: None,
            commit: None,
            file: None,
            mode: mode_of(settings.diff_mode),
            expanded: false,
            layout: layout_of(settings.file_layout),
            files_visible: true,
            filter: String::new(),
            file_rows: Vec::new(),
        }
    }

    /// Makes the graph rows again and keeps the same commit selected if it is still shown.
    pub(crate) fn rebuild(&mut self, settings: &Settings) {
        let selected = self.selected_id();
        if let Phase::Ready(view) = &mut self.phase {
            view.rebuild(settings, &self.collapsed, &self.graph_filter, self.project.workflow.as_ref().and_then(|w| w.base.as_deref()));
            self.selected = selected.and_then(|id| view.entries.iter().position(|e| e.commit.as_deref() == Some(id.as_str())));
        }
    }

    /// The id of the selected commit row.
    pub fn selected_id(&self) -> Option<String> {
        match (&self.phase, self.selected) {
            (Phase::Ready(view), Some(ix)) => view.entries.get(ix)?.commit.clone(),
            _ => None,
        }
    }

    /// Rebuilds the file list rows from the selected commit's files, the filter and the layout.
    pub(crate) fn refresh_file_rows(&mut self) {
        let rows = match self.commit.as_ref().map(|commit| &commit.phase) {
            Some(Phase::Ready(view)) => visible_rows(&view.files, &self.filter, self.layout, &self.folded_dirs),
            _ => Vec::new(),
        };
        self.file_rows = rows;
    }
}

/// A divider being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Splitter {
    Sidebar,
    Pane,
    /// Between the changed-files column and the diff.
    Files,
    /// Between the graph and the file pane below it.
    PaneHeight,
}

/// A panel that can be hidden and still reached: pointing at its edge slides it back over the content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Sidebar,
    Graph,
    Files,
}

/// How wide the changed-files column starts, and the least and most it may be dragged to.
pub const FILES_WIDTH: f32 = 280.;
pub const FILES_MIN: f32 = 150.;
pub const FILES_MAX: f32 = 520.;
/// With the pane below the graph: its share of the height to start with, and what each keeps at least.
pub const PANE_HEIGHT_SHARE: f32 = 0.6;
pub const PANE_HEIGHT_MIN: f32 = 180.;
pub const GRAPH_HEIGHT_MIN: f32 = 160.;
/// About what the banner at the bottom takes while it shows, and the bar over the main area while an operation is in
/// progress.
const NOTICE_HEIGHT: f32 = 36.;
const OPERATION_BAR_HEIGHT: f32 = 36.;
/// How often, while fetching on its own is on, the app looks whether the open project is due a fetch.
pub(crate) const AUTO_FETCH_TICK: Duration = Duration::from_secs(30);

gpui::actions!(workspace, [PreviousFile, NextFile]);

pub struct Workspace {
    /// The window's own keyboard focus: a click outside a text field comes back here, so keys like
    /// Up and Down reach the window instead of the field typed in last.
    focus: gpui::FocusHandle,
    pub(crate) store: Option<Store>,
    pub projects: Vec<Project>,
    pub notice: Option<Notice>,
    pub repo: Option<RepoState>,
    pub input: Entity<TextInput>,
    pub filter_input: Entity<TextInput>,
    /// The search box above the graph.
    pub search_input: Entity<TextInput>,
    /// The commit message, under the open project's changes.
    pub commit_input: Entity<TextInput>,
    /// The text field in the rename / new branch / new tag dialog.
    pub dialog_input: Entity<TextInput>,
    pub settings: Settings,
    pub settings_open: bool,
    /// The diff line the pointer is on: its row, and which side of it (0 whole row, 1 left half, 2 right half).
    pub hover_line: Option<(usize, u8)>,
    /// The hidden panel that is showing over the content because the pointer is at its edge.
    pub peek: Option<Panel>,
    /// The graph is hidden while a commit is open, leaving the file pane the whole width.
    pub graph_hidden: bool,
    /// The changed-files column's width, and where it was last drawn (for dragging its divider).
    pub files_width: f32,
    pub files_left: std::rc::Rc<std::cell::Cell<f32>>,
    /// With the pane below the graph: the height it was dragged to.
    pub pane_height: Option<f32>,
    /// The page of the settings window that is showing.
    pub settings_page: crate::settings_view::SettingsPage,
    /// Every color theme found, built in first.
    pub themes: Vec<Arc<Theme>>,
    /// Icon themes from Zed's extensions; the built-in icons are not in the list.
    pub icon_themes: Vec<Arc<IconTheme>>,
    /// Authors' pictures, fetched as rows that show them are drawn.
    avatars: std::cell::RefCell<Avatars>,
    /// For each author's email, a commit of theirs on GitHub to ask who made it, when Gravatar has no picture.
    avatar_hints: std::cell::RefCell<HashMap<String, avatars::Hint>>,
    pub menu: Option<MenuState>,
    pub dialog: Option<Dialog>,
    /// The "New branch" window, while it is open, and what is typed in it.
    pub new_branch: Option<crate::workflow_ui::NewBranch>,
    pub branch_ticket: Entity<TextInput>,
    pub branch_title: Entity<TextInput>,
    /// Asks the next draw to give the title the keyboard (after "Enter" in the ticket).
    pub branch_focus_title: std::cell::Cell<bool>,
    /// The list of branches to start from, while it is open on top of the "New branch" window.
    pub branch_picker: Option<crate::workflow_ui::BranchPicker>,
    pub branch_search: Entity<TextInput>,
    pub(crate) branch_pick_scroll: gpui::ScrollHandle,
    /// Asks the next draw to bring the highlighted branch into view.
    pub(crate) branch_pick_reveal: std::cell::Cell<bool>,
    /// What is being done right now, while a git operation runs.
    pub busy: Option<SharedString>,
    /// The key to the graph's marks is open.
    pub legend_open: bool,
    /// The branch line the pointer is on in the graph.
    pub graph_hover: Option<usize>,
    /// The commit that is a copy of the one the pointer is on, to light.
    pub twin_hover: Option<String>,
    /// How many folders are being looked at to be added as projects.
    opening: usize,
    loads: u64,
    graph_scroll: UniformListScrollHandle,
    /// The sidebar's width as last dragged; the window may allow less (see `layout`).
    pub sidebar_width: f32,
    /// The file pane's width as last dragged; `None` until it is, then it takes a share of the room.
    pub pane_width: Option<f32>,
    pub resizing: Option<Splitter>,
    /// What each project's merge scans found, so the next scan asks git only about what has moved.
    scan_caches: HashMap<PathBuf, Arc<ScanCache>>,
    running_scan: Option<RunningScan>,
    /// A read of the repository is running. Reads asked for meanwhile become one more read after it.
    reading: bool,
    reread: bool,
    /// Each file and each commit opened takes the next number; a background read for one that is no
    /// longer open stops before doing more work.
    file_ticket: Arc<AtomicU64>,
    commit_ticket: Arc<AtomicU64>,
    last_log: Option<LastLog>,
    /// The pictures the file pane drew last; each is handed back to the window once it is not shown.
    shown_pictures: Vec<Arc<gpui::RenderImage>>,
    /// How much background work has actually run.
    pub(crate) counts: Arc<Counts>,
    /// Wakes now and then to fetch the open project, while that is on; dropping it stops it.
    auto_fetch_timer: Option<gpui::Task<()>>,
    /// A fetch the app started on its own is running.
    pub auto_fetching: bool,
    /// When each project was last fetched, by hand or on its own, by the executor's clock.
    fetched_at: HashMap<PathBuf, Instant>,
    /// Fetches on its own that failed in a row, and whether the banner has said so.
    auto_fetch_failures: u32,
    auto_fetch_warned: bool,
    /// An operation just stopped on conflicts: the next read of the repository opens the first file with conflicts.
    pub(crate) open_next_conflict: bool,
    /// The graph was hidden to make room for resolving conflicts, and comes back when the operation ends.
    pub(crate) graph_hidden_for_conflicts: bool,
}

/// Background work done so far, counted so tests can tell work that ran from work that was skipped.
#[derive(Default)]
pub(crate) struct Counts {
    pub repo_reads: AtomicUsize,
    pub scans: AtomicUsize,
    pub commit_reads: AtomicUsize,
    pub file_reads: AtomicUsize,
    /// Fetches the app started on its own.
    pub fetches: AtomicUsize,
}

impl Drop for Workspace {
    /// A closed window leaves its merge scan nobody to report to.
    fn drop(&mut self) {
        self.stop_merge_scan();
    }
}

/// Whether the background read that took `ticket` is still for what is open.
fn current(latest: &AtomicU64, ticket: u64) -> bool {
    latest.load(Ordering::Relaxed) == ticket
}

/// The commits as last read, to reuse when git prints the same log again.
struct LastLog {
    path: PathBuf,
    fingerprint: u64,
    commits: Arc<Vec<Commit>>,
    people: Arc<People>,
}

struct RepoData {
    /// The log's fingerprint, and the commits: `None` when they are the same as last time.
    fingerprint: u64,
    commits: Option<Vec<Commit>>,
    current_branch: Option<String>,
    changed: usize,
    read: Duration,
    /// A merge, rebase or cherry-pick that stopped (in this session or an earlier one) and is waiting.
    operation: Option<OperationState>,
    /// The repository's web home, from its remote, for pull request and commit links.
    web: Option<WebRemote>,
    work: Vec<WorkFile>,
    /// The day it is here, for `date:today` in the search.
    today: gitgui_core::Day,
    /// Each local branch's upstream, from one `git for-each-ref`.
    upstreams: HashMap<String, Upstream>,
    remotes: Vec<String>,
    /// The current branch's last move was a finished rebase, and its old tip is still there.
    rebased: Option<gitgui_core::Rebased>,
}

/// Reads the repository; the commits are left unparsed (`None`) when the log is the one `unchanged`
/// fingerprints.
/// For each author's email, remembers one of their commits on GitHub, so a picture Gravatar does not
/// have can be asked of GitHub (see `avatars`). Only github.com repositories count.
fn note_avatar_hints(
    hints: &std::cell::RefCell<HashMap<String, avatars::Hint>>,
    commits: &[Commit],
    web: Option<&WebRemote>,
) {
    let Some(repo) = web.filter(|web| web.host == Host::GitHub).and_then(|web| web.base.strip_prefix("https://github.com/")) else {
        return;
    };
    let mut hints = hints.borrow_mut();
    for commit in commits.iter().filter(|c| c.stash.is_none()) {
        hints
            .entry(commit.email.trim().to_ascii_lowercase())
            .or_insert_with(|| avatars::Hint { repo: repo.to_owned(), sha: commit.id.clone() });
    }
}

fn read_repo(path: &Path, unchanged: Option<u64>) -> Result<RepoData, gitgui_core::Error> {
    let git = GitCli::new(path);
    let started = Instant::now();
    let (fingerprint, commits) = git.log_if_changed(&LogOptions { max_count: Some(LOAD_LIMIT), skip: 0 }, unchanged)?;
    let current_branch = git.current_branch()?;
    let changed = git.changed_files()?;
    let operation = git.operation_state();
    let web = git.remote_url().ok().flatten().and_then(|url| web_remote(&url));
    let work = git.work_status().unwrap_or_default();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let today = gitgui_core::day_of(now, git.utc_offset());
    // Nice to know, not worth failing the read over.
    let upstreams = git.branch_sync().unwrap_or_default().into_iter().map(|b| (b.branch, b.upstream)).collect();
    let remotes = git.remotes().unwrap_or_default();
    let rebased = current_branch.as_deref().and_then(|branch| git.last_rebase(branch));
    Ok(RepoData { fingerprint, commits, current_branch, changed, read: started.elapsed(), operation, web, work, today, upstreams, remotes, rebased })
}

/// A commit's record and the files it changed; `None` when it stopped early because `wanted` said the
/// commit is no longer the one open.
fn read_commit(path: &Path, id: &str, wanted: impl Fn() -> bool) -> Option<Result<CommitView, gitgui_core::Error>> {
    let git = GitCli::new(path);
    if !wanted() {
        return None;
    }
    let detail = match git.commit_detail(id) {
        Ok(detail) => detail,
        Err(err) => return Some(Err(err)),
    };
    if !wanted() {
        return None;
    }
    let files = match git.commit_files(id) {
        Ok(files) => files,
        Err(err) => return Some(Err(err)),
    };
    let additions = files.iter().filter_map(|f| f.additions).sum();
    let deletions = files.iter().filter_map(|f| f.deletions).sum();
    Some(Ok(CommitView { detail, files, additions, deletions }))
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let this = Self::with_store(Store::open_default(), cx);
        // The graph's look is process-wide, like the color theme; only the app itself sets it from the saved choice.
        crate::graph_style::set_active(&this.settings.graph_style);
        this
    }

    /// Like `new`, with the store given: tests pass one that points at a scratch folder.
    pub fn with_store(store: Result<Store, gitgui_store::Error>, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Write a comment…  (Enter to save, Esc to cancel)", cx));
        cx.subscribe(&input, |this, _input, event: &TextInputEvent, cx| match event {
            TextInputEvent::Submit => this.submit_comment(cx),
            TextInputEvent::Cancel => this.cancel_comment(cx),
            TextInputEvent::Changed => {}
        })
        .detach();

        let filter_input = cx.new(|cx| TextInput::new("Filter files…", cx));
        cx.subscribe(&filter_input, |this, input, event: &TextInputEvent, cx| match event {
            TextInputEvent::Changed => {
                let text = input.read(cx).text().to_owned();
                this.set_filter(text, cx);
            }
            TextInputEvent::Cancel => input.update(cx, |input, cx| input.clear(cx)),
            TextInputEvent::Submit => {}
        })
        .detach();

        let search_input = cx.new(|cx| TextInput::new("Search commits…   path:file   code:text", cx));
        cx.subscribe(&search_input, |this, input, event: &TextInputEvent, cx| match event {
            TextInputEvent::Changed => {
                let text = input.read(cx).text().to_owned();
                this.set_search(text, cx);
            }
            TextInputEvent::Submit => this.submit_search(cx),
            TextInputEvent::Cancel => input.update(cx, |input, cx| input.clear(cx)),
        })
        .detach();

        let commit_input = cx.new(|cx| TextInput::new("Commit message (Enter to commit)", cx));
        cx.subscribe(&commit_input, |this, _input, event: &TextInputEvent, cx| match event {
            TextInputEvent::Submit => this.commit_work(cx),
            TextInputEvent::Changed => cx.notify(),
            TextInputEvent::Cancel => {}
        })
        .detach();

        let dialog_input = cx.new(|cx| TextInput::new("Type a name…", cx));
        cx.subscribe(&dialog_input, |this, _input, event: &TextInputEvent, cx| match event {
            TextInputEvent::Submit => this.confirm_dialog(cx),
            TextInputEvent::Cancel => this.cancel_dialog(cx),
            // The hint under a branch name follows what is typed.
            TextInputEvent::Changed => cx.notify(),
        })
        .detach();

        let branch_ticket = cx.new(|cx| TextInput::new("e.g. 74 or ABC-123 (optional)", cx));
        cx.subscribe(&branch_ticket, |this, _input, event: &TextInputEvent, cx| match event {
            // "Enter" after the ticket goes on to the title.
            TextInputEvent::Submit => {
                this.branch_focus_title.set(true);
                cx.notify();
            }
            TextInputEvent::Cancel => this.close_new_branch(cx),
            TextInputEvent::Changed => cx.notify(),
        })
        .detach();
        let branch_title = cx.new(|cx| TextInput::new("What is the work? e.g. driver reporting", cx));
        cx.subscribe(&branch_title, |this, _input, event: &TextInputEvent, cx| match event {
            TextInputEvent::Submit => this.create_new_branch(cx),
            TextInputEvent::Cancel => this.close_new_branch(cx),
            TextInputEvent::Changed => cx.notify(),
        })
        .detach();

        let branch_search = cx.new(|cx| TextInput::new("Search branches (Up, Down and Enter work too)", cx));
        cx.subscribe(&branch_search, |this, _input, event: &TextInputEvent, cx| match event {
            TextInputEvent::Submit => this.pick_highlighted_branch(cx),
            TextInputEvent::Cancel => this.close_branch_picker(cx),
            TextInputEvent::Changed => this.search_branches_changed(cx),
        })
        .detach();

        let (store, mut notice) = match store {
            Ok(store) => (Some(store), None),
            Err(err) => (None, Some(format!("Projects and comments will not be saved: {err}"))),
        };
        let projects = match store.as_ref().map(Store::projects) {
            Some(Ok(projects)) => projects,
            Some(Err(err)) => {
                notice = Some(err.to_string());
                Vec::new()
            }
            None => Vec::new(),
        };
        let settings = match store.as_ref().map(Store::settings) {
            Some(Ok(settings)) => settings,
            Some(Err(err)) => {
                notice = Some(err.to_string());
                Settings::default()
            }
            None => Settings::default(),
        };
        let themes = theme::all(store.as_ref().map(Store::dir));
        let chosen = settings.theme.as_deref().unwrap_or(theme::DEFAULT);
        if let Some(found) = themes.iter().find(|t| t.name == chosen).or(themes.first()) {
            theme::set(found.clone());
        }
        let icon_themes = icons::all();
        icons::set(settings.icon_theme.as_ref().and_then(|name| icon_themes.iter().find(|t| &t.name == name).cloned()));
        let avatars = std::cell::RefCell::new(Avatars::new(store.as_ref().map(|s| s.dir().join("avatars"))));
        let mut this = Self {
            focus: cx.focus_handle(),
            themes,
            icon_themes,
            avatars,
            avatar_hints: Default::default(),
            store,
            projects,
            notice: notice.map(Notice::warn),
            repo: None,
            input,
            filter_input,
            search_input,
            commit_input,
            dialog_input,
            new_branch: None,
            branch_ticket,
            branch_title,
            branch_focus_title: Default::default(),
            branch_picker: None,
            branch_search,
            branch_pick_scroll: gpui::ScrollHandle::new(),
            branch_pick_reveal: Default::default(),
            settings,
            settings_open: false,
            hover_line: None,
            peek: None,
            graph_hidden: false,
            files_width: FILES_WIDTH,
            files_left: Default::default(),
            pane_height: None,
            settings_page: crate::settings_view::SettingsPage::Graph,
            menu: None,
            dialog: None,
            busy: None,
            legend_open: false,
            graph_hover: None,
            twin_hover: None,
            opening: 0,
            loads: 0,
            graph_scroll: UniformListScrollHandle::new(),
            sidebar_width: layout::SIDEBAR_DEFAULT,
            pane_width: None,
            resizing: None,
            scan_caches: HashMap::new(),
            running_scan: None,
            reading: false,
            reread: false,
            file_ticket: Arc::new(AtomicU64::new(0)),
            commit_ticket: Arc::new(AtomicU64::new(0)),
            last_log: None,
            shown_pictures: Vec::new(),
            counts: Arc::default(),
            auto_fetch_timer: None,
            auto_fetching: false,
            fetched_at: HashMap::new(),
            auto_fetch_failures: 0,
            auto_fetch_warned: false,
            open_next_conflict: false,
            graph_hidden_for_conflicts: false,
        };
        this.schedule_auto_fetch(cx);
        this.reopen_last_project(cx);
        this
    }

    /// Opens the project that was open when the app was last closed, if it is still in the list and
    /// its folder is still there.
    fn reopen_last_project(&mut self, cx: &mut Context<Self>) {
        let Some(last) = self.settings.last_project.clone() else { return };
        if last.is_dir() && self.projects.iter().any(|p| p.path == last) {
            self.select_project(last, cx);
        }
    }

    // ---- resizing ---------------------------------------------------------------------------

    pub fn begin_resize(&mut self, which: Splitter, cx: &mut Context<Self>) {
        self.resizing = Some(which);
        cx.notify();
    }

    pub fn end_resize(&mut self, cx: &mut Context<Self>) {
        if let Some(which) = self.resizing.take() {
            if which == Splitter::Sidebar {
                // Dragged shut or open: keep that, as the button does.
                self.save_settings();
            }
            cx.notify();
        }
    }

    /// Follows the pointer while a divider is held. A move with no button down means the button was
    /// released somewhere we were not told about (outside the window), so the drag ends.
    pub fn drag_divider(&mut self, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        let Some(which) = self.resizing else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            return self.end_resize(cx);
        }
        let total = f32::from(window.viewport_size().width);
        let x = f32::from(event.position.x);
        let pane_open = self.repo.as_ref().is_some_and(|repo| repo.commit.is_some());
        match which {
            Splitter::Sidebar => {
                // Far enough left hides it; dragging back out shows it again, from its smallest width.
                let hide = x < layout::SIDEBAR_HIDE_AT && self.repo.is_some();
                self.settings.sidebar_hidden = hide;
                if !hide {
                    self.sidebar_width = layout::sidebar_at(x, total, pane_open);
                }
            }
            Splitter::Pane => {
                let sidebar = self.shown_sidebar_width(total, pane_open);
                self.pane_width = Some(layout::pane_at(x, total, sidebar));
            }
            Splitter::Files => {
                self.files_width = (x - self.files_left.get()).clamp(FILES_MIN, FILES_MAX);
            }
            Splitter::PaneHeight => {
                // The pane runs from the pointer down to the bottom of the area (above the banner, if showing).
                let height = self.main_height(window);
                let bottom = height;
                let most = (height - GRAPH_HEIGHT_MIN).max(PANE_HEIGHT_MIN);
                self.pane_height = Some((bottom - f32::from(event.position.y)).clamp(PANE_HEIGHT_MIN, most));
            }
        }
        cx.notify();
    }

    // ---- loading ----------------------------------------------------------------------------

    /// Runs `job` on a background thread, then hands its result to `done` on the UI thread.
    pub(crate) fn spawn_load<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        job: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        let task = cx.background_executor().spawn(async move { job() });
        cx.spawn(async move |this, cx| {
            let value = task.await;
            this.update(cx, |this, cx| done(this, value, cx)).ok();
        })
        .detach();
    }

    pub(crate) fn fail(&mut self, message: impl std::fmt::Display, cx: &mut Context<Self>) {
        self.notice = Some(Notice::warn(message.to_string()));
        cx.notify();
    }

    pub fn dismiss_notice(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        cx.notify();
    }

    // ---- projects ---------------------------------------------------------------------------

    pub fn open_folder(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            this.update(cx, |this, cx| {
                for path in paths {
                    this.add_folder(&path, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Adds a folder to the sidebar and opens it. A folder inside a repository adds the repository.
    /// Asking git where the repository starts takes a moment (on Windows, with a virus scanner, many), so
    /// it happens off the window's thread; the window says "Opening…" meanwhile and stays usable.
    pub fn add_folder(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.add_folder_noting(path, None, cx);
    }

    /// `add_folder`, then says `note` once the project is open (opening a project clears the banner).
    pub(crate) fn add_folder_noting(&mut self, path: &Path, note: Option<String>, cx: &mut Context<Self>) {
        let Some(store) = &self.store else {
            return self.fail("There is no folder to save projects in.", cx);
        };
        let store = Store::at(store.dir());
        let path = path.to_owned();
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        self.opening += 1;
        self.busy = Some(format!("Opening {name}…").into());
        cx.notify();
        self.spawn_load(
            cx,
            move || {
                let root = GitCli::new(&path)
                    .toplevel()
                    .map_err(|_| format!("{} is not inside a git repository.", path.display()))?;
                store.add_project(&root).map_err(|err| err.to_string())
            },
            move |this, result, cx| {
                this.opening = this.opening.saturating_sub(1);
                if this.opening == 0 {
                    this.busy = None;
                }
                match result {
                    Ok(project) => {
                        if !this.projects.iter().any(|p| p.path == project.path) {
                            this.projects.push(project.clone());
                        }
                        this.select_project(project.path, cx);
                        if let Some(note) = note {
                            this.notice = Some(Notice::info(note));
                        }
                    }
                    Err(message) => this.fail(message, cx),
                }
                cx.notify();
            },
        );
    }

    pub fn remove_project(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(store) = &self.store
            && let Err(err) = store.remove_project(path)
        {
            return self.fail(err, cx);
        }
        self.projects.retain(|p| p.path != path);
        self.scan_caches.remove(path);
        if self.settings.last_project.as_deref() == Some(path) {
            self.settings.last_project = None;
            self.save_settings();
        }
        if self.last_log.as_ref().is_some_and(|last| last.path == path) {
            self.last_log = None;
        }
        if self.running_scan.as_ref().is_some_and(|scan| scan.path == path) {
            self.stop_merge_scan();
        }
        if self.repo.as_ref().is_some_and(|repo| repo.project.path == path) {
            self.repo = None;
        }
        cx.notify();
    }

    pub fn select_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| p.path == path).cloned() else { return };
        if self.settings.last_project.as_ref() != Some(&path) {
            self.settings.last_project = Some(path.clone());
            self.save_settings();
        }
        self.loads += 1;
        let generation = self.loads;
        // What the banner says is about the project it was said in.
        if self.repo.as_ref().is_some_and(|repo| repo.project.path != path) {
            self.notice = None;
        }
        // Reading the same repository again keeps its filters; another one starts with none.
        let kept = self.repo.as_ref().filter(|repo| repo.project.path == path).map(|repo| GraphFilter {
            found: None,
            searching: false,
            ..repo.graph_filter.clone()
        });
        if kept.is_none() {
            self.search_input.update(cx, |input, cx| input.clear(cx));
        }
        // The last project's parsed history is of no use to another one, and keeping it until the new one
        // has been read would hold both in memory at once.
        if self.last_log.as_ref().is_some_and(|last| last.path != path) {
            self.last_log = None;
        }
        // Whatever was open is closed: reads still running for it stop.
        self.cancel_file_load();
        self.cancel_commit_load();
        let mut state = RepoState::loading(project, generation, &self.settings);
        state.graph_filter = kept.unwrap_or_default();
        let rerun = matches!(state.graph_filter.query(), Query::Path(_) | Query::Code(_));
        self.repo = Some(state);
        cx.notify();
        if rerun {
            self.run_search(cx);
        }

        // Opening another project leaves the last one's merge scan nothing to do.
        if self.running_scan.as_ref().is_some_and(|scan| scan.path != path) {
            self.stop_merge_scan();
        }
        self.start_read(cx);
    }

    /// Reads the open repository in the background. Only one read runs at a time: asking again while
    /// one runs (holding Cmd-R, or an operation finishing) makes one more read once it is done, since
    /// the one running may have started before what changed.
    fn start_read(&mut self, cx: &mut Context<Self>) {
        if self.reading {
            self.reread = true;
            return;
        }
        let Some(repo) = self.repo.as_ref() else { return };
        let (path, generation) = (repo.project.path.clone(), repo.generation);
        self.reading = true;
        self.reread = false;
        let counts = self.counts.clone();
        let read_path = path.clone();
        let unchanged = self.last_log.as_ref().filter(|last| last.path == path).map(|last| last.fingerprint);
        self.spawn_load(
            cx,
            move || {
                counts.repo_reads.fetch_add(1, Ordering::Relaxed);
                read_repo(&read_path, unchanged)
            },
            move |this, result, cx| {
                this.reading = false;
                let settings = this.settings.clone();
                let stale = this.reread || this.repo.as_ref().is_none_or(|repo| repo.generation != generation);
                if stale {
                    // Something asked for a newer read meanwhile: this answer may predate it.
                    if this.repo.as_ref().is_some_and(|repo| matches!(repo.phase, Phase::Loading)) {
                        this.start_read(cx);
                    }
                    return;
                }
                // An unchanged log reuses the commits read last time. (Reads run one at a time, so the
                // last read is still the one the fingerprint was taken from.)
                let last = this.last_log.take().filter(|last| last.path == path);
                let result = result.map_err(|err| err.to_string()).and_then(|mut data| {
                    let (commits, everyone) = match (data.commits.take(), last) {
                        (Some(commits), _) => {
                            let everyone = Arc::new(people(&commits));
                            (Arc::new(commits), everyone)
                        }
                        (None, Some(last)) => (last.commits, last.people),
                        (None, None) => return Err("The history could not be read; try again.".to_owned()),
                    };
                    Ok((data, commits, everyone))
                });
                let Some(repo) = this.repo.as_mut() else { return };
                match result {
                    Ok((data, commits, everyone)) => {
                        repo.work = data.work;
                        repo.graph_filter.today = data.today;
                        this.last_log = Some(LastLog {
                            path: path.clone(),
                            fingerprint: data.fingerprint,
                            commits: commits.clone(),
                            people: everyone.clone(),
                        });
                        note_avatar_hints(&this.avatar_hints, &commits, data.web.as_ref());
                        let (branches, targets) = scan_inputs(&commits);
                        let mut view = RepoView {
                            commits,
                            changed: data.changed,
                            entries: Vec::new(),
                            graph_width: 0.,
                            folded_lanes: 0,
                            sync_count: 0,
                            current_branch: data.current_branch.map(SharedString::from),
                            timing: SharedString::default(),
                            read: data.read,
                            lineages: Vec::new(),
                            names: Vec::new(),
                            base: None,
                            web: data.web,
                            people: everyone,
                            shown: 0,
                            stashes: 0,
                            matches: Vec::new(),
                            clues: Vec::new(),
                            tips: branches.iter().chain(&targets).map(|t| (t.name.clone(), t.id.clone())).collect(),
                            scanning: false,
                            unchecked: 0,
                            scan_inputs: (branches, targets),
                            upstreams: data.upstreams,
                            remotes: data.remotes,
                            operation: data.operation,
                            rebased: data.rebased,
                            twins: HashMap::new(),
                        };
                        // Branches that have not moved since the last scan are known at once.
                        let cache = this.scan_caches.entry(path.clone()).or_default().clone();
                        let (branches, targets) = &view.scan_inputs;
                        let wanted = !branches.is_empty() && !targets.is_empty();
                        let known = wanted.then(|| cache.lookup(branches, targets)).flatten();
                        let scan = wanted && known.is_none();
                        if let Some(known) = known {
                            view.clues = known.clues;
                        }
                        view.scanning = scan;
                        view.rebuild(&settings, &repo.collapsed, &repo.graph_filter, repo.project.workflow.as_ref().and_then(|w| w.base.as_deref()));
                        let inputs = view.scan_inputs.clone();
                        // Read again in the background (after a fetch), the selected commit stays selected wherever its row went.
                        let selected = repo.selected_id();
                        repo.selected = selected.and_then(|id| view.entries.iter().position(|e| e.commit.as_deref() == Some(id.as_str())));
                        let finished = view.operation.is_none();
                        let twin_inputs = view.twin_inputs();
                        repo.phase = Phase::Ready(view);
                        if finished && std::mem::take(&mut this.graph_hidden_for_conflicts) {
                            this.graph_hidden = false;
                        }
                        if !twin_inputs.is_empty() {
                            this.start_twin_scan(path.clone(), twin_inputs, cx);
                        }
                        if scan {
                            this.start_merge_scan(path, inputs, cache, cx);
                        }
                        // An operation that just stopped on conflicts opens its first file in the resolver.
                        if std::mem::take(&mut this.open_next_conflict) {
                            this.open_first_conflict(cx);
                        }
                    }
                    Err(err) => repo.phase = Phase::Failed(err.into()),
                }
                cx.notify();
            },
        );
    }

    /// A handle on the commits kept for the next refresh, which tells whether they are still alive.
    #[cfg(test)]
    pub(crate) fn kept_history(&self) -> Option<std::sync::Weak<Vec<Commit>>> {
        self.last_log.as_ref().map(|last| Arc::downgrade(&last.commits))
    }

    /// A merge scan is running.
    #[cfg(test)]
    pub(crate) fn scan_running(&self) -> bool {
        self.running_scan.is_some()
    }

    /// Looks, in the background, for commits that are copies of one another (a rebase or a cherry-pick makes new commits
    /// with the same changes), then marks them. Cheap: git compares the patches of what each side has on its own.
    fn start_twin_scan(&mut self, path: PathBuf, inputs: Vec<(String, String)>, cx: &mut Context<Self>) {
        let job_inputs = inputs.clone();
        let git_path = path.clone();
        self.spawn_load(
            cx,
            move || {
                let git = GitCli::new(&git_path);
                job_inputs.iter().flat_map(|(a, b)| git.twins(a, b)).collect::<Vec<_>>()
            },
            move |this, pairs, cx| {
                let settings = this.settings.clone();
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.project.path == path) else { return };
                let Phase::Ready(view) = &mut repo.phase else { return };
                // The answer is about the branches as they were when asked.
                if view.twin_inputs() != inputs {
                    return;
                }
                view.twins = pairs.iter().flat_map(|(a, b)| [(a.clone(), b.clone()), (b.clone(), a.clone())]).collect();
                if view.twins.is_empty() {
                    return;
                }
                repo.rebuild(&settings);
                cx.notify();
            },
        );
    }

    /// Stops the merge scan that is running, if any. What it has found so far stays in its cache.
    fn stop_merge_scan(&mut self) {
        if let Some(scan) = self.running_scan.take() {
            scan.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Looks, in the background, for branches already merged by a squash or rebase, then redraws with
    /// what it found. A scan of the same branches that is already running is waited for, not repeated;
    /// one of other branches is stopped, since its answer is out of date.
    fn start_merge_scan(&mut self, path: PathBuf, inputs: ScanInputs, cache: Arc<ScanCache>, cx: &mut Context<Self>) {
        if self.running_scan.as_ref().is_some_and(|scan| scan.path == path && scan.inputs == inputs) {
            return;
        }
        self.stop_merge_scan();
        let cancel = Arc::new(AtomicBool::new(false));
        self.running_scan = Some(RunningScan { path: path.clone(), inputs: inputs.clone(), cancel: cancel.clone() });
        let counts = self.counts.clone();
        let (scan_path, scan_inputs, stop) = (path.clone(), inputs.clone(), cancel.clone());
        self.spawn_load(
            cx,
            move || {
                counts.scans.fetch_add(1, Ordering::Relaxed);
                let (branches, targets) = &scan_inputs;
                GitCli::new(&scan_path).merge_clues_cached(branches, targets, &cache, &stop)
            },
            move |this, result, cx| {
                if this.running_scan.as_ref().is_some_and(|scan| Arc::ptr_eq(&scan.cancel, &cancel)) {
                    this.running_scan = None;
                }
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let settings = this.settings.clone();
                // Whichever read of this repository is shown, if it is about the same branches.
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.project.path == path) else { return };
                let Phase::Ready(view) = &mut repo.phase else { return };
                if view.scan_inputs != inputs {
                    return;
                }
                view.scanning = false;
                if let Ok(scan) = result {
                    view.clues = scan.clues;
                    view.unchecked = scan.unchecked;
                }
                repo.rebuild(&settings);
                cx.notify();
            },
        );
    }

    /// The banner has been dealt with; read the repository again, keeping the banner's message.
    pub(crate) fn refresh_keeping_notice(&mut self, cx: &mut Context<Self>) {
        self.refresh(cx);
    }

    /// Reads the open repository again, keeping nothing selected.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.repo.as_ref().map(|repo| repo.project.path.clone()) {
            self.select_project(path, cx);
        }
    }

    // ---- fetching on its own ----------------------------------------------------------------

    /// Fetch the open project every `minutes` in the background, or never (0). Kept for the next launch.
    pub fn set_auto_fetch(&mut self, minutes: u32, cx: &mut Context<Self>) {
        if self.settings.auto_fetch_minutes != minutes {
            self.settings.auto_fetch_minutes = minutes;
            self.save_settings();
            (self.auto_fetch_failures, self.auto_fetch_warned) = (0, false);
            self.schedule_auto_fetch(cx);
        }
        cx.notify();
    }

    /// Starts or stops the timer behind fetching on its own, to match the setting.
    fn schedule_auto_fetch(&mut self, cx: &mut Context<Self>) {
        self.auto_fetch_timer = (self.settings.auto_fetch_minutes > 0).then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(AUTO_FETCH_TICK).await;
                    if this.update(cx, |this, cx| this.auto_fetch_tick(cx)).is_err() {
                        break;
                    }
                }
            })
        });
    }

    /// Fetching on its own is on: there is a timer for it.
    #[cfg(test)]
    pub(crate) fn auto_fetch_scheduled(&self) -> bool {
        self.auto_fetch_timer.is_some()
    }

    /// The open project was just fetched by hand: the next fetch on its own counts from now.
    pub(crate) fn note_fetched(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.repo.as_ref().map(|repo| repo.project.path.clone()) {
            self.fetched_at.insert(path, cx.background_executor().now());
        }
    }

    /// The timer went off: fetches the open project if it is due one and nothing else is running.
    fn auto_fetch_tick(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref() else { return };
        let Phase::Ready(view) = &repo.phase else { return };
        if view.remotes.is_empty() || self.busy.is_some() || self.reading || self.auto_fetching {
            return;
        }
        let last = self.fetched_at.get(&repo.project.path).copied();
        if fetch_due(self.settings.auto_fetch_minutes, last, cx.background_executor().now()) {
            self.auto_fetch(cx);
        }
    }

    /// Fetches the open project in the background without a banner, and reads it again only when a remote branch or
    /// tag changed. The second failure in a row is said, once, until one works again.
    fn auto_fetch(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.repo.as_ref().map(|repo| repo.project.path.clone()) else { return };
        self.auto_fetching = true;
        cx.notify();
        let (fetch_path, counts) = (path.clone(), self.counts.clone());
        self.spawn_load(
            cx,
            move || {
                counts.fetches.fetch_add(1, Ordering::Relaxed);
                GitCli::new(&fetch_path).fetch_changed()
            },
            move |this, result, cx| {
                this.auto_fetching = false;
                this.fetched_at.insert(path.clone(), cx.background_executor().now());
                match result {
                    Ok(changed) => {
                        (this.auto_fetch_failures, this.auto_fetch_warned) = (0, false);
                        // An operation that is running reads the repository again when it ends anyway.
                        let open = this.repo.as_ref().is_some_and(|repo| repo.project.path == path);
                        if changed && open && this.busy.is_none() {
                            this.start_read(cx);
                        }
                    }
                    Err(error) => {
                        this.auto_fetch_failures += 1;
                        let warning_up = this.notice.as_ref().is_some_and(|notice| notice.warn);
                        if this.auto_fetch_failures >= 2 && !this.auto_fetch_warned && !warning_up {
                            this.auto_fetch_warned = true;
                            let said = crate::menu::explain(&error);
                            this.notice = Some(Notice::warn(format!(
                                "Fetching from the remote on its own failed twice in a row: {}. It tries again every {} minutes; Settings, Projects turns it off.",
                                said.trim_end_matches('.'),
                                this.settings.auto_fetch_minutes
                            )));
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    // ---- commits ----------------------------------------------------------------------------

    pub fn select_entry(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        let Phase::Ready(view) = &repo.phase else { return };
        // The working-tree row opens what has changed.
        let Some(id) = view.entries.get(ix).and_then(|entry| entry.commit.clone()) else { return self.open_work(cx) };

        repo.selected = Some(ix);
        repo.file = None;
        repo.filter.clear();
        repo.file_rows.clear();
        repo.commit = Some(CommitState { id: id.clone(), phase: Phase::Loading });
        let path = repo.project.path.clone();
        self.filter_input.update(cx, |input, cx| input.clear(cx));
        cx.notify();

        let read_path = path.clone();
        let read_id = id.clone();
        self.cancel_file_load();
        let ticket = self.commit_ticket.fetch_add(1, Ordering::Relaxed) + 1;
        let (latest, counts) = (self.commit_ticket.clone(), self.counts.clone());
        self.spawn_load(
            cx,
            move || {
                read_commit(&read_path, &read_id, || current(&latest, ticket))
                    .inspect(|_| _ = counts.commit_reads.fetch_add(1, Ordering::Relaxed))
            },
            move |this, result, cx| {
                // Stopped early: another commit was picked since.
                let Some(result) = result else { return };
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.project.path == path) else { return };
                let Some(commit) = repo.commit.as_mut().filter(|commit| commit.id == id) else { return };
                commit.phase = match result {
                    Ok(view) => Phase::Ready(view),
                    Err(err) => Phase::Failed(err.to_string().into()),
                };
                repo.refresh_file_rows();
                cx.notify();
            },
        );
    }

    /// Selects a commit by id (a parent link in the detail panel) and scrolls the graph to it.
    pub fn select_commit_id(&mut self, id: &str, cx: &mut Context<Self>) {
        let ix = match self.repo.as_ref().map(|repo| &repo.phase) {
            Some(Phase::Ready(view)) => view.entries.iter().position(|e| e.commit.as_deref() == Some(id)),
            _ => None,
        };
        // It may be inside a group that is folded away: unfold everything and look again.
        let ix = ix.or_else(|| {
            let settings = self.settings.clone();
            let repo = self.repo.as_mut()?;
            if repo.collapsed.is_empty() {
                return None;
            }
            repo.collapsed.clear();
            repo.rebuild(&settings);
            match &repo.phase {
                Phase::Ready(view) => view.entries.iter().position(|e| e.commit.as_deref() == Some(id)),
                _ => None,
            }
        });
        match ix {
            Some(ix) => {
                self.graph_scroll.scroll_to_item(ix, gpui::ScrollStrategy::Center);
                self.select_entry(ix, cx);
            }
            None => self.fail("That commit is older than the history loaded here.", cx),
        }
    }

    // ---- files ------------------------------------------------------------------------------

    pub fn open_file(&mut self, index: usize, cx: &mut Context<Self>) {
        // A file left in conflict opens in the resolver, not as a diff.
        if self.work_open() && self.repo.as_ref().is_some_and(|repo| repo.work.get(index).is_some_and(|f| f.conflicted)) {
            return self.open_conflict(index, cx);
        }
        let Some(repo) = self.repo.as_mut() else { return };
        let Some(commit) = repo.commit.as_ref() else { return };
        let Phase::Ready(view) = &commit.phase else { return };
        let Some(change) = view.files.get(index).cloned() else { return };
        let (path, id) = (repo.project.path.clone(), commit.id.clone());

        let mut file = FileState::new(index);
        match self.comments_for(&path, &id, &change.path) {
            Ok(comments) => file.comments = comments,
            Err(message) => self.notice = Some(Notice::warn(message)),
        }
        let Some(repo) = self.repo.as_mut() else { return };
        repo.file = Some(file);
        cx.notify();

        self.load_file(index, DIFF_CONTEXT, false, cx);
    }

    /// Reads the open file's diff with `context` unchanged lines around each change, colors it, and
    /// shows it. `keep_scroll` keeps the place in the list (for showing more lines).
    fn load_file(&mut self, index: usize, context: u32, keep_scroll: bool, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        let Some(commit) = repo.commit.as_ref() else { return };
        let Phase::Ready(view) = &commit.phase else { return };
        let Some(change) = view.files.get(index).cloned() else { return };
        let (path, id) = (repo.project.path.clone(), commit.id.clone());
        // A new or deleted file has one side only.
        let single_column = matches!(change.status, FileStatus::Added | FileStatus::Deleted);
        let read_path = path.clone();
        let read_id = id.clone();
        let read_change = change.clone();
        // An uncommitted change is read from the working tree and the index, not from a commit.
        let work = (id == WORKTREE).then(|| repo.work.get(index).cloned()).flatten();
        // Stepping through files quickly leaves reads behind for files already left: each stops at its
        // next step instead of reading and coloring whole files nobody will see.
        let ticket = self.file_ticket.fetch_add(1, Ordering::Relaxed) + 1;
        let (latest, counts) = (self.file_ticket.clone(), self.counts.clone());
        self.spawn_load(
            cx,
            move || -> Option<Result<FileRead, gitgui_core::Error>> {
                let wanted = || current(&latest, ticket);
                if !wanted() {
                    return None;
                }
                counts.file_reads.fetch_add(1, Ordering::Relaxed);
                let git = GitCli::new(&read_path);
                let path = read_change.path.as_str();
                let image = preview::is_image(path);
                let language = syntax::language_of(path).is_some();
                // The diff, and the whole file before and after (only read when there is a use for it:
                // coloring, so each line is colored knowing what comes before it, or a picture).
                let (diff, old, new) = match &work {
                    Some(work) => {
                        let diff = match git.work_diff(work, context) {
                            Ok(diff) => diff,
                            Err(err) => return Some(Err(err)),
                        };
                        if !wanted() {
                            return None;
                        }
                        let (old, new) = if image || language || (!diff.binary && !single_column) { git.work_sides(work) } else { (None, None) };
                        (diff, old, new)
                    }
                    None => {
                        let diff = match git.file_diff(&read_id, &read_change, context) {
                            Ok(diff) => diff,
                            Err(err) => return Some(Err(err)),
                        };
                        if !wanted() {
                            return None;
                        }
                        let (old, new) = if image || language || (!diff.binary && !single_column) {
                            let old_path = read_change.old_path.as_deref().unwrap_or(path);
                            let old = (read_change.status != FileStatus::Added)
                                .then(|| git.file_bytes_at(&format!("{read_id}^1"), old_path).ok().flatten())
                                .flatten();
                            let new = (read_change.status != FileStatus::Deleted)
                                .then(|| git.file_bytes_at(&read_id, path).ok().flatten())
                                .flatten();
                            (old, new)
                        } else {
                            (None, None)
                        };
                        (diff, old, new)
                    }
                };
                // A picture is shown before and after, as well as (for an SVG) diffed as text.
                let images = image.then(|| preview::Images {
                    old: old.as_deref().and_then(|bytes| preview::preview(path, bytes)),
                    new: new.as_deref().and_then(|bytes| preview::preview(path, bytes)),
                });
                if !wanted() {
                    return None;
                }
                let colors = if language && !diff.binary {
                    fn text(bytes: &Option<Vec<u8>>) -> Option<&str> {
                        bytes.as_deref().and_then(|b| std::str::from_utf8(b).ok())
                    }
                    syntax::for_diff(path, text(&old), text(&new), &diff)
                } else {
                    syntax::FileColors::default()
                };
                // The file after the change, to show more of the unchanged lines around the changes from.
                let text = (!diff.binary && !single_column)
                    .then(|| new.as_deref().filter(|bytes| bytes.len() <= REVEAL_MAX_BYTES).and_then(|bytes| String::from_utf8(bytes.to_vec()).ok()))
                    .flatten()
                    .map(Arc::new);
                Some(Ok((diff, colors, images, text)))
            },
            move |this, result, cx| {
                // Stopped early: another file was opened since.
                let Some(result) = result else { return };
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.project.path == path) else { return };
                let mode = repo.mode;
                let still_open = repo.commit.as_ref().is_some_and(|commit| commit.id == id);
                let Some(file) =
                    repo.file.as_mut().filter(|file| still_open && file.index == index && file.context == context)
                else {
                    return;
                };
                match result {
                    Ok((diff, colors, images, text)) => {
                        file.raw = diff;
                        file.text = text;
                        file.reveals.clear();
                        file.colors = colors;
                        file.images = images;
                        file.single_column = single_column;
                        file.apply_reveals();
                        file.phase = Phase::Ready(());
                        file.rebuild(mode, keep_scroll);
                    }
                    Err(err) => file.phase = Phase::Failed(err.to_string().into()),
                }
                cx.notify();
            },
        );
    }


    /// Shows more unchanged lines around every change in the open file: 25, then 100, then 400, then the
    /// whole file. `None` goes back to the usual 3.
    pub fn set_diff_context(&mut self, step: Option<u32>, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        let Some(file) = repo.file.as_mut() else { return };
        let context = step.unwrap_or(DIFF_CONTEXT);
        if file.context == context || !matches!(file.phase, Phase::Ready(())) {
            return;
        }
        file.context = context;
        let index = file.index;
        self.cancel_file_load();
        self.load_file(index, context, true, cx);
    }

    /// The next amount of context after "Show more lines".
    pub fn more_context(&mut self, cx: &mut Context<Self>) {
        let Some(current) = self.repo.as_ref().and_then(|repo| repo.file.as_ref()).map(|file| file.context) else { return };
        let next = match current {
            0..=24 => 25,
            25..=99 => 100,
            100..=399 => 400,
            _ => WHOLE_FILE,
        };
        self.set_diff_context(Some(next), cx);
    }

    /// Stops a background read of a file that is about to be closed or replaced.
    pub(crate) fn cancel_file_load(&self) {
        self.file_ticket.fetch_add(1, Ordering::Relaxed);
    }

    /// Stops a background read of a commit that is about to be closed or replaced.
    pub(crate) fn cancel_commit_load(&self) {
        self.commit_ticket.fetch_add(1, Ordering::Relaxed);
    }

    pub fn close_file(&mut self, cx: &mut Context<Self>) {
        self.cancel_file_load();
        self.hover_line = None;
        if let Some(repo) = self.repo.as_mut()
            && repo.file.take().is_some()
        {
            cx.notify();
        }
    }

    /// Escape: leave full view first, then go from a file back to the commit overview.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            return self.close_menu(cx);
        }
        if self.dialog.is_some() {
            return self.cancel_dialog(cx);
        }
        if self.branch_picker.is_some() {
            return self.close_branch_picker(cx);
        }
        if self.new_branch.is_some() {
            return self.close_new_branch(cx);
        }
        if self.settings_open {
            return self.close_settings(cx);
        }
        if self.legend_open {
            return self.toggle_legend(cx);
        }
        let Some(repo) = self.repo.as_mut() else { return };
        if repo.expanded {
            repo.expanded = false;
            cx.notify();
        } else if repo.file.is_none() && repo.graph_filter.isolate.is_some() {
            // Nothing open: the branch picked out of the graph goes back among the others.
            self.clear_isolate(cx);
        } else {
            self.close_file(cx);
        }
    }

    pub fn toggle_expanded(&mut self, cx: &mut Context<Self>) {
        if let Some(repo) = self.repo.as_mut() {
            repo.expanded = !repo.expanded;
            cx.notify();
        }
    }

    /// The pointer is at the edge of a hidden panel: show it over the content.
    pub fn peek_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.peek != Some(panel) {
            self.peek = Some(panel);
            cx.notify();
        }
    }

    /// The pointer left the panel that was showing over the content.
    pub fn unpeek(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.peek == Some(panel) {
            self.peek = None;
            cx.notify();
        }
    }

    /// Hides or shows the graph while a commit is open.
    pub fn toggle_graph_hidden(&mut self, cx: &mut Context<Self>) {
        self.graph_hidden = !self.graph_hidden;
        self.graph_hidden_for_conflicts = false;
        self.peek = None;
        cx.notify();
    }

    /// The height of the area the graph and the file pane share: the window's, less the banner when one shows.
    pub(crate) fn main_height(&self, window: &Window) -> f32 {
        let banner = if self.notice.is_some() || self.busy.is_some() { NOTICE_HEIGHT } else { 0. };
        let bar = if self.operation().is_some() { OPERATION_BAR_HEIGHT } else { 0. };
        f32::from(window.viewport_size().height) - banner - bar
    }

    /// A thin strip standing where a hidden panel was; pointing at it slides the panel in over the content.
    /// It takes a few pixels of its own, so it never covers anything. `drag` makes it also start that divider's
    /// drag (the sidebar's rail is where the sidebar's divider is, to drag it back out).
    pub(crate) fn rail(&self, id: &'static str, panel: Panel, drag: Option<Splitter>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .flex_none()
            .w(px(10.))
            .h_full()
            .flex()
            .justify_center()
            .cursor(CursorStyle::ResizeLeftRight)
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.peek_panel(panel, cx);
                }
            }))
            .when_some(drag, |rail, which| {
                rail.on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.begin_resize(which, cx)))
            })
            // A hairline so there is something to find, which brightens under the pointer.
            .child(div().w(px(2.)).h_full().bg(rgb(t().border)).hover(|line| line.bg(rgb(t().accent))))
            .into_any_element()
    }

    /// A hidden panel slid in over the content, until the pointer leaves it.
    pub(crate) fn peeking(&self, id: &'static str, panel: Panel, left: f32, width: f32, content: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .absolute()
            .left(px(left))
            .top_0()
            .bottom_0()
            .w(px(width))
            .flex()
            .bg(rgb(t().panel))
            .border_r_1()
            .border_color(rgb(t().border))
            .shadow_lg()
            .occlude()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.unpeek(panel, cx);
                }
            }))
            .child(content)
            .into_any_element()
    }

    /// Full view of the file pane, if it is not already.
    pub fn expand_pane(&mut self, cx: &mut Context<Self>) {
        if let Some(repo) = self.repo.as_mut()
            && !repo.expanded
        {
            repo.expanded = true;
            cx.notify();
        }
    }

    /// Closes the file pane and deselects the commit.
    pub fn close_pane(&mut self, cx: &mut Context<Self>) {
        self.cancel_file_load();
        self.cancel_commit_load();
        if let Some(repo) = self.repo.as_mut() {
            repo.commit = None;
            repo.file = None;
            repo.selected = None;
            repo.expanded = false;
            repo.file_rows.clear();
            cx.notify();
        }
    }

    pub fn toggle_files_visible(&mut self, cx: &mut Context<Self>) {
        if let Some(repo) = self.repo.as_mut() {
            repo.files_visible = !repo.files_visible;
            cx.notify();
        }
    }

    /// Tree or flat list of changed files. The choice is kept for the next launch.
    pub fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        self.settings.file_layout = file_layout_of(layout);
        self.save_settings();
        let Some(repo) = self.repo.as_mut() else { return cx.notify() };
        if repo.layout != layout {
            repo.layout = layout;
            repo.refresh_file_rows();
        }
        cx.notify();
    }

    pub fn set_filter(&mut self, filter: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        if repo.filter != filter {
            repo.filter = filter;
            repo.refresh_file_rows();
            cx.notify();
        }
    }

    /// Folds or unfolds a folder in the changed-files tree.
    pub fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        if !repo.folded_dirs.remove(path) {
            repo.folded_dirs.insert(path.to_owned());
        }
        repo.refresh_file_rows();
        cx.notify();
    }

    /// Folds or unfolds the commits listed under a pull request or merge.
    pub fn toggle_group(&mut self, commit: &str, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        let Some(repo) = self.repo.as_mut() else { return };
        if !repo.collapsed.remove(commit) {
            repo.collapsed.insert(commit.to_owned());
        }
        repo.rebuild(&settings);
        cx.notify();
    }

    pub fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = true;
        // Themes installed in Zed since the app started show up here.
        self.themes = theme::all(self.store.as_ref().map(Store::dir));
        self.icon_themes = icons::all();
        cx.notify();
    }

    /// Switches file icons to the Zed icon theme named `name`, or the built-in ones for `None`.
    pub fn set_icon_theme(&mut self, name: Option<&str>, cx: &mut Context<Self>) {
        let found = name.and_then(|name| self.icon_themes.iter().find(|t| t.name == name).cloned());
        self.settings.icon_theme = found.as_ref().map(|t| t.name.clone());
        icons::set(found);
        self.save_settings();
        cx.notify();
    }

    /// Switches to the color theme named `name` and keeps the choice.
    pub fn set_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(found) = self.themes.iter().find(|t| t.name == name).cloned() else { return };
        theme::set(found);
        // Icons drawn in the old theme's colors are drawn again.
        icons::set(self.settings.icon_theme.as_ref().and_then(|n| self.icon_themes.iter().find(|t| &t.name == n).cloned()));
        self.settings.theme = (name != theme::DEFAULT).then(|| name.to_owned());
        self.save_settings();
        cx.refresh_windows();
        cx.notify();
    }

    /// The picture for an author, asking for it in the background the first time.
    pub fn avatar_for(&self, email: &str, cx: &mut Context<Self>) -> Avatar {
        if !self.settings.fetch_avatars || email.is_empty() {
            return Avatar::None;
        }
        let (avatar, ask) = self.avatars.borrow_mut().get(email);
        if ask && self.avatars.borrow_mut().start(email) {
            self.fetch_avatar(email.to_owned(), cx);
        }
        avatar
    }

    /// Asks for one author's picture in the background; when it is in, the next waiting one is asked.
    fn fetch_avatar(&self, email: String, cx: &mut Context<Self>) {
        let dir = self.avatars.borrow().dir.clone();
        let hint = self.avatar_hints.borrow().get(&email.trim().to_ascii_lowercase()).cloned();
        let task = cx.background_executor().spawn({
            let email = email.clone();
            // Tests never reach the network.
            let fetch: fn(&str) -> avatars::Fetched = if cfg!(test) { |_| avatars::Fetched::Failed } else { avatars::fetch };
            let lookup: fn(&avatars::Hint) -> Result<Option<String>, ()> = if cfg!(test) { |_| Err(()) } else { avatars::gh_lookup };
            async move { avatars::load(&email, dir.as_deref(), hint.as_ref(), fetch, lookup) }
        });
        cx.spawn(async move |this, cx| {
            let avatar = task.await;
            this.update(cx, |this, cx| {
                let next = {
                    let mut avatars = this.avatars.borrow_mut();
                    avatars.set(&email, avatar);
                    avatars.finish()
                };
                if let Some(next) = next {
                    this.fetch_avatar(next, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Turns fetching authors' pictures on or off, and keeps the choice.
    pub fn toggle_fetch_avatars(&mut self, cx: &mut Context<Self>) {
        self.settings.fetch_avatars = !self.settings.fetch_avatars;
        self.avatars.borrow_mut().clear();
        self.save_settings();
        cx.notify();
    }

    /// A text field has the keyboard, so keys like the arrows are its own.
    pub fn typing(&self, window: &Window, cx: &App) -> bool {
        [&self.input, &self.filter_input, &self.search_input, &self.commit_input, &self.dialog_input, &self.branch_ticket, &self.branch_title, &self.branch_search]
            .into_iter()
            .any(|input| gpui::Focusable::focus_handle(input.read(cx), cx).is_focused(window))
    }

    /// The app's data folder, when there is one.
    pub fn store_dir(&self) -> Option<&Path> {
        self.store.as_ref().map(Store::dir)
    }

    pub(crate) fn save_settings(&mut self) {
        if let Some(store) = &self.store
            && let Err(err) = store.save_settings(&self.settings)
        {
            self.notice = Some(Notice::warn(format!("The choice could not be saved: {err}")));
        }
    }

    pub fn close_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings_open {
            self.settings_open = false;
            cx.notify();
        }
    }

    /// Turns "group commits under their pull request" on or off, keeps the choice, and redraws the graph.
    pub fn toggle_group_by_parent(&mut self, cx: &mut Context<Self>) {
        self.settings.group_by_parent = !self.settings.group_by_parent;
        if let Some(store) = &self.store
            && let Err(err) = store.save_settings(&self.settings)
        {
            self.notice = Some(Notice::warn(format!("The choice could not be saved: {err}")));
        }
        let settings = self.settings.clone();
        if let Some(repo) = self.repo.as_mut() {
            repo.rebuild(&settings);
        }
        cx.notify();
    }

    // ---- graph filters ----------------------------------------------------------------------

    /// Changes the graph's filters with `change`, then lays the graph out again.
    fn change_filter(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut GraphFilter)) {
        let settings = self.settings.clone();
        let Some(repo) = self.repo.as_mut() else { return };
        change(&mut repo.graph_filter);
        repo.rebuild(&settings);
        cx.notify();
    }

    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.change_filter(cx, |filter| filter.scope = scope);
    }

    pub fn toggle_hide_merged(&mut self, cx: &mut Context<Self>) {
        self.change_filter(cx, |filter| filter.hide_merged = !filter.hide_merged);
    }

    pub fn toggle_stashes(&mut self, cx: &mut Context<Self>) {
        self.change_filter(cx, |filter| filter.hide_stashes = !filter.hide_stashes);
    }

    /// Picks one branch out of the graph: it and the branch it was cut from, nothing else. `name` is what its label
    /// says; a remote branch's is `origin/feat/x`, and it is the branch `feat/x` that is picked.
    pub fn isolate_branch(&mut self, name: &str, remote: bool, cx: &mut Context<Self>) {
        let branch = if remote { name.split_once('/').map_or(name, |(_, rest)| rest) } else { name }.to_owned();
        self.change_filter(cx, |filter| filter.isolate = Some(branch));
    }

    /// Back to the view the filter bar says.
    pub fn clear_isolate(&mut self, cx: &mut Context<Self>) {
        self.change_filter(cx, |filter| filter.isolate = None);
    }

    /// The pointer is on a row of this branch line (or, with `None`, has left the graph): its line comes forward.
    pub fn hover_graph_line(&mut self, line: Option<usize>, twin: Option<String>, cx: &mut Context<Self>) {
        if self.graph_hover != line || self.twin_hover != twin {
            self.graph_hover = line;
            self.twin_hover = twin;
            cx.notify();
        }
    }

    /// For the screenshot script: the pointer is on graph row `row` (none: it has left).
    pub fn script_graph_hover(&mut self, row: Option<usize>, cx: &mut Context<Self>) {
        let line = row.and_then(|row| match self.repo.as_ref().map(|repo| &repo.phase) {
            Some(Phase::Ready(view)) => view.entries.get(row).map(|entry| entry.row.lineage),
            _ => None,
        });
        let twin = row.and_then(|row| match self.repo.as_ref().map(|repo| &repo.phase) {
            Some(Phase::Ready(view)) => view.entries.get(row).and_then(|entry| entry.twin.clone()),
            _ => None,
        });
        self.hover_graph_line(line, twin, cx);
    }

    pub fn toggle_sync_merges(&mut self, cx: &mut Context<Self>) {
        self.change_filter(cx, |filter| filter.show_sync = !filter.show_sync);
    }

    /// The search box changed: a plain search dims what it misses right away; `path:` and `code:`
    /// wait for Enter, since they ask git.
    pub fn set_search(&mut self, text: String, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        let Some(repo) = self.repo.as_mut() else { return };
        if repo.graph_filter.search == text {
            return;
        }
        let before = repo.graph_filter.parsed().0;
        repo.graph_filter.search = text;
        // A new author or date limits the graph itself, so it is drawn again; other words only mark rows.
        if repo.graph_filter.parsed().0 != before {
            repo.rebuild(&settings);
        } else {
            let filter = repo.graph_filter.clone();
            if let Phase::Ready(view) = &mut repo.phase {
                view.apply_search(&filter);
            }
        }
        cx.notify();
    }

    /// Enter in the search box: run a `path:` or `code:` search that has not run yet, else go to
    /// the next commit found.
    pub fn submit_search(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref() else { return };
        let filter = &repo.graph_filter;
        if matches!(filter.query(), Query::Path(_) | Query::Code(_)) && filter.found_now().is_none() {
            if !filter.searching {
                self.run_search(cx);
            }
            return;
        }
        self.next_match(cx);
    }

    /// Selects the next commit the search found after the selected one, wrapping round.
    pub fn next_match(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref() else { return };
        let Phase::Ready(view) = &repo.phase else { return };
        let after = repo.selected;
        let Some(&ix) = view.matches.iter().find(|&&ix| after.is_none_or(|at| ix > at)).or(view.matches.first()) else {
            return;
        };
        self.graph_scroll.scroll_to_item(ix, gpui::ScrollStrategy::Center);
        self.select_entry(ix, cx);
    }

    /// Asks git for the commits a `path:` or `code:` search finds, then dims the rest.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        let query = repo.graph_filter.query();
        let text = repo.graph_filter.parsed().1.trim().to_owned();
        let path = repo.project.path.clone();
        let generation = repo.generation;
        repo.graph_filter.searching = true;
        cx.notify();
        self.spawn_load(
            cx,
            move || GitCli::new(&path).search(&query),
            move |this, result, cx| {
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.generation == generation) else { return };
                repo.graph_filter.searching = false;
                match result {
                    Ok(ids) => {
                        repo.graph_filter.found = Some((text, ids));
                        let filter = repo.graph_filter.clone();
                        if let Phase::Ready(view) = &mut repo.phase {
                            view.apply_search(&filter);
                        }
                        this.next_match(cx);
                    }
                    Err(err) => this.fail(format!("The search failed: {err}"), cx),
                }
                cx.notify();
            },
        );
    }

    /// Switches between the roomy graph and the compact one, keeps the choice, and redraws.
    pub fn toggle_compact_graph(&mut self, cx: &mut Context<Self>) {
        self.settings.compact_graph = !self.settings.compact_graph;
        self.save_settings();
        let settings = self.settings.clone();
        if let Some(repo) = self.repo.as_mut() {
            repo.rebuild(&settings);
        }
        cx.notify();
    }

    /// Folds the lanes past the sixth into one, or draws them all. Kept for the next launch.
    pub fn toggle_all_lanes(&mut self, cx: &mut Context<Self>) {
        self.settings.all_lanes = !self.settings.all_lanes;
        self.save_settings();
        let settings = self.settings.clone();
        if let Some(repo) = self.repo.as_mut() {
            repo.rebuild(&settings);
        }
        cx.notify();
    }

    /// Where the file pane sits: below the graph, or beside it. Kept for the next launch.
    pub fn set_review_layout(&mut self, layout: ReviewLayout, cx: &mut Context<Self>) {
        if self.settings.review_layout != layout {
            self.settings.review_layout = layout;
            self.save_settings();
            cx.notify();
        }
    }

    /// Which commits in the graph show their author's picture. Kept for the next launch.
    pub fn set_graph_faces(&mut self, faces: GraphFaces, cx: &mut Context<Self>) {
        if self.settings.graph_faces != faces {
            self.settings.graph_faces = faces;
            self.save_settings();
            cx.notify();
        }
    }

    /// The graph's look, by id (see `graph_style::STYLES`). Kept for the next launch.
    pub fn set_graph_style(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(style) = crate::graph_style::STYLES.iter().find(|s| s.id == id) else { return };
        crate::graph_style::set_active(style.id);
        if self.settings.graph_style != style.id {
            self.settings.graph_style = style.id.to_owned();
            self.save_settings();
        }
        cx.refresh_windows();
        cx.notify();
    }

    /// How large the graph is drawn, in percent. Kept for the next launch.
    pub fn set_graph_scale(&mut self, percent: u32, cx: &mut Context<Self>) {
        let percent = percent.clamp(gitgui_store::GRAPH_SCALE_MIN, gitgui_store::GRAPH_SCALE_MAX);
        if self.settings.graph_scale == percent {
            return;
        }
        self.settings.graph_scale = percent;
        self.save_settings();
        let settings = self.settings.clone();
        if let Some(repo) = self.repo.as_mut() {
            repo.rebuild(&settings);
        }
        cx.notify();
    }

    /// Unified or split diff. The choice is kept for the next launch.
    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.settings.diff_mode = diff_mode_of(mode);
        self.save_settings();
        let Some(repo) = self.repo.as_mut() else { return cx.notify() };
        if repo.mode != mode {
            repo.mode = mode;
            if let Some(file) = repo.file.as_mut() {
                file.rebuild(mode, false);
            }
        }
        cx.notify();
    }

    // ---- comments ---------------------------------------------------------------------------

    fn comments_for(&self, repo: &Path, commit: &str, file: &str) -> Result<Vec<Comment>, String> {
        match &self.store {
            Some(store) => store.comments_on(repo, commit, file).map_err(|err| err.to_string()),
            None => Ok(Vec::new()),
        }
    }

    /// The repository path, commit id and file path of the open file.
    fn open_file_key(&self) -> Option<(PathBuf, String, String)> {
        let repo = self.repo.as_ref()?;
        let commit = repo.commit.as_ref()?;
        let Phase::Ready(view) = &commit.phase else { return None };
        let file = repo.file.as_ref()?;
        let change = view.files.get(file.index)?;
        Some((repo.project.path.clone(), commit.id.clone(), change.path.clone()))
    }

    pub fn start_comment(&mut self, anchor: Anchor, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        let mode = repo.mode;
        let Some(file) = repo.file.as_mut() else { return };
        file.composing = Some(anchor);
        file.rebuild(mode, true);
        self.input.update(cx, |input, cx| {
            input.clear(cx);
            input.focus(window);
        });
        cx.notify();
    }

    pub fn cancel_comment(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        let mode = repo.mode;
        if let Some(file) = repo.file.as_mut()
            && file.composing.take().is_some()
        {
            file.rebuild(mode, true);
            cx.notify();
        }
    }

    pub fn submit_comment(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let Some((repo_path, commit, path)) = self.open_file_key() else { return };
        let Some(anchor) = self.repo.as_ref().and_then(|r| r.file.as_ref()).and_then(|f| f.composing) else { return };
        let Some(store) = &self.store else {
            return self.fail("There is no folder to save comments in.", cx);
        };
        let added = store.add_comment(
            &repo_path,
            NewComment { commit, path, side: anchor.side, line: anchor.line, text },
        );
        if let Err(err) = added {
            return self.fail(err, cx);
        }
        self.input.update(cx, |input, cx| input.clear(cx));
        self.reload_comments(true, cx);
    }

    pub fn delete_comment(&mut self, id: &str, cx: &mut Context<Self>) {
        let (Some((repo_path, ..)), Some(store)) = (self.open_file_key(), &self.store) else { return };
        if let Err(err) = store.delete_comment(&repo_path, id) {
            return self.fail(err, cx);
        }
        self.reload_comments(false, cx);
    }

    pub fn toggle_resolved(&mut self, id: &str, resolved: bool, cx: &mut Context<Self>) {
        let (Some((repo_path, ..)), Some(store)) = (self.open_file_key(), &self.store) else { return };
        if let Err(err) = store.set_resolved(&repo_path, id, resolved) {
            return self.fail(err, cx);
        }
        self.reload_comments(false, cx);
    }

    /// Reads the open file's comments again and redraws. `close_composer` ends the one being written.
    fn reload_comments(&mut self, close_composer: bool, cx: &mut Context<Self>) {
        let Some((repo_path, commit, path)) = self.open_file_key() else { return };
        let comments = match self.comments_for(&repo_path, &commit, &path) {
            Ok(comments) => comments,
            Err(message) => return self.fail(message, cx),
        };
        let Some(repo) = self.repo.as_mut() else { return };
        let mode = repo.mode;
        if let Some(file) = repo.file.as_mut() {
            file.comments = comments;
            if close_composer {
                file.composing = None;
            }
            file.rebuild(mode, true);
        }
        cx.notify();
    }
}

// ---- rendering --------------------------------------------------------------------------------

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.release_pictures(window);
        let total = f32::from(window.viewport_size().width);
        let pane_open = self.repo.as_ref().is_some_and(|repo| repo.commit.is_some());
        // The sidebar is the person's to hide, full view of the file pane or not: it stays while they read code
        // (the files changed are in it) until they hide it with its button, a drag or the shortcut.
        let sidebar_width = self.shown_sidebar_width(total, pane_open);
        let sidebar = (sidebar_width > 0.).then(|| self.render_sidebar(sidebar_width, cx));
        // Hidden, the sidebar can still be reached by pointing at the window's left edge, where a thin strip
        // stands in for its divider (to drag it back out as well).
        let sidebar_hidden = sidebar_width <= 0. && self.repo.is_some();
        let divider = if sidebar_hidden {
            self.rail("sidebar-rail", Panel::Sidebar, Some(Splitter::Sidebar), cx)
        } else {
            self.splitter("sidebar-divider", Splitter::Sidebar, cx)
        };
        let peek_width = layout::sidebar_width(self.sidebar_width, total, pane_open);
        let sidebar_peek = (sidebar_hidden && self.peek == Some(Panel::Sidebar)).then(|| {
            let content = self.render_sidebar(peek_width, cx);
            self.peeking("sidebar-peek", Panel::Sidebar, 0., peek_width, content, cx)
        });

        div()
            .relative()
            .size_full()
            .flex()
            .track_focus(&self.focus)
            .key_context("Workspace")
            // Before anything under the pointer: a text field that is clicked focuses itself after this.
            .capture_any_mouse_down(cx.listener(|this, _, window, _| window.focus(&this.focus)))
            .on_action(cx.listener(|this, _: &PreviousFile, window, cx| {
                if this.branch_picker.is_some() {
                    this.step_branch_pick(-1, cx);
                } else if !this.typing(window, cx) {
                    this.step_file(-1, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextFile, window, cx| {
                if this.branch_picker.is_some() {
                    this.step_branch_pick(1, cx);
                } else if !this.typing(window, cx) {
                    this.step_file(1, cx);
                }
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| this.resolver_key(event, window, cx)))
            .bg(rgb(t().bg))
            .text_color(rgb(t().text))
            .text_sm()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| this.drag_divider(event, window, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_resize(cx)))
            .children(sidebar)
            .child(divider)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .children(self.render_operation_bar(cx))
                    .child(div().flex_1().min_h_0().child(self.render_main(window, sidebar_width, total, cx)))
                    .children(self.render_notice(cx)),
            )
            .children(sidebar_peek)
            .children(self.render_overlays(window, cx))
    }
}

impl Workspace {
    /// Hands back to the window the pictures the file pane no longer shows: GPUI keeps a drawn
    /// picture's texture until it is told to let go, so every picture ever looked at would stay.
    fn release_pictures(&mut self, window: &mut Window) {
        let now: Vec<Arc<gpui::RenderImage>> = match self.repo.as_ref().and_then(|repo| repo.file.as_ref()) {
            Some(FileState { images: Some(images), .. }) => images.pictures().cloned().collect(),
            _ => Vec::new(),
        };
        for gone in std::mem::replace(&mut self.shown_pictures, now) {
            if !self.shown_pictures.iter().any(|shown| Arc::ptr_eq(shown, &gone)) {
                window.drop_image(gone).ok();
            }
        }
    }

    /// The sidebar can be hidden only while a repository is open, so there is always a way to pick one.
    pub fn sidebar_shown(&self) -> bool {
        !self.settings.sidebar_hidden || self.repo.is_none()
    }

    /// How wide the sidebar is drawn: nothing while it is hidden.
    fn shown_sidebar_width(&self, total: f32, pane_open: bool) -> f32 {
        if self.sidebar_shown() { layout::sidebar_width(self.sidebar_width, total, pane_open) } else { 0. }
    }

    /// Shows or hides the projects sidebar, and keeps the choice.
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.repo.is_none() {
            return;
        }
        self.settings.sidebar_hidden = self.sidebar_shown();
        self.save_settings();
        cx.notify();
    }

    /// The button that shows or hides the sidebar: a window with its left panel filled in while shown.
    pub(crate) fn sidebar_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let shown = self.sidebar_shown();
        div()
            .id("toggle-sidebar")
            .flex_none()
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(t().hover)))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)))
            .child(ui::file_icon(icons::sidebar(shown)))
    }

    /// A thin draggable line between two panes.
    pub fn splitter(&self, id: &'static str, which: Splitter, cx: &mut Context<Self>) -> AnyElement {
        let active = self.resizing == Some(which);
        // Between the graph and a pane below it the line is horizontal.
        if which == Splitter::PaneHeight {
            return div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .w_full()
                .h(px(5.))
                .flex_none()
                .flex()
                .flex_col()
                .justify_center()
                .cursor(CursorStyle::ResizeUpDown)
                .hover(|style| style.bg(rgb(t().selected)))
                .when(active, |divider| divider.bg(rgb(t().selected)))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.begin_resize(which, cx)))
                .child(div().h(px(if active { 2. } else { 1. })).w_full().bg(rgb(if active { t().accent } else { t().border })))
                .into_any_element();
        }
        div()
            .id(id)
            .w(px(5.))
            .h_full()
            .flex_none()
            .flex()
            .justify_center()
            .cursor(CursorStyle::ResizeLeftRight)
            .hover(|style| style.bg(rgb(t().selected)))
            .when(active, |divider| divider.bg(rgb(t().selected)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.begin_resize(which, cx)))
            .child(div().w(px(if active { 2. } else { 1. })).h_full().bg(rgb(if active { t().accent } else { t().border })))
            .into_any_element()
    }

    fn render_sidebar(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let selected_path = self.repo.as_ref().map(|repo| repo.project.path.clone());
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from);
        // What the open project can say about itself: its branch, and whether anything is waiting to be committed.
        let open_status: Option<(Option<SharedString>, usize)> = self.repo.as_ref().and_then(|repo| match &repo.phase {
            Phase::Ready(view) => Some((view.current_branch.clone(), repo.work.len())),
            _ => None,
        });
        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, project) in self.projects.iter().enumerate() {
            let selected = selected_path.as_deref() == Some(project.path.as_path());
            let (open_path, remove_path) = (project.path.clone(), project.path.clone());
            let muted_line = |child: AnyElement| {
                div().overflow_hidden().line_clamp(1).text_ellipsis().text_size(px(11.)).text_color(rgb(t().muted)).child(child)
            };
            let second: AnyElement = match open_status.as_ref().filter(|_| selected) {
                Some((branch, changes)) => div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(11.))
                    .text_color(rgb(t().muted))
                    .children(branch.clone().map(|branch| div().min_w_0().overflow_hidden().line_clamp(1).text_ellipsis().child(branch)))
                    // A detached HEAD (in the middle of a rebase, say) has no name to separate.
                    .when(branch.is_some(), |row| row.child("·"))
                    .child(if *changes == 0 {
                        div().flex_none().child("clean")
                    } else {
                        div().flex_none().text_color(rgb(t().modified)).child(format!("{changes} change{}", if *changes == 1 { "" } else { "s" }))
                    })
                    .into_any_element(),
                None => muted_line(SharedString::from(ui::tidy_parent(&project.path, home.as_deref())).into_any_element()).into_any_element(),
            };
            // A tile of the project's own color with its first letter: enough to tell projects apart at a glance.
            let lane = t().lane(avatars::hue(&project.name));
            let (tile_fill, tile_ink) = if selected { (lane, theme::text_on(lane)) } else { (theme::mix(t().panel, lane, 0.26), lane) };
            let tile = div()
                .flex_none()
                .size(px(28.))
                .rounded_lg()
                .bg(rgb(tile_fill))
                .text_color(rgb(tile_ink))
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .font_weight(FontWeight::BOLD)
                .child(project.name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into()));
            let row = div()
                .id(("project", i))
                .group("project-row")
                .mx_2()
                .my_px()
                .px_2()
                .py_1p5()
                .flex()
                .items_center()
                .gap_2p5()
                .rounded_lg()
                .cursor_pointer()
                .when(selected, |row| row.bg(rgb(t().selected)))
                .hover(|style| style.bg(rgb(if selected { t().selected } else { t().hover })))
                .on_click(cx.listener(move |this, _, _, cx| this.select_project(open_path.clone(), cx)))
                .child(tile)
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .overflow_hidden()
                                .line_clamp(1)
                                .text_ellipsis()
                                .text_size(px(13.))
                                .font_weight(if selected { FontWeight::BOLD } else { FontWeight::MEDIUM })
                                .text_color(rgb(if selected { t().text_strong } else { t().text }))
                                .child(SharedString::from(project.name.clone())),
                        )
                        .child(second),
                )
                .child(
                    div()
                        .id(("remove", i))
                        .flex_none()
                        .px_1()
                        .rounded_sm()
                        .text_color(rgb(t().muted))
                        .opacity(0.)
                        .group_hover("project-row", |style| style.opacity(1.))
                        .hover(|style| style.text_color(rgb(t().text_strong)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.remove_project(&remove_path, cx);
                        }))
                        .child("×"),
                );
            rows.push(row.into_any_element());
            // The open project's changes sit under its name.
            if selected && let Some(changes) = self.render_changes(cx) {
                rows.push(changes);
            }
        }

        div()
            .w(px(width))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(t().panel))
            .child(
                // One row, as tall as the headers beside it: the title, and the two ways to add a project.
                div()
                    .h(px(38.))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(rgb(t().border))
                    .child(div().min_w_0().flex_1().text_sm().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child("Projects"))
                    .child(ghost("open-folder", "Open…").on_click(cx.listener(|this, _, _, cx| this.open_folder(cx))))
                    .child(
                        ghost("clone-repo", "Clone…")
                            .debug_selector(|| "clone-repo".to_owned())
                            .on_click(cx.listener(|this, _, window, cx| this.start_clone(window, cx))),
                    ),
            )
            .child(div().id("projects").flex_1().overflow_y_scroll().children(rows).when(self.projects.is_empty(), |list| {
                list.child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(rgb(t().muted))
                        .child("No projects yet. Open a folder that is a git repository, or one inside it."),
                )
            }))
            .children(self.render_commit_box(cx))
            .into_any_element()
    }

    fn render_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // What is happening now outranks what happened before.
        if let Some(busy) = &self.busy {
            return Some(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .bg(rgb(t().head_row))
                    .border_t_1()
                    .border_color(rgb(t().selected))
                    .text_color(rgb(t().accent))
                    .child(busy.clone())
                    .into_any_element(),
            );
        }
        let notice = self.notice.clone()?;
        let tone = if notice.warn { t().warning } else { t().added };
        let (bg, border, color) = (crate::theme::mix(t().bg, tone, 0.14), crate::theme::mix(t().bg, tone, 0.35), tone);
        let action = notice.action.clone().map(|(label, action)| {
            button("notice-action", label).on_click(cx.listener(move |this, _, _, cx| {
                this.dismiss_notice(cx);
                this.run_notice_action(action.clone(), cx);
            }))
        });
        Some(
            div()
                .flex_none()
                .px_3()
                .py_1()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .bg(rgb(bg))
                .border_t_1()
                .border_color(rgb(border))
                .text_color(rgb(color))
                .child(div().min_w_0().flex_1().child(notice.text))
                .children(action)
                .child(button("dismiss-notice", "Dismiss").on_click(cx.listener(|this, _, _, cx| this.dismiss_notice(cx))))
                .into_any_element(),
        )
    }

    /// Everything right of the sidebar: the graph in the middle and, once a commit is picked, the
    /// file pane on the right. In full view the pane takes the whole area.
    fn render_main(&mut self, window: &mut Window, sidebar_width: f32, total: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else {
            return centered(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(div().text_color(rgb(t().muted)).child("Pick a project, or open a folder to see its history."))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(button("open-folder-empty", "Open Folder…").on_click(cx.listener(|this, _, _, cx| this.open_folder(cx))))
                            .child(button("clone-empty", "Clone Repository…").on_click(cx.listener(|this, _, window, cx| this.start_clone(window, cx)))),
                    ),
            );
        };
        match &repo.phase {
            Phase::Loading => centered(div().text_color(rgb(t().muted)).child(format!("Loading {}…", repo.project.name))),
            Phase::Failed(message) => centered(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(div().text_color(rgb(t().removed)).child(message.clone()))
                    .child(button("retry", "Try again").on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))),
            ),
            Phase::Ready(_) => {
                let pane_open = repo.commit.is_some();
                let expanded = repo.expanded && pane_open;
                // The graph steps aside in full view, and when it was hidden by hand.
                let graph_gone = pane_open && (expanded || self.graph_hidden);
                // While a file is open the code gets most of the width, unless the pane was dragged to a width.
                let share = if repo.file.is_some() { layout::PANE_SHARE_READING } else { layout::PANE_SHARE };
                let width = layout::pane_width(self.pane_width.or(Some((total - sidebar_width) * share)), total, sidebar_width);
                // What is left for the graph beside the sidebar and the file pane (and its divider).
                let area = total - sidebar_width - if pane_open { width + 6. } else { 0. };
                // The pane under the graph, across the whole width, or beside it.
                let below = pane_open && !graph_gone && self.settings.review_layout == ReviewLayout::Below;
                if below {
                    let main_h = self.main_height(window);
                    let most = (main_h - GRAPH_HEIGHT_MIN).max(PANE_HEIGHT_MIN);
                    let height = self.pane_height.unwrap_or(main_h * PANE_HEIGHT_SHARE).clamp(PANE_HEIGHT_MIN, most);
                    let graph = self.render_middle(total - sidebar_width, cx);
                    let divider = self.splitter("pane-height-divider", Splitter::PaneHeight, cx);
                    let pane = self.render_pane(window, total - sidebar_width, Some(height), cx);
                    return div()
                        .size_full()
                        .relative()
                        .flex()
                        .flex_col()
                        .child(div().flex_1().min_h_0().flex().child(graph))
                        .child(divider)
                        .child(pane)
                        .into_any_element();
                }
                let middle = (!graph_gone).then(|| self.render_middle(area, cx));
                let divider = (pane_open && !graph_gone).then(|| self.splitter("pane-divider", Splitter::Pane, cx));
                let pane = pane_open.then(|| self.render_pane(window, width, None, cx));
                // Hidden by hand, the graph can be reached by pointing at the left edge of the file pane (in full
                // view there is no graph to point at; the sidebar's own edge is by the window's).
                let by_hand = pane_open && !expanded && self.graph_hidden;
                let rail = by_hand.then(|| self.rail("graph-rail", Panel::Graph, None, cx));
                let peek = (by_hand && self.peek == Some(Panel::Graph)).then(|| {
                    let wide = (total - sidebar_width - 120.).clamp(320., 600.);
                    let content = div().w(px(wide)).h_full().flex().flex_col().child(self.render_middle(wide, cx)).into_any_element();
                    self.peeking("graph-peek", Panel::Graph, 0., wide, content, cx)
                });
                div().size_full().relative().flex().children(middle).children(divider).children(rail).children(pane).children(peek).into_any_element()
            }
        }
    }

    /// The branch header and the commit graph.
    fn render_middle(&self, area: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let Phase::Ready(view) = &repo.phase else { return div().into_any_element() };

        // With no remote there is nothing to be ahead of or behind.
        let upstream = view.upstream().filter(|_| !view.remotes.is_empty());
        let current = match &view.current_branch {
            Some(name) => {
                let spare = if view.remotes.is_empty() { SYNC_BUTTONS } else { 0. };
                let room = header_room(area + spare, name, upstream.map(|u| upstream_chars(name, u)), view.base.as_ref().map(base_chars));
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(ui::ring(rgb(t().accent)))
                    .when(room.label, |row| row.child(div().flex_none().text_color(rgb(t().muted)).child("Current branch")))
                    .child(
                        div()
                            .flex_1()
                            // Some of a long name stays; a short one keeps no room it does not use.
                            .min_w(px((name.chars().count() as f32 * 7.6).min(48.)))
                            .overflow_hidden()
                            // The row does not wrap, and text that may not wrap is never cut short with an ellipsis.
                            .whitespace_normal()
                            .line_clamp(1)
                            .text_ellipsis()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(t().text_strong))
                            .child(name.clone()),
                    )
                    .children(upstream.map(|upstream| render_upstream(name, upstream, room.remote)))
                    .children(view.base.clone().filter(|_| room.base).map(|base| self.render_base(base, cx)))
            }
            None => div().text_color(rgb(t().warning)).child("HEAD is detached"),
        };
        let header = div()
            .overflow_hidden()
            .h(px(38.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_4()
            .border_b_1()
            .border_color(rgb(t().border))
            .child(self.sidebar_button(cx))
            .child(current)
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .text_xs()
                    .text_color(rgb(t().muted))
                    .child(if view.scanning {
                        SharedString::from(format!("{} · checking which branches are already merged…", view.timing))
                    } else if view.unchecked > 0 {
                        SharedString::from(format!("{} · {} branches were not checked for merges (it took too long)", view.timing, view.unchecked))
                    } else {
                        view.timing.clone()
                    }),
            )
            .child(
                button("new-branch", "New branch…")
                    .debug_selector(|| "new-branch".to_owned())
                    .on_click(cx.listener(|this, _, window, cx| this.open_new_branch(window, cx))),
            )
            .children((!view.remotes.is_empty()).then(|| self.render_sync_buttons(view.upstream(), cx)))
            .child(button("refresh", "Refresh").on_click(cx.listener(|this, _, _, cx| this.refresh(cx))));

        div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .child(self.render_filter_bar(repo, view, area < 640., cx))
            .children(self.render_rebased_bar(view, cx))
            .child(self.render_graph(area, cx))
            .when(self.legend_open, |center| center.child(self.render_legend(cx)))
            .into_any_element()
    }

    /// Which branches the graph shows, whether merged ones are left out, and the search box.
    fn render_filter_bar(&self, repo: &RepoState, view: &RepoView, narrow: bool, cx: &mut Context<Self>) -> AnyElement {
        let filter = &repo.graph_filter;
        let scope = filter.scope;
        let segment = |id: &'static str, label: &'static str, this: Scope| {
            button(id, label)
                .rounded_none()
                .when(scope == this, |b| b.bg(rgb(t().accent)).text_color(rgb(t().on_accent)).font_weight(FontWeight::BOLD))
                .on_click(cx.listener(move |workspace, _, _, cx| workspace.set_scope(this, cx)))
        };
        let scopes = div()
            .flex()
            .flex_none()
            .rounded_sm()
            .overflow_hidden()
            .child(segment("scope-focus", if narrow { "Focus" } else { "Branch + base" }, Scope::Focus))
            .child(segment("scope-current", if narrow { "Branch" } else { "Branch only" }, Scope::Current))
            .child(segment("scope-local", "Local", Scope::Local))
            .child(segment("scope-all", "All", Scope::All));

        let merged = view.clues.len();
        let hide = filter.hide_merged;
        let hide_merged = bar_checkbox(
            "hide-merged",
            hide,
            match (view.scanning, narrow) {
                (true, true) => "Merged…".to_owned(),
                (true, false) => "Hide merged (checking…)".to_owned(),
                (false, true) => format!("Merged ({merged})"),
                (false, false) => format!("Hide merged ({merged})"),
            },
            cx.listener(|this, _, _, cx| this.toggle_hide_merged(cx)),
        );
        let stashes = bar_checkbox(
            "show-stashes",
            !filter.hide_stashes,
            if narrow { format!("Stash ({})", view.stashes) } else { format!("Stashes ({})", view.stashes) },
            cx.listener(|this, _, _, cx| this.toggle_stashes(cx)),
        );

        // The author and the days the graph is limited to, as chips that open a menu to change them.
        let author_term = gitgui_core::term(&filter.search, "author");
        let date_term = gitgui_core::term(&filter.search, "date")
            .or_else(|| gitgui_core::term(&filter.search, "since").map(|d| format!("from {d}")))
            .or_else(|| gitgui_core::term(&filter.search, "until").map(|d| format!("until {d}")));
        let author_label = match &author_term {
            Some(asked) => {
                // An email or a name that belongs to someone in the history is shown as their name.
                let name = view.people.of(asked).map(|person| person.name.clone());
                format!("Author: {} ▾", name.unwrap_or_else(|| asked.clone()))
            }
            None => "Author ▾".to_owned(),
        };
        let date_label = match date_term.as_deref() {
            Some("today") => "Date: Today ▾".to_owned(),
            Some("yesterday") => "Date: Yesterday ▾".to_owned(),
            Some("7d") => "Date: Last 7 days ▾".to_owned(),
            Some("30d") => "Date: Last 30 days ▾".to_owned(),
            Some("month") => "Date: This month ▾".to_owned(),
            Some(other) => format!("Date: {other} ▾"),
            None => "Date ▾".to_owned(),
        };
        let chip = |id: &'static str, label: String, active: bool, target: crate::menu::MenuTarget| {
            ui::toggle(id, label, active)
                .debug_selector(move || id.to_owned())
                .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| this.open_menu(event.position(), target.clone(), cx)))
        };
        let author_chip = chip("filter-author", author_label, author_term.is_some(), crate::menu::MenuTarget::Authors);
        let date_chip = chip("filter-date", date_label, date_term.is_some(), crate::menu::MenuTarget::Dates);

        let query = filter.query();
        let asks_git = matches!(query, Query::Path(_) | Query::Code(_));
        let status: Option<String> = match query {
            Query::None => None,
            _ if filter.searching => Some("searching…".into()),
            _ if asks_git && filter.found_now().is_none() => Some("Enter to search".into()),
            _ if view.matches.is_empty() => Some("no matches".into()),
            _ => Some(match repo.selected.and_then(|at| view.matches.iter().position(|&ix| ix == at)) {
                Some(at) => format!("{} of {}", at + 1, view.matches.len()),
                None => format!("{} found · Enter for next", view.matches.len()),
            }),
        };
        // Starts at a comfortable width, grows into spare room and gives way before anything else does.
        let search = div()
            .w(px(260.))
            .flex_grow()
            .flex_shrink()
            .min_w(px(if narrow { 70. } else { 160. }))
            .max_w(px(420.))
            .overflow_hidden()
            .child(self.search_input.clone());

        div()
            .w_full()
            .h(px(36.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .overflow_hidden()
            .border_b_1()
            .border_color(rgb(t().border))
            .child(scopes)
            .children(filter.isolate.clone().map(|branch| {
                ui::toggle("isolated", format!("Only {branch} ✕"), true)
                    .debug_selector(|| "isolated".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.clear_isolate(cx)))
            }))
            .child(hide_merged)
            .child(stashes)
            .children((view.sync_count > 0).then(|| {
                bar_checkbox(
                    "show-sync",
                    filter.show_sync,
                    if narrow { format!("Sync ({})", view.sync_count) } else { format!("Sync merges ({})", view.sync_count) },
                    cx.listener(|this, _, _, cx| this.toggle_sync_merges(cx)),
                )
            }))
            .child(author_chip)
            .child(date_chip)
            .child(search)
            .children(status.map(|text| div().flex_none().text_xs().text_color(rgb(t().muted)).child(text)))
            .child(
                ui::ghost("legend", "?")
                    .debug_selector(|| "legend".to_owned())
                    .when(self.legend_open, |button| button.bg(rgb(t().element_hover)).text_color(rgb(t().text_strong)))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_legend(cx))),
            )
            .into_any_element()
    }

    /// "feat/x was rebased onto 3f2a1b9: 3 commits were rewritten" with a button that moves it back, for as long as the
    /// rebase is the last thing that happened to the branch and the rewritten commits are not on the remote yet.
    fn render_rebased_bar(&self, view: &RepoView, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rebased = view.rebased.clone()?;
        // Pushed (the remote has them): going back would put the two sides apart again.
        if matches!(view.upstreams.get(&rebased.branch), Some(Upstream::Tracking { ahead: 0, .. })) {
            return None;
        }
        let short = |id: &str| id.chars().take(7).collect::<String>();
        let onto = rebased.onto.as_deref().map(|onto| format!(" onto {}", short(onto))).unwrap_or_default();
        let count = if rebased.commits > 0 { format!(": {} commit{} rewritten", rebased.commits, if rebased.commits == 1 { "" } else { "s" }) } else { String::new() };
        let said = format!("{} was rebased{onto}{count}. Before, it was at {}.", rebased.branch, short(&rebased.old_tip));
        Some(
            div()
                .debug_selector(|| "rebased-bar".to_owned())
                .flex_none()
                .h(px(32.))
                .px_3()
                .flex()
                .items_center()
                .gap_3()
                .overflow_hidden()
                .bg(rgb(crate::theme::mix(t().bg, t().accent, 0.10)))
                .border_b_1()
                .border_color(rgb(crate::theme::mix(t().bg, t().accent, 0.30)))
                .text_xs()
                .child(div().flex_none().size(px(7.)).rounded_full().bg(rgb(t().accent)))
                .child(div().min_w_0().flex_1().overflow_hidden().line_clamp(1).text_ellipsis().text_color(rgb(t().text)).child(said))
                .child(
                    ui::ghost("undo-rebase", "Undo rebase")
                        .debug_selector(|| "undo-rebase".to_owned())
                        .text_color(rgb(t().accent))
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(cx.listener(move |this, _, window, cx| this.open_dialog(crate::menu::Action::UndoRebase(rebased.clone()), window, cx))),
                )
                .into_any_element(),
        )
    }

    pub fn toggle_legend(&mut self, cx: &mut Context<Self>) {
        self.legend_open = !self.legend_open;
        cx.notify();
    }

    /// The key to the graph's marks, over the top right of the graph. A click anywhere on it closes it.
    fn render_legend(&self, cx: &mut Context<Self>) -> AnyElement {
        use gitgui_core::CommitKind;
        let entry = |icon: AnyElement, title: &'static str, says: &'static str| {
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(div().flex_none().w(px(14.)).h(px(18.)).flex().items_center().child(icon))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_color(rgb(t().text_strong)).font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_color(rgb(t().muted)).child(says)),
                )
        };
        div()
            .id("legend-panel")
            .debug_selector(|| "legend-panel".to_owned())
            .absolute()
            .top(px(80.))
            .right(px(12.))
            .w(px(340.))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_md()
            .bg(rgb(t().panel))
            .border_1()
            .border_color(rgb(t().border))
            .shadow_lg()
            .text_xs()
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.toggle_legend(cx)))
            .child(div().text_sm().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child("What the marks mean"))
            .child(entry(ui::kind_icon(CommitKind::PullRequest).into_any_element(), "Pull request", "Merged with a merge commit."))
            .child(entry(ui::kind_icon(CommitKind::Squash).into_any_element(), "Squashed pull request", "All of a branch's changes in one commit, its title ending in (#N)."))
            .child(entry(ui::kind_icon(CommitKind::Merge).into_any_element(), "Merge", "One branch merged into another."))
            .child(entry(
                ui::kind_icon(CommitKind::Sync).into_any_element(),
                "Sync merge",
                "The trunk merged into a branch to keep it current. Left out of the graph unless Sync merges is ticked.",
            ))
            .child(entry(
                div().text_color(rgb(t().added)).child("✓").into_any_element(),
                "merged into / already in",
                "The branch's commits are part of another branch. \"already in\" means no merge commit brought them: it was fast-forwarded.",
            ))
            .child(entry(
                div().text_color(rgb(t().added)).font_weight(FontWeight::BOLD).child("↑").into_any_element(),
                "Not pushed yet",
                "A commit no remote branch has. A push would send it.",
            ))
            .child(entry(
                div().text_color(rgb(t().modified)).font_weight(FontWeight::BOLD).child("↓").into_any_element(),
                "Not pulled yet",
                "A commit only on the remote, drawn as a hollow dot. As of the last fetch.",
            ))
            .child(entry(
                gpui::img(crate::icons::flag(t().muted)).size(px(11.)).into_any_element(),
                "Tag",
                "A release or version mark on a commit.",
            ))
            .child(entry(
                div().text_color(rgb(t().accent)).font_weight(FontWeight::BOLD).child("≈").into_any_element(),
                "Same changes as another commit",
                "A rebase or a cherry-pick copied it. Point at one to light the other.",
            ))
            .child(entry(
                div().text_color(rgb(t().muted)).child("+N").into_any_element(),
                "Folded lanes",
                "More lines than fit beside the messages are drawn as one gray lane. Press +N above the graph to open them.",
            ))
            .into_any_element()
    }

    /// Fetch, Pull and Push side by side. The one the current branch needs is lit: Push when it has commits the remote
    /// does not or is not on a remote yet, Pull when the remote has commits it does not (pull first, then push).
    fn render_sync_buttons(&self, upstream: Option<&Upstream>, cx: &mut Context<Self>) -> AnyElement {
        let (pull, push) = sync_emphasis(upstream);
        // A fetch the app started on its own is running: quietly greyed, not a banner.
        let fetch = sync_button("fetch", "Fetch", if self.auto_fetching { Emphasis::Idle } else { Emphasis::Plain })
            .rounded_l_sm()
            .on_click(cx.listener(|this, _, _, cx| this.fetch(cx)));
        let pull = sync_button("pull-rebase", "Pull", pull)
            .on_click(cx.listener(|this, _, window, cx| this.choose(crate::menu::Action::PullRebase, window, cx)));
        let push = sync_button("push", "Push", push)
            .rounded_r_sm()
            .on_click(cx.listener(|this, _, window, cx| this.push_current(window, cx)));
        div().flex().flex_none().gap(px(1.)).child(fetch).child(pull).child(push).into_any_element()
    }

    /// "2 ahead · 1 behind release/1.0.0": click it to go to the commit the branch was cut from.
    fn render_base(&self, base: graph::Base, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = |n: usize, word: &str, color: u32| {
            div().text_color(rgb(if n > 0 { color } else { t().muted })).child(format!("{n} {word}"))
        };
        let fork = base.fork.clone();
        div()
            .id("branch-base")
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .rounded_md()
            .text_xs()
            .text_color(rgb(t().muted))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(t().hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.select_commit_id(&fork, cx)))
            .child(count(base.ahead, "ahead", t().added))
            .child("·")
            .child(count(base.behind, "behind", t().modified))
            .child(div().text_color(rgb(t().text)).child(base.name))
    }

    fn render_graph(&self, area: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let Phase::Ready(view) = &repo.phase else { return div().into_any_element() };
        let count = view.entries.len();
        let width = view.graph_width;
        let cols = layout::Columns::fit(area, width);
        // More lanes than fit beside the messages: a button after "Graph" folds the far ones into one, or unfolds them.
        let lanes_button = (view.folded_lanes > 0).then(|| {
            let open = self.settings.all_lanes;
            let label = if open { "fold".to_owned() } else { format!("+{}", view.folded_lanes) };
            div()
                .id("lanes")
                .debug_selector(|| "lanes".to_owned())
                .flex_none()
                .px_1()
                .rounded_sm()
                .border_1()
                .border_color(rgb(t().border))
                .text_xs()
                .text_color(rgb(t().muted))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(t().hover)))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_all_lanes(cx)))
                .child(label)
                .into_any_element()
        });

        div()
            .id("graph")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            // The line the pointer was on goes back when the pointer leaves the list.
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.hover_graph_line(None, None, cx);
                }
            }))
            .child(graph::columns(width, cols, lanes_button))
            .child(
                uniform_list(
                    "commits",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                        // Authors' pictures first: asking for one needs the workspace, not just the rows. Only the
                        // rows that draw one ask: a plain dot has no use for a picture.
                        let faces = this.settings.graph_faces;
                        let emails: Vec<Option<String>> = match this.repo.as_ref().map(|repo| (&repo.phase, repo.selected)) {
                            Some((Phase::Ready(view), selected)) => {
                                let highlight = selected.and_then(|ix| view.entries.get(ix)).map(|entry| entry.row.lineage);
                                range
                                    .clone()
                                    .map(|ix| {
                                        let entry = view.entries.get(ix)?;
                                        (cols.author || graph::shows_face(faces, entry, highlight)).then(|| entry.person.avatar_email.clone())
                                    })
                                    .collect()
                            }
                            _ => Vec::new(),
                        };
                        let mut avatars: Vec<Avatar> =
                            emails.iter().map(|email| email.as_ref().map_or(Avatar::None, |email| this.avatar_for(email, cx))).collect();
                        avatars.reverse();
                        let Some(repo) = this.repo.as_ref() else { return Vec::new() };
                        let Phase::Ready(view) = &repo.phase else { return Vec::new() };
                        let selected = repo.selected;
                        let density = graph::Density::from_settings(&this.settings);
                        // The selected commit's branch line comes forward; the others step back.
                        let highlight = selected.and_then(|ix| view.entries.get(ix)).map(|entry| entry.row.lineage);
                        range
                            .filter_map(|ix| {
                                let entry = view.entries.get(ix)?;
                                let avatar = avatars.pop().unwrap_or(Avatar::None);
                                Some(graph::render_entry(
                                    ix,
                                    entry,
                                    avatar,
                                    view.graph_width,
                                    selected == Some(ix),
                                    highlight,
                                    this.graph_hover,
                                    entry.commit.is_some() && entry.commit == this.twin_hover,
                                    cols,
                                    faces,
                                    density,
                                    cx,
                                ))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.graph_scroll.clone())
                .flex_1(),
            )
            .into_any_element()
    }
}

/// A small checkbox and its label, for the filter bar.
fn bar_checkbox(
    id: &'static str,
    on: bool,
    label: String,
    toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .text_xs()
        .cursor_pointer()
        .text_color(rgb(if on { t().text } else { t().muted }))
        .hover(|style| style.text_color(rgb(t().text_strong)))
        .on_click(toggle)
        .child(
            div()
                .size(px(13.))
                .rounded_sm()
                .border_1()
                .border_color(rgb(if on { t().accent } else { t().muted }))
                .when(on, |b| b.bg(rgb(t().accent)))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(t().on_accent))
                .text_size(px(10.))
                .when(on, |b| b.child("✓")),
        )
        .child(label)
}

fn layout_of(saved: FileLayout) -> Layout {
    match saved {
        FileLayout::Tree => Layout::Tree,
        FileLayout::Flat => Layout::Flat,
    }
}

fn file_layout_of(layout: Layout) -> FileLayout {
    match layout {
        Layout::Tree => FileLayout::Tree,
        Layout::Flat => FileLayout::Flat,
    }
}

fn mode_of(saved: DiffMode) -> Mode {
    match saved {
        DiffMode::Unified => Mode::Unified,
        DiffMode::Split => Mode::Split,
    }
}

fn diff_mode_of(mode: Mode) -> DiffMode {
    match mode {
        Mode::Unified => DiffMode::Unified,
        Mode::Split => DiffMode::Split,
    }
}

/// How a button of the Fetch, Pull and Push group looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Emphasis {
    /// The thing to do next.
    Lit,
    Plain,
    /// Nothing to do: there is no remote branch to pull from, or no branch to push.
    Idle,
}

/// How Pull and Push look for where the current branch stands (`None` on a detached HEAD).
pub(crate) fn sync_emphasis(upstream: Option<&Upstream>) -> (Emphasis, Emphasis) {
    match upstream {
        None => (Emphasis::Idle, Emphasis::Idle),
        Some(Upstream::None | Upstream::Gone { .. }) => (Emphasis::Idle, Emphasis::Lit),
        // A push of a branch the remote has moved on from is rejected: pull first.
        Some(Upstream::Tracking { behind, .. }) if *behind > 0 => (Emphasis::Lit, Emphasis::Plain),
        Some(Upstream::Tracking { ahead, .. }) if *ahead > 0 => (Emphasis::Plain, Emphasis::Lit),
        Some(Upstream::Tracking { .. }) => (Emphasis::Plain, Emphasis::Plain),
    }
}

/// One button of the Fetch, Pull and Push group: a `button` whose fill says how much it matters now. The caller rounds
/// the outer corners.
fn sync_button(id: &'static str, label: &'static str, look: Emphasis) -> gpui::Stateful<gpui::Div> {
    let (fill, hover) = match look {
        Emphasis::Lit => (theme::mix(t().element, t().accent, 0.3), theme::mix(t().element, t().accent, 0.45)),
        Emphasis::Plain | Emphasis::Idle => (t().element, t().element_hover),
    };
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex_none()
        .px_2()
        .h(px(22.))
        .flex()
        .items_center()
        .bg(rgb(fill))
        .text_xs()
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(hover)))
        .when(look == Emphasis::Lit, |b| b.text_color(rgb(t().text_strong)).font_weight(FontWeight::SEMIBOLD))
        .when(look == Emphasis::Idle, |b| b.text_color(rgb(t().muted)))
        .child(label)
}

/// What the branch header has room for beside the current branch's name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeaderRoom {
    /// The words "Current branch".
    pub label: bool,
    /// What the branch was cut from, and how far apart they are.
    pub base: bool,
    /// The remote's name after the ↑↓ counts.
    pub remote: bool,
}

/// About what the header's other parts take: the sidebar button, New branch, Fetch Pull Push, Refresh, gaps, padding;
/// and of that, Fetch Pull Push, which a repository with no remote does not show.
const HEADER_FIXED: f32 = 420.;
const SYNC_BUTTONS: f32 = 110.;

/// What fits in a header `area` wide, by a rough measure of the text (about 7.6 px a bold character, 6.4 a small
/// one): `upstream` is the length of the ↑↓ part and of the remote's name, `base` the length of the base's words.
/// What says least goes first: the words "Current branch", then the base, then the remote's name; the branch's own
/// name is cut short last, and the counts stay.
pub(crate) fn header_room(area: f32, name: &str, upstream: Option<(usize, usize)>, base: Option<usize>) -> HeaderRoom {
    let small = |chars: usize| chars as f32 * 6.4;
    let room = area - HEADER_FIXED;
    let name = name.chars().count() as f32 * 7.6;
    let (counts, remote) = upstream.map_or((0., 0.), |(counts, remote)| (small(counts) + 8., small(remote) + 4.));
    let base_w = base.map_or(0., |chars| small(chars) + 24.);
    let show_remote = room >= name.min(160.) + counts + remote;
    let remote = if show_remote { remote } else { 0. };
    let show_base = base.is_some() && room >= name.min(220.) + counts + remote + base_w;
    let base_w = if show_base { base_w } else { 0. };
    HeaderRoom { label: room >= name + counts + remote + base_w + 108., base: show_base, remote: show_remote }
}

/// How long the parts of [`render_upstream`] are: the counts (or the words said instead), and the remote's name.
fn upstream_chars(branch: &str, upstream: &Upstream) -> (usize, usize) {
    let count = |n: usize| if n > 0 { 1 + n.to_string().len() } else { 0 };
    match upstream {
        Upstream::None => ("not on a remote yet".len(), 0),
        // "remote gone", or "origin/x was deleted" when there is room.
        Upstream::Gone { name, .. } => (11, (name.chars().count() + 12).saturating_sub(11)),
        Upstream::Tracking { ahead, behind, .. } => {
            let counts = (count(*ahead) + count(*behind)).max(1) + usize::from(*ahead > 0 && *behind > 0);
            (counts, upstream_shown(branch, upstream).map_or(0, |shown| shown.chars().count() + 1))
        }
    }
}

/// The remote branch as the header names it: the remote alone when the branch there has this branch's name.
fn upstream_shown<'a>(branch: &str, upstream: &'a Upstream) -> Option<&'a str> {
    match upstream {
        Upstream::Tracking { name, remote, .. } | Upstream::Gone { name, remote } => {
            Some(if name.strip_prefix(remote.as_str()).and_then(|rest| rest.strip_prefix('/')) == Some(branch) { remote } else { name })
        }
        Upstream::None => None,
    }
}

/// How long "2 ahead · 1 behind develop" is.
fn base_chars(base: &graph::Base) -> usize {
    format!("{} ahead · {} behind {}", base.ahead, base.behind, base.name).chars().count()
}

/// "↑2 ↓1 origin" beside the current branch: what a push would send and a pull would bring, as of the last fetch; a
/// check when there is neither. The remote branch is named in full only when its name is not the branch's own, and
/// only with `remote` room for it.
fn render_upstream(branch: &str, upstream: &Upstream, remote: bool) -> AnyElement {
    let chip = div()
        .debug_selector(|| "branch-upstream".to_owned())
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(rgb(t().muted));
    match upstream {
        Upstream::None => chip.child("not on a remote yet"),
        Upstream::Gone { name, .. } => chip.text_color(rgb(t().warning)).child(if remote { format!("{name} was deleted") } else { "remote gone".to_owned() }),
        Upstream::Tracking { ahead, behind, .. } => chip
            .when(*ahead == 0 && *behind == 0, |chip| chip.child("✓"))
            .when(*ahead > 0, |chip| chip.child(div().text_color(rgb(t().added)).child(format!("↑{ahead}"))))
            .when(*behind > 0, |chip| chip.child(div().text_color(rgb(t().modified)).child(format!("↓{behind}"))))
            .when_some(upstream_shown(branch, upstream).filter(|_| remote), |chip, shown| chip.child(SharedString::from(shown.to_owned()))),
    }
    .into_any_element()
}

fn centered(content: impl IntoElement) -> AnyElement {
    div().size_full().flex().items_center().justify_center().child(content).into_any_element()
}

/// Opens the folder given on the command line, or else the first project from last time.
pub fn open_initial(workspace: &Entity<Workspace>, cx: &mut App) {
    workspace.update(cx, |this, cx| match std::env::args().nth(1) {
        Some(path) => this.add_folder(Path::new(&path), cx),
        None => {
            if let Some(first) = this.projects.first().map(|p| p.path.clone()) {
                this.select_project(first, cx);
            }
        }
    });
}
