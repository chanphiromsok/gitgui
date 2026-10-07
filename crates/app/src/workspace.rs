//! The app's state and actions: projects in the sidebar, the open repository, the selected commit,
//! the open file, and its comments.
//!
//! Git work runs on a background thread. Each result is checked against what is selected when it
//! arrives, so a slow answer for a commit you have already left is dropped, not shown.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gitgui_core::{
    Backend, BranchTip, Commit, CommitDetail, Evidence, FileChange, FileDiff, GitCli, Layout, Lineage, LogOptions,
    MergeClue, Operation, TreeRow, scan_inputs, visible_rows,
};
use gitgui_store::{Comment, NewComment, Project, Settings, Store};
use gpui::{
    AnyElement, App, Context, CursorStyle, Entity, FontWeight, ListAlignment, ListState, MouseButton, MouseMoveEvent,
    PathPromptOptions, SharedString, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};

use crate::graph::{self, Entry};
use crate::layout;
use crate::menu::{Dialog, MenuState, Notice, NoticeAction};
use crate::rows::{Anchor, DisplayRow, Mode, display_rows};
use crate::text_input::{TextInput, TextInputEvent};
use crate::ui::{self, ACCENT, BG, BORDER, MUTED, PANEL, TEXT, WARNING, button};

const LOAD_LIMIT: usize = 20_000;
const DIFF_CONTEXT: u32 = 3;

pub enum Phase<T> {
    Loading,
    Failed(SharedString),
    Ready(T),
}

pub struct RepoView {
    /// The commits as read, newest first. The rows are made from these, and made again when the
    /// grouping, a fold, or the merge scan changes what to show.
    pub commits: Vec<Commit>,
    pub changed: usize,
    pub entries: Vec<Entry>,
    pub graph_width: f32,
    pub current_branch: Option<SharedString>,
    pub timing: SharedString,
    read: Duration,
    pub lineages: Vec<Lineage>,
    pub names: Vec<Option<String>>,
    /// Branches found to be merged already, once the scan has run.
    pub clues: Vec<MergeClue>,
    /// Branch name → the commit it pointed at when scanned.
    pub tips: HashMap<String, String>,
    /// The scan is still running.
    pub scanning: bool,
    /// Branches the scan ran out of time before checking.
    pub unchecked: usize,
}

impl RepoView {
    /// Tip commit of each squash-merged branch → the commit that carries its changes.
    fn squashed(&self) -> HashMap<String, String> {
        self.clues
            .iter()
            .filter(|clue| clue.evidence != Evidence::Contained)
            .filter_map(|clue| Some((self.tips.get(&clue.branch)?.clone(), clue.commit.clone()?)))
            .collect()
    }

    /// Makes the rows again from the commits.
    fn rebuild(&mut self, settings: &Settings, collapsed: &HashSet<String>) {
        let started = Instant::now();
        let squashed = self.squashed();
        let built = graph::build_entries(
            &self.commits,
            &graph::Options { changed: self.changed, group: settings.group_by_parent, squashed: &squashed, collapsed },
        );
        self.entries = built.entries;
        graph::apply_clues(&mut self.entries, &self.clues);
        self.graph_width = graph::graph_width(built.widest);
        self.lineages = built.lineages;
        self.names = built.names;
        self.timing = SharedString::from(format!(
            "{} commits · read {:.0} ms · layout {:.1} ms · {} lanes",
            self.commits.len(),
            self.read.as_secs_f64() * 1000.,
            started.elapsed().as_secs_f64() * 1000.,
            built.widest,
        ));
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
    pub diff: FileDiff,
    pub comments: Vec<Comment>,
    pub composing: Option<Anchor>,
    pub rows: Vec<DisplayRow>,
    pub list: ListState,
}

impl FileState {
    fn new(index: usize) -> Self {
        Self {
            index,
            phase: Phase::Loading,
            diff: FileDiff::default(),
            comments: Vec::new(),
            composing: None,
            rows: Vec::new(),
            // Rows this far past the edge are built ahead of the scroll; more only costs frame time.
            list: ListState::new(0, ListAlignment::Top, px(120.)),
        }
    }

    /// Rebuilds the rows from the diff, comments and composer. Scroll stays where it was unless
    /// the whole layout changed (a different mode).
    pub fn rebuild(&mut self, mode: Mode, keep_scroll: bool) {
        let top = self.list.logical_scroll_top();
        self.rows = display_rows(&self.diff, mode, &self.comments, self.composing);
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
    /// Which load this is, so a slow answer for an earlier one is ignored.
    pub generation: u64,
}

impl RepoState {
    fn loading(project: Project, generation: u64) -> Self {
        Self {
            collapsed: HashSet::new(),
            generation,
            project,
            phase: Phase::Loading,
            selected: None,
            commit: None,
            file: None,
            mode: Mode::Unified,
            expanded: false,
            layout: Layout::Tree,
            files_visible: true,
            filter: String::new(),
            file_rows: Vec::new(),
        }
    }

    /// Makes the graph rows again and keeps the same commit selected if it is still shown.
    fn rebuild(&mut self, settings: &Settings) {
        let selected = self.selected_id();
        if let Phase::Ready(view) = &mut self.phase {
            view.rebuild(settings, &self.collapsed);
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
    fn refresh_file_rows(&mut self) {
        let rows = match self.commit.as_ref().map(|commit| &commit.phase) {
            Some(Phase::Ready(view)) => visible_rows(&view.files, &self.filter, self.layout),
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
}

pub struct Workspace {
    store: Option<Store>,
    pub projects: Vec<Project>,
    pub notice: Option<Notice>,
    pub repo: Option<RepoState>,
    pub input: Entity<TextInput>,
    pub filter_input: Entity<TextInput>,
    /// The text field in the rename / new branch / new tag dialog.
    pub dialog_input: Entity<TextInput>,
    pub settings: Settings,
    pub settings_open: bool,
    pub menu: Option<MenuState>,
    pub dialog: Option<Dialog>,
    /// What is being done right now, while a git operation runs.
    pub busy: Option<SharedString>,
    loads: u64,
    graph_scroll: UniformListScrollHandle,
    /// The sidebar's width as last dragged; the window may allow less (see `layout`).
    pub sidebar_width: f32,
    /// The file pane's width as last dragged; `None` until it is, then it takes a share of the room.
    pub pane_width: Option<f32>,
    pub resizing: Option<Splitter>,
}

struct RepoData {
    commits: Vec<Commit>,
    current_branch: Option<String>,
    changed: usize,
    read: Duration,
    /// A merge, rebase or cherry-pick that an earlier session left half done.
    in_progress: Option<Operation>,
}

fn read_repo(path: &Path) -> Result<RepoData, gitgui_core::Error> {
    let git = GitCli::new(path);
    let started = Instant::now();
    let commits = git.log(&LogOptions { max_count: Some(LOAD_LIMIT), skip: 0 })?;
    let current_branch = git.current_branch()?;
    let changed = git.changed_files()?;
    let in_progress = git.in_progress();
    Ok(RepoData { commits, current_branch, changed, read: started.elapsed(), in_progress })
}

fn read_commit(path: &Path, id: &str) -> Result<CommitView, gitgui_core::Error> {
    let git = GitCli::new(path);
    let detail = git.commit_detail(id)?;
    let files = git.commit_files(id)?;
    let additions = files.iter().filter_map(|f| f.additions).sum();
    let deletions = files.iter().filter_map(|f| f.deletions).sum();
    Ok(CommitView { detail, files, additions, deletions })
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_store(Store::open_default(), cx)
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

        let dialog_input = cx.new(|cx| TextInput::new("Type a name…", cx));
        cx.subscribe(&dialog_input, |this, _input, event: &TextInputEvent, cx| match event {
            TextInputEvent::Submit => this.confirm_dialog(cx),
            TextInputEvent::Cancel => this.cancel_dialog(cx),
            TextInputEvent::Changed => {}
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
        Self {
            store,
            projects,
            notice: notice.map(Notice::warn),
            repo: None,
            input,
            filter_input,
            dialog_input,
            settings,
            settings_open: false,
            menu: None,
            dialog: None,
            busy: None,
            loads: 0,
            graph_scroll: UniformListScrollHandle::new(),
            sidebar_width: layout::SIDEBAR_DEFAULT,
            pane_width: None,
            resizing: None,
        }
    }

    // ---- resizing ---------------------------------------------------------------------------

    pub fn begin_resize(&mut self, which: Splitter, cx: &mut Context<Self>) {
        self.resizing = Some(which);
        cx.notify();
    }

    pub fn end_resize(&mut self, cx: &mut Context<Self>) {
        if self.resizing.take().is_some() {
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
            Splitter::Sidebar => self.sidebar_width = layout::sidebar_at(x, total, pane_open),
            Splitter::Pane => {
                let sidebar = layout::sidebar_width(self.sidebar_width, total, pane_open);
                self.pane_width = Some(layout::pane_at(x, total, sidebar));
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

    fn fail(&mut self, message: impl std::fmt::Display, cx: &mut Context<Self>) {
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
    pub fn add_folder(&mut self, path: &Path, cx: &mut Context<Self>) {
        let root = match GitCli::new(path).toplevel() {
            Ok(root) => root,
            Err(_) => return self.fail(format!("{} is not inside a git repository.", path.display()), cx),
        };
        let Some(store) = &self.store else {
            return self.fail("There is no folder to save projects in.", cx);
        };
        match store.add_project(&root) {
            Ok(project) => {
                if !self.projects.iter().any(|p| p.path == project.path) {
                    self.projects.push(project.clone());
                }
                self.select_project(project.path, cx);
            }
            Err(err) => self.fail(err, cx),
        }
    }

    pub fn remove_project(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(store) = &self.store
            && let Err(err) = store.remove_project(path)
        {
            return self.fail(err, cx);
        }
        self.projects.retain(|p| p.path != path);
        if self.repo.as_ref().is_some_and(|repo| repo.project.path == path) {
            self.repo = None;
        }
        cx.notify();
    }

    pub fn select_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| p.path == path).cloned() else { return };
        self.loads += 1;
        let generation = self.loads;
        self.repo = Some(RepoState::loading(project, generation));
        cx.notify();

        let read_path = path.clone();
        self.spawn_load(
            cx,
            move || read_repo(&read_path),
            move |this, result, cx| {
                let settings = this.settings.clone();
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.generation == generation) else { return };
                match result {
                    Ok(data) => {
                        let in_progress = data.in_progress;
                        let mut view = RepoView {
                            commits: data.commits,
                            changed: data.changed,
                            entries: Vec::new(),
                            graph_width: 0.,
                            current_branch: data.current_branch.map(SharedString::from),
                            timing: SharedString::default(),
                            read: data.read,
                            lineages: Vec::new(),
                            names: Vec::new(),
                            clues: Vec::new(),
                            tips: HashMap::new(),
                            scanning: true,
                            unchecked: 0,
                        };
                        view.rebuild(&settings, &repo.collapsed);
                        let (branches, targets) = scan_inputs(&view.commits);
                        view.tips = branches.iter().chain(&targets).map(|t| (t.name.clone(), t.id.clone())).collect();
                        view.scanning = !branches.is_empty() && !targets.is_empty();
                        let scan = view.scanning;
                        repo.phase = Phase::Ready(view);
                        if let Some(operation) = in_progress.filter(|_| this.notice.is_none()) {
                            this.notice = Some(Notice {
                                text: format!(
                                    "A {} is in progress in this repository, left half done. Finish it in your editor or terminal, or abort it.",
                                    operation.name()
                                )
                                .into(),
                                warn: true,
                                action: Some((format!("Abort {}", operation.name()).into(), NoticeAction::Abort(operation))),
                            });
                        }
                        if scan {
                            this.start_merge_scan(path, generation, branches, targets, cx);
                        }
                    }
                    Err(err) => repo.phase = Phase::Failed(err.to_string().into()),
                }
                cx.notify();
            },
        );
    }

    /// Looks, in the background, for branches already merged by a squash or rebase, then redraws with
    /// what it found.
    fn start_merge_scan(
        &mut self,
        path: PathBuf,
        generation: u64,
        branches: Vec<BranchTip>,
        targets: Vec<BranchTip>,
        cx: &mut Context<Self>,
    ) {
        self.spawn_load(
            cx,
            move || GitCli::new(&path).merge_clues(&branches, &targets),
            move |this, result, cx| {
                let settings = this.settings.clone();
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.generation == generation) else { return };
                if let Phase::Ready(view) = &mut repo.phase {
                    view.scanning = false;
                    if let Ok(scan) = result {
                        view.clues = scan.clues;
                        view.unchecked = scan.unchecked;
                    }
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

    // ---- commits ----------------------------------------------------------------------------

    pub fn select_entry(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        let Phase::Ready(view) = &repo.phase else { return };
        // The working-tree row has no commit to show yet.
        let Some(id) = view.entries.get(ix).and_then(|entry| entry.commit.clone()) else { return };

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
        self.spawn_load(
            cx,
            move || read_commit(&read_path, &read_id),
            move |this, result, cx| {
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

        let read_path = path.clone();
        let read_id = id.clone();
        let read_change = change.clone();
        self.spawn_load(
            cx,
            move || GitCli::new(&read_path).file_diff(&read_id, &read_change, DIFF_CONTEXT),
            move |this, result, cx| {
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.project.path == path) else { return };
                let mode = repo.mode;
                let still_open = repo.commit.as_ref().is_some_and(|commit| commit.id == id);
                let Some(file) = repo.file.as_mut().filter(|file| still_open && file.index == index) else { return };
                match result {
                    Ok(diff) => {
                        file.diff = diff;
                        file.phase = Phase::Ready(());
                        file.rebuild(mode, false);
                    }
                    Err(err) => file.phase = Phase::Failed(err.to_string().into()),
                }
                cx.notify();
            },
        );
    }

    pub fn close_file(&mut self, cx: &mut Context<Self>) {
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
        if self.settings_open {
            return self.close_settings(cx);
        }
        let Some(repo) = self.repo.as_mut() else { return };
        if repo.expanded {
            repo.expanded = false;
            cx.notify();
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

    /// Closes the file pane and deselects the commit.
    pub fn close_pane(&mut self, cx: &mut Context<Self>) {
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

    pub fn set_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        if repo.layout != layout {
            repo.layout = layout;
            repo.refresh_file_rows();
            cx.notify();
        }
    }

    pub fn set_filter(&mut self, filter: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        if repo.filter != filter {
            repo.filter = filter;
            repo.refresh_file_rows();
            cx.notify();
        }
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
        cx.notify();
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

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        if repo.mode == mode {
            return;
        }
        repo.mode = mode;
        if let Some(file) = repo.file.as_mut() {
            file.rebuild(mode, false);
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
        let total = f32::from(window.viewport_size().width);
        let pane_open = self.repo.as_ref().is_some_and(|repo| repo.commit.is_some());
        let sidebar_width = layout::sidebar_width(self.sidebar_width, total, pane_open);

        div()
            .relative()
            .size_full()
            .flex()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_sm()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| this.drag_divider(event, window, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_resize(cx)))
            .child(self.render_sidebar(sidebar_width, cx))
            .child(self.splitter("sidebar-divider", Splitter::Sidebar, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(div().flex_1().min_h_0().child(self.render_main(window, sidebar_width, total, cx)))
                    .children(self.render_notice(cx)),
            )
            .children(self.render_overlays(window, cx))
    }
}

impl Workspace {
    /// A thin draggable line between two panes.
    pub fn splitter(&self, id: &'static str, which: Splitter, cx: &mut Context<Self>) -> AnyElement {
        let active = self.resizing == Some(which);
        div()
            .id(id)
            .w(px(5.))
            .h_full()
            .flex_none()
            .flex()
            .justify_center()
            .cursor(CursorStyle::ResizeLeftRight)
            .hover(|style| style.bg(rgb(0x24323f)))
            .when(active, |divider| divider.bg(rgb(0x24323f)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.begin_resize(which, cx)))
            .child(div().w(px(if active { 2. } else { 1. })).h_full().bg(rgb(if active { ACCENT } else { BORDER })))
            .into_any_element()
    }

    fn render_sidebar(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let selected_path = self.repo.as_ref().map(|repo| repo.project.path.clone());
        let rows: Vec<_> = self
            .projects
            .iter()
            .enumerate()
            .map(|(i, project)| {
                let selected = selected_path.as_deref() == Some(project.path.as_path());
                let (open_path, remove_path) = (project.path.clone(), project.path.clone());
                div()
                    .id(("project", i))
                    .group("project-row")
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(rgb(ui::SELECTED)))
                    .hover(|style| style.bg(rgb(if selected { ui::SELECTED } else { ui::HOVER })))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_project(open_path.clone(), cx)))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .font_weight(if selected { FontWeight::BOLD } else { FontWeight::MEDIUM })
                                    .child(SharedString::from(project.name.clone())),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(SharedString::from(project.path.display().to_string())),
                            ),
                    )
                    .child(
                        div()
                            .id(("remove", i))
                            .flex_none()
                            .px_1()
                            .text_color(rgb(MUTED))
                            .opacity(0.)
                            .group_hover("project-row", |style| style.opacity(1.))
                            .hover(|style| style.text_color(rgb(0xffffff)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_project(&remove_path, cx);
                            }))
                            .child("×"),
                    )
            })
            .collect();

        div()
            .w(px(width))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(PANEL))
            .child(
                div()
                    .h(px(34.))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(div().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(MUTED)).child("PROJECTS"))
                    .child(button("open-folder", "Open Folder…").on_click(cx.listener(|this, _, _, cx| this.open_folder(cx)))),
            )
            .child(div().id("projects").flex_1().overflow_y_scroll().children(rows).when(self.projects.is_empty(), |list| {
                list.child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child("No projects yet. Open a folder that is a git repository, or one inside it."),
                )
            }))
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
                    .bg(rgb(0x14283a))
                    .border_t_1()
                    .border_color(rgb(0x1f4a6e))
                    .text_color(rgb(ACCENT))
                    .child(busy.clone())
                    .into_any_element(),
            );
        }
        let notice = self.notice.clone()?;
        let (bg, border, color) = if notice.warn { (0x3a2f12, 0x5a4a1a, WARNING) } else { (0x12301f, 0x1f5a3a, 0x7ee2a8) };
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
                    .child(div().text_color(rgb(MUTED)).child("Pick a project, or open a folder to see its history."))
                    .child(button("open-folder-empty", "Open Folder…").on_click(cx.listener(|this, _, _, cx| this.open_folder(cx)))),
            );
        };
        match &repo.phase {
            Phase::Loading => centered(div().text_color(rgb(MUTED)).child(format!("Loading {}…", repo.project.name))),
            Phase::Failed(message) => centered(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(div().text_color(rgb(ui::REMOVED)).child(message.clone()))
                    .child(button("retry", "Try again").on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))),
            ),
            Phase::Ready(_) => {
                let pane_open = repo.commit.is_some();
                let expanded = repo.expanded && pane_open;
                let middle = (!expanded).then(|| self.render_middle(pane_open, cx));
                let width = layout::pane_width(self.pane_width, total, sidebar_width);
                let divider = (pane_open && !expanded).then(|| self.splitter("pane-divider", Splitter::Pane, cx));
                let pane = pane_open.then(|| self.render_pane(window, width, cx));
                div().size_full().flex().children(middle).children(divider).children(pane).into_any_element()
            }
        }
    }

    /// The branch header and the commit graph.
    fn render_middle(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let Phase::Ready(view) = &repo.phase else { return div().into_any_element() };

        let current = match &view.current_branch {
            Some(name) => div()
                .flex()
                .items_center()
                .gap_2()
                .child(ui::ring(rgb(ACCENT)))
                .child(div().text_color(rgb(MUTED)).child("Current branch"))
                .child(div().font_weight(FontWeight::BOLD).text_color(rgb(0xffffff)).child(name.clone())),
            None => div().text_color(rgb(WARNING)).child("HEAD is detached"),
        };
        let header = div()
            .h(px(34.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_4()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(current)
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if view.scanning {
                        SharedString::from(format!("{} · checking which branches are already merged…", view.timing))
                    } else if view.unchecked > 0 {
                        SharedString::from(format!("{} · {} branches were not checked for merges (it took too long)", view.timing, view.unchecked))
                    } else {
                        view.timing.clone()
                    }),
            )
            .child(button("refresh", "Refresh").on_click(cx.listener(|this, _, _, cx| this.refresh(cx))));

        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .child(self.render_graph(compact, cx))
            .into_any_element()
    }

    fn render_graph(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let Phase::Ready(view) = &repo.phase else { return div().into_any_element() };
        let count = view.entries.len();
        let width = view.graph_width;

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(graph::columns(width, compact))
            .child(
                uniform_list(
                    "commits",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                        let Some(repo) = this.repo.as_ref() else { return Vec::new() };
                        let Phase::Ready(view) = &repo.phase else { return Vec::new() };
                        let selected = repo.selected;
                        // The selected commit's branch line comes forward; the others step back.
                        let highlight = selected.and_then(|ix| view.entries.get(ix)).map(|entry| entry.row.lineage);
                        range
                            .filter_map(|ix| {
                                let entry = view.entries.get(ix)?;
                                Some(graph::render_entry(ix, entry, view.graph_width, selected == Some(ix), highlight, compact, cx))
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
