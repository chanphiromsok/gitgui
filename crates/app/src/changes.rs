//! The working tree, under the open project in the sidebar: what has changed since the last commit,
//! staged and not, a click away from its diff; staging and unstaging; and a box to commit with.
//!
//! The working tree opens in the file pane as if it were a commit (id [`WORKTREE`]), so the diff,
//! colors and pictures work as they do for history. Its file list is the one in the sidebar.

use gitgui_core::{CommitDetail, FileStatus, GitCli, WorkFile};
use gpui::{AnyElement, Context, FontWeight, SharedString, div, prelude::*, px, rgb};

use crate::icons;
use crate::menu::{Notice, explain};
use crate::theme::t;
use crate::ui::{self, button};
use crate::workspace::{CommitState, CommitView, Phase, Workspace};

/// The id the working tree goes by when it is open in the file pane.
pub const WORKTREE: &str = "WORKTREE";

/// What opens once the working tree has been read again.
pub(crate) enum AfterWork {
    /// The file that was open, where it has moved to.
    Reopen,
    /// The next file with conflicts after the one just resolved (by path).
    NextConflict(String),
}

const ROW_H: f32 = 22.;

/// The letter git uses for a change, and its color.
fn letter(file: &WorkFile) -> (&'static str, u32) {
    if file.conflicted {
        return ("!", t().removed);
    }
    if file.untracked {
        return ("U", t().added);
    }
    match file.change.status {
        FileStatus::Added => ("A", t().added),
        FileStatus::Deleted => ("D", t().removed),
        FileStatus::Renamed => ("R", t().accent),
        FileStatus::Copied => ("C", t().accent),
        FileStatus::TypeChanged => ("T", t().modified),
        FileStatus::Modified => ("M", t().modified),
    }
}

/// The working tree as a commit for the file pane: its files are the changes, staged first.
fn work_view(files: &[WorkFile]) -> CommitView {
    CommitView {
        detail: CommitDetail {
            id: WORKTREE.to_owned(),
            parents: Vec::new(),
            author: String::new(),
            author_email: String::new(),
            committer: String::new(),
            committer_email: String::new(),
            date: String::new(),
            message: "Uncommitted changes".to_owned(),
        },
        files: files.iter().map(|f| f.change.clone()).collect(),
        additions: files.iter().filter_map(|f| f.change.additions).sum(),
        deletions: files.iter().filter_map(|f| f.change.deletions).sum(),
    }
}

impl Workspace {
    /// The working tree is what the file pane shows.
    pub fn work_open(&self) -> bool {
        self.repo.as_ref().and_then(|repo| repo.commit.as_ref()).is_some_and(|commit| commit.id == WORKTREE)
    }

    /// Shows the working tree in the file pane.
    pub fn open_work(&mut self, cx: &mut Context<Self>) {
        self.cancel_file_load();
        self.cancel_commit_load();
        let Some(repo) = self.repo.as_mut() else { return };
        repo.commit = Some(CommitState { id: WORKTREE.to_owned(), phase: Phase::Ready(work_view(&repo.work)) });
        repo.file = None;
        // The "Uncommitted Changes" row stands for it in the graph.
        repo.selected = match &repo.phase {
            Phase::Ready(view) => view.entries.first().filter(|e| e.commit.is_none()).map(|_| 0),
            _ => None,
        };
        repo.refresh_file_rows();
        cx.notify();
    }

    /// Opens the file before (`-1`) or after (`1`) the open one: in the sidebar's order for the
    /// working tree, in the file list's order for a commit. Nothing happens at either end, or with no
    /// file open (except in the working tree, where Down opens the first).
    pub fn step_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref() else { return };
        // A file read from a list of usages is not one of the changed files: the arrows leave it alone.
        if repo.file.as_ref().is_some_and(|file| file.viewing.is_some()) {
            return;
        }
        let work = self.work_open();
        let order: Vec<usize> = if work {
            (0..repo.work.len()).collect()
        } else {
            repo.file_rows
                .iter()
                .filter_map(|row| match row {
                    gitgui_core::TreeRow::File { index, .. } => Some(*index),
                    gitgui_core::TreeRow::Dir { .. } => None,
                })
                .collect()
        };
        let next = match repo.file.as_ref().and_then(|file| order.iter().position(|&i| i == file.index)) {
            Some(at) => at.checked_add_signed(delta).filter(|&n| n < order.len()),
            None if delta > 0 && repo.commit.is_some() => Some(0).filter(|_| !order.is_empty()),
            None => None,
        };
        if let Some(next) = next {
            self.open_file(order[next], cx);
        }
    }

    /// Opens one changed file's diff.
    pub fn open_work_file(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.work_open() {
            self.open_work(cx);
        }
        self.open_file(index, cx);
    }

    /// Reads what has changed again, keeping the open file open (its diff read again).
    pub fn refresh_work(&mut self, cx: &mut Context<Self>) {
        self.reload_work(AfterWork::Reopen, cx);
    }

    /// Reads what has changed again, then opens what `after` says.
    pub(crate) fn reload_work(&mut self, after: AfterWork, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref() else { return };
        let (path, generation) = (repo.project.path.clone(), repo.generation);
        self.spawn_load(
            cx,
            move || GitCli::new(&path).work_status(),
            move |this, result, cx| {
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.generation == generation) else { return };
                let files = match result {
                    Ok(files) => files,
                    Err(err) => return this.say(Notice::warn(explain(&err)), cx),
                };
                // The file open now, by path and side, so it stays open where it has moved to.
                let open = repo
                    .file
                    .as_ref()
                    .and_then(|file| repo.work.get(file.index))
                    .map(|f| (f.change.path.clone(), f.staged));
                repo.work = files;
                let changed = {
                    let mut paths: Vec<&str> = repo.work.iter().map(|f| f.change.path.as_str()).collect();
                    paths.sort_unstable();
                    paths.dedup();
                    paths.len()
                };
                if let Phase::Ready(view) = &mut repo.phase
                    && view.changed != changed
                {
                    view.changed = changed;
                    let settings = this.settings.clone();
                    if let Some(repo) = this.repo.as_mut() {
                        repo.rebuild(&settings);
                    }
                }
                if !this.work_open() {
                    return cx.notify();
                }
                let Some(repo) = this.repo.as_mut() else { return };
                repo.commit = Some(CommitState { id: WORKTREE.to_owned(), phase: Phase::Ready(work_view(&repo.work)) });
                repo.refresh_file_rows();
                let reopen = match after {
                    AfterWork::Reopen => open.and_then(|(path, staged)| {
                        // Staged or not, the same file is still the one to show.
                        let same = |f: &WorkFile| f.change.path == path;
                        repo.work.iter().position(|f| same(f) && f.staged == staged).or_else(|| repo.work.iter().position(same))
                    }),
                    // The next file with conflicts in the list's order, going round; none when all are resolved.
                    AfterWork::NextConflict(done) => {
                        let conflicted = |f: &&WorkFile| f.conflicted;
                        let done = done.to_lowercase();
                        let after = repo.work.iter().position(|f| f.conflicted && f.change.path.to_lowercase() > done);
                        after.or_else(|| repo.work.iter().position(|f| conflicted(&f)))
                    }
                };
                match reopen {
                    Some(index) => this.open_file(index, cx),
                    None => {
                        repo.file = None;
                        cx.notify();
                    }
                }
            },
        );
    }

    /// Stages (or, with `stage` false, unstages) `paths`, then reads the working tree again.
    pub fn stage_paths(&mut self, paths: Vec<String>, stage: bool, cx: &mut Context<Self>) {
        let Some(path) = self.repo.as_ref().map(|repo| repo.project.path.clone()) else { return };
        if paths.is_empty() {
            return;
        }
        self.spawn_load(
            cx,
            move || {
                let git = GitCli::new(&path);
                if stage { git.stage(&paths) } else { git.unstage(&paths) }
            },
            move |this, result, cx| match result {
                Ok(_) => this.refresh_work(cx),
                Err(err) => this.say(Notice::warn(explain(&err)), cx),
            },
        );
    }

    /// Commits what is staged with the message in the box; with nothing staged, everything.
    pub fn commit_work(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_ref() else { return };
        let message = self.commit_input.read(cx).text().trim().to_owned();
        if message.is_empty() {
            return self.say(Notice::warn("Write a commit message first."), cx);
        }
        if repo.work.is_empty() {
            return self.say(Notice::warn("There is nothing to commit."), cx);
        }
        // Committing a file with conflict markers in it would mark it resolved as it is.
        if repo.work.iter().any(|f| f.conflicted) {
            return self.say(Notice::warn("Resolve the files with conflicts first: they would be committed with their conflict markers."), cx);
        }
        if self.busy.is_some() {
            return self.say(Notice::warn("Another operation is still running."), cx);
        }
        let all: Vec<String> = repo.work.iter().map(|f| f.change.path.clone()).collect();
        let stage_all = !repo.work.iter().any(|f| f.staged);
        let path = repo.project.path.clone();
        let summary = message.lines().next().unwrap_or_default().to_owned();
        self.busy = Some("Committing…".into());
        cx.notify();
        self.spawn_load(
            cx,
            move || {
                let git = GitCli::new(&path);
                if stage_all {
                    git.stage(&all)?;
                }
                git.commit_staged(&message)
            },
            move |this, result, cx| {
                this.busy = None;
                match result {
                    Ok(_) => {
                        this.commit_input.update(cx, |input, cx| input.clear(cx));
                        this.notice = Some(Notice::info(format!("Committed “{summary}”.")));
                        this.refresh_keeping_notice(cx);
                    }
                    Err(err) => this.say(Notice::warn(explain(&err)), cx),
                }
            },
        );
    }

    /// The changed files of the open project, staged then not, under its name in the sidebar.
    pub fn render_changes(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let repo = self.repo.as_ref()?;
        if !matches!(repo.phase, Phase::Ready(_)) {
            return None;
        }
        // A clean tree says so on the project's own row, not with a row of its own.
        if repo.work.is_empty() {
            return None;
        }
        let open = self.work_open().then(|| repo.file.as_ref().map(|f| f.index)).flatten();
        let conflicted: Vec<usize> = (0..repo.work.len()).filter(|&i| repo.work[i].conflicted).collect();
        let staged: Vec<usize> = (0..repo.work.len()).filter(|&i| repo.work[i].staged).collect();
        let unstaged: Vec<usize> = (0..repo.work.len()).filter(|&i| !repo.work[i].staged && !repo.work[i].conflicted).collect();

        let section = |title: &'static str, rows: &[usize], stage: Option<bool>, cx: &mut Context<Self>| -> Option<AnyElement> {
            if rows.is_empty() {
                return None;
            }
            let paths: Vec<String> = rows.iter().map(|&i| repo.work[i].change.path.clone()).collect();
            let header = div()
                .id(SharedString::from(format!("section-{title}")))
                .group("section")
                .h(px(ROW_H))
                .pl(px(18.))
                .pr_2()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(if stage.is_none() { t().warning } else { t().muted }))
                .child(div().flex_1().child(format!("{title}  {}", rows.len())))
                // A file with conflicts is staged by resolving it, never as it is.
                .children(stage.map(|stage| {
                    div()
                        .id(SharedString::from(format!("all-{title}")))
                        .px_1()
                        .rounded_sm()
                        .opacity(0.)
                        .group_hover("section", |style| style.opacity(1.))
                        .hover(|style| style.bg(rgb(t().hover)).text_color(rgb(t().text_strong)))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.stage_paths(paths.clone(), stage, cx)))
                        .child(if stage { "+ all" } else { "− all" })
                }));
            let files = rows.iter().map(|&i| self.render_change_row(i, &repo.work[i], open == Some(i), cx));
            Some(div().flex().flex_col().child(header).children(files).into_any_element())
        };

        Some(
            div()
                .flex()
                .flex_col()
                .pb_1()
                .children(section("CONFLICTS", &conflicted, None, cx))
                .children(section("STAGED", &staged, Some(false), cx))
                .children(section("CHANGES", &unstaged, Some(true), cx))
                .into_any_element(),
        )
    }

    fn render_change_row(&self, index: usize, file: &WorkFile, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let path = file.change.path.clone();
        let (dir, name) = path.rsplit_once('/').map_or(("", path.as_str()), |(d, n)| (d, n));
        let (mark, mark_color) = letter(file);
        let deleted = file.change.status == FileStatus::Deleted;
        let staged = file.staged;
        let conflicted = file.conflicted;
        let toggle_path = path.clone();
        let group = SharedString::from(format!("change-{index}"));
        div()
            .id(("change", index))
            .debug_selector(move || format!("change-{index}"))
            .group(group.clone())
            .h(px(ROW_H))
            .pl(px(26.))
            .pr_2()
            .flex()
            .items_center()
            .gap_1p5()
            .text_xs()
            .cursor_pointer()
            .when(open, |row| row.bg(rgb(t().selected)))
            .hover(|style| style.bg(rgb(if open { t().selected } else { t().hover })))
            .on_click(cx.listener(move |this, _, _, cx| this.open_work_file(index, cx)))
            .child(ui::file_icon(icons::file(name)))
            .child(
                div()
                    .flex_none()
                    .max_w(px(150.))
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .text_color(rgb(t().text))
                    .when(deleted, |name| name.line_through().text_color(rgb(t().muted)))
                    .child(SharedString::from(name.to_owned())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .text_color(rgb(t().muted))
                    .child(SharedString::from(dir.to_owned())),
            )
            .child(
                div()
                    .id(("stage", index))
                    .flex_none()
                    .w(px(16.))
                    .flex()
                    .justify_center()
                    .rounded_sm()
                    .text_color(rgb(t().muted))
                    .opacity(0.)
                    // A file with conflicts is staged by resolving it, never as it is.
                    .when(!conflicted, |toggle| {
                        toggle
                            .group_hover(group, |style| style.opacity(1.))
                            .hover(|style| style.bg(rgb(t().element_hover)).text_color(rgb(t().text_strong)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.stage_paths(vec![toggle_path.clone()], !staged, cx);
                            }))
                    })
                    .child(if staged { "−" } else { "+" }),
            )
            .child(div().flex_none().w(px(10.)).font_weight(FontWeight::BOLD).text_color(rgb(mark_color)).child(mark))
            .into_any_element()
    }

    /// The message box and the button that commits, at the foot of the sidebar.
    pub fn render_commit_box(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let repo = self.repo.as_ref()?;
        if !matches!(repo.phase, Phase::Ready(_)) || repo.work.is_empty() {
            return None;
        }
        let staged = repo.work.iter().filter(|f| f.staged).count();
        let label = if staged > 0 { format!("Commit {staged} staged") } else { "Commit all".to_owned() };
        let length = self.commit_input.read(cx).text().lines().next().map_or(0, |l| l.chars().count());
        Some(
            div()
                .flex_none()
                .p_2()
                .flex()
                .flex_col()
                .gap_1p5()
                .border_t_1()
                .border_color(rgb(t().border))
                .child(self.commit_input.clone())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(rgb(if length > 72 { t().warning } else { t().muted }))
                                .child(if length > 72 { format!("{length}/72 · keep the summary short") } else { format!("{length}/72") }),
                        )
                        .child(
                            button("commit", label)
                                .bg(rgb(t().accent))
                                .text_color(rgb(t().on_accent))
                                .font_weight(FontWeight::SEMIBOLD)
                                .on_click(cx.listener(|this, _, _, cx| this.commit_work(cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}
