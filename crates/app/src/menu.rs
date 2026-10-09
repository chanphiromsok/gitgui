//! Right-click menus on branches and commits, the confirmations behind them, the settings panel, and
//! running what was chosen.
//!
//! Anything that changes history or touches a remote asks first, in words that say what will happen.
//! The work runs off the UI thread; when it ends the repository is read again and the result is shown
//! in the banner at the bottom: what was done or why git refused. A merge, rebase or cherry-pick that hits
//! conflicts opens its first file in the resolver, and the bar over the main area finishes or aborts it.
//!
//! The question before a merge, rebase, pull or cherry-pick also says what a test merge found (`git merge-tree`,
//! which touches nothing): whether it would conflict, and in which files. It is read in the background, and the
//! question works the same without it.

use gitgui_core::{
    CheckoutTarget, Error, Evidence, GitCli, Label, LabelKind, Operation, Outcome, Preflight, Upstream, default_remote, nothing_to_pull,
};
use gpui::{
    AnyElement, ClipboardItem, Context, FontWeight, MouseButton, Pixels, Point, SharedString, Window, div, prelude::*, px,
    rgb, rgba,
};

use crate::ui::button;
use crate::workspace::{Phase, Workspace};
use crate::theme::t;

const MENU_WIDTH: f32 = 300.;
const ITEM_HEIGHT: f32 = 26.;
const SEPARATOR_HEIGHT: f32 = 9.;

/// What was right-clicked.
#[derive(Clone, Debug)]
pub enum MenuTarget {
    /// A branch, remote branch or tag badge.
    Label(Label),
    /// A commit, by its full id.
    Commit(String),
    /// The authors to limit the graph to.
    Authors,
    /// The days to limit the graph to.
    Dates,
}

/// Something a menu item does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Checkout(CheckoutTarget),
    RenameBranch(String),
    DeleteBranch(String),
    /// Merge this branch into the current one.
    Merge(String),
    /// Rebase the current branch onto this one.
    Rebase(String),
    Push(String),
    /// `git pull --rebase` on the current branch.
    PullRebase,
    /// Make a branch at this commit.
    CreateBranch(String),
    /// Make a tag at this commit.
    CreateTag(String),
    CherryPick(String),
    /// Limit the graph to this author (an email), or to anyone.
    FilterAuthor(Option<String>),
    /// Limit the graph to these days (a `date:` value like `today` or `7d`), or to any time.
    FilterDate(Option<String>),
    /// Clone a repository by address (typed in the dialog) into the dialog's folder.
    Clone,
    Copy { text: String, what: &'static str },
    /// Open a page (a pull request, a commit) in the browser.
    OpenUrl(String),
    /// Give up the merge, rebase or cherry-pick in progress (the bar's Abort).
    AbortOperation(Operation),
    /// Leave out the commit a rebase or cherry-pick stopped on.
    SkipCommit(Operation),
    /// Put a conflicted file back as git made the conflict, over what is in it now.
    StartOver(String),
    /// Resolve a conflicted file by deleting it.
    DeleteConflicted(String),
    /// Move the branch back to where it was before its last rebase.
    UndoRebase(gitgui_core::Rebased),
}

/// What the test merge behind a question has found so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    Running,
    Found(Preflight),
}

pub struct MenuState {
    pub position: Point<Pixels>,
    pub target: MenuTarget,
}

#[derive(Clone)]
pub struct MenuItem {
    pub label: SharedString,
    /// `None` makes this a separator.
    pub action: Option<Action>,
    pub enabled: bool,
}

/// A question the user answers before something that cannot be undone with one click.
pub struct Dialog {
    pub title: SharedString,
    pub body: SharedString,
    pub confirm: SharedString,
    /// The confirm button is red.
    pub danger: bool,
    /// A name to type, and what it starts as.
    pub prompt: Option<String>,
    /// A folder shown under the text field, with a button to change it (where a clone goes).
    pub folder: Option<std::path::PathBuf>,
    /// What a test merge says about it, for a merge, rebase, pull or cherry-pick; none when git cannot say.
    pub check: Option<Check>,
    pub action: Action,
}

/// What the banner's button does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeAction {
    /// Delete a branch git says is not merged, now that the user has seen that.
    ForceDelete(String),
}

/// The banner at the bottom.
#[derive(Clone)]
pub struct Notice {
    pub text: SharedString,
    /// Something went wrong or needs attention, as opposed to news.
    pub warn: bool,
    pub action: Option<(SharedString, NoticeAction)>,
}

impl Notice {
    pub fn warn(text: impl Into<SharedString>) -> Self {
        Self { text: text.into(), warn: true, action: None }
    }

    pub fn info(text: impl Into<SharedString>) -> Self {
        Self { text: text.into(), warn: false, action: None }
    }
}

/// Git's own words, trimmed, for the banner.
pub(crate) fn explain(error: &Error) -> String {
    let text = match error {
        Error::Git { stderr, .. } => stderr.trim().to_owned(),
        // A message already written for the person to read.
        Error::Parse(message) => message.clone(),
        other => other.to_string(),
    };
    let text: String = text.lines().filter(|l| !l.trim().is_empty()).take(6).collect::<Vec<_>>().join(" ");
    if text.chars().count() > 600 { text.chars().take(600).collect::<String>() + "…" } else { text }
}

impl Workspace {
    // ---- the menu ---------------------------------------------------------------------------

    pub fn open_menu(&mut self, position: Point<Pixels>, target: MenuTarget, cx: &mut Context<Self>) {
        if matches!(&target, MenuTarget::Label(label) if label.kind == LabelKind::Stash) {
            return;
        }
        self.menu = Some(MenuState { position, target });
        cx.notify();
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn current_branch_name(&self) -> Option<String> {
        match &self.repo.as_ref()?.phase {
            Phase::Ready(view) => view.current_branch.as_ref().map(ToString::to_string),
            _ => None,
        }
    }

    fn summary_of(&self, commit: &str) -> Option<String> {
        match &self.repo.as_ref()?.phase {
            Phase::Ready(view) => view
                .entries
                .iter()
                .find(|entry| entry.commit.as_deref() == Some(commit))
                .map(|entry| entry.summary.to_string()),
            _ => None,
        }
    }

    /// Where `branch` stands against its upstream, and the repository's remotes, as last read.
    pub(crate) fn sync_of(&self, branch: &str) -> (Upstream, Vec<String>) {
        match self.repo.as_ref().map(|repo| &repo.phase) {
            Some(Phase::Ready(view)) => (view.upstreams.get(branch).cloned().unwrap_or(Upstream::None), view.remotes.clone()),
            _ => (Upstream::None, Vec::new()),
        }
    }

    /// What the scan found about this branch, if it found anything.
    fn clue_for(&self, branch: &str) -> Option<gitgui_core::MergeClue> {
        match &self.repo.as_ref()?.phase {
            Phase::Ready(view) => view.clues.iter().find(|clue| clue.branch == branch).cloned(),
            _ => None,
        }
    }

    pub fn menu_items(&self, target: &MenuTarget) -> Vec<MenuItem> {
        let current = self.current_branch_name();
        let item = |label: &str, action: Action, enabled: bool| MenuItem { label: label.to_owned().into(), action: Some(action), enabled };
        let separator = || MenuItem { label: SharedString::default(), action: None, enabled: false };
        let copy = |text: &str, what: &'static str| Action::Copy { text: text.to_owned(), what };

        match target {
            MenuTarget::Authors => {
                let (search, people, commits) = match self.repo.as_ref().map(|repo| (&repo.graph_filter.search, &repo.phase)) {
                    Some((search, Phase::Ready(view))) => (search.clone(), Some(view.people.clone()), Some(view.commits.clone())),
                    Some((search, _)) => (search.clone(), None, None),
                    None => (String::new(), None, None),
                };
                let asked = gitgui_core::term(&search, "author").map(|a| a.to_lowercase());
                let tick = |on: bool, label: &str| if on { format!("✓  {label}") } else { format!("    {label}") };
                let mut out = vec![item(&tick(asked.is_none(), "Anyone"), Action::FilterAuthor(None), true), separator()];
                if let (Some(people), Some(commits)) = (people, commits) {
                    // The people who made the most commits first.
                    let mut counts: Vec<(String, String, usize)> = Vec::new();
                    for commit in commits.iter() {
                        if let Some(person) = people.of(&commit.email) {
                            match counts.iter_mut().find(|(_, email, _)| *email == person.email) {
                                Some(entry) => entry.2 += 1,
                                None => counts.push((person.name.clone(), person.email.clone(), 1)),
                            }
                        }
                    }
                    counts.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
                    for (name, email, _) in counts.into_iter().take(14) {
                        let on = asked.as_deref().is_some_and(|a| a == email.to_lowercase() || name.to_lowercase() == a);
                        out.push(item(&tick(on, &name), Action::FilterAuthor(Some(email)), true));
                    }
                }
                out
            }
            MenuTarget::Dates => {
                let search = self.repo.as_ref().map(|repo| repo.graph_filter.search.clone()).unwrap_or_default();
                let asked = gitgui_core::term(&search, "date");
                let tick = |on: bool, label: &str| if on { format!("✓  {label}") } else { format!("    {label}") };
                let mut out = vec![item(&tick(asked.is_none(), "Any time"), Action::FilterDate(None), true), separator()];
                for (label, spec) in [("Today", "today"), ("Yesterday", "yesterday"), ("Last 7 days", "7d"), ("Last 30 days", "30d"), ("This month", "month")] {
                    out.push(item(&tick(asked.as_deref() == Some(spec), label), Action::FilterDate(Some(spec.to_owned())), true));
                }
                out.push(separator());
                out.push(item("    Or type date:2026-10-01..2026-10-07", Action::FilterDate(asked.clone()), false));
                out
            }
            MenuTarget::Label(label) => {
                let name = label.name.clone();
                let on_it = label.head || current.as_deref() == Some(name.as_str());
                let can_combine = !on_it && current.is_some();
                match label.kind {
                    LabelKind::Branch => vec![
                        item("Checkout Branch", Action::Checkout(CheckoutTarget::Branch(name.clone())), !on_it),
                        item("Rename Branch…", Action::RenameBranch(name.clone()), true),
                        item("Delete Branch…", Action::DeleteBranch(name.clone()), !on_it),
                        item("Merge into Current Branch…", Action::Merge(name.clone()), can_combine),
                        item("Rebase Current Branch onto Branch…", Action::Rebase(name.clone()), can_combine),
                        item("Push Branch…", Action::Push(name.clone()), true),
                        separator(),
                        item("Copy Branch Name", copy(&name, "branch name"), true),
                    ],
                    LabelKind::RemoteBranch => vec![
                        item("Checkout Branch", Action::Checkout(CheckoutTarget::RemoteBranch(name.clone())), true),
                        item("Merge into Current Branch…", Action::Merge(name.clone()), current.is_some()),
                        item("Rebase Current Branch onto Branch…", Action::Rebase(name.clone()), current.is_some()),
                        separator(),
                        item("Copy Branch Name", copy(&name, "branch name"), true),
                    ],
                    LabelKind::Tag => vec![
                        item("Checkout Tag (detached HEAD)", Action::Checkout(CheckoutTarget::Detached(name.clone())), true),
                        separator(),
                        item("Copy Tag Name", copy(&name, "tag name"), true),
                    ],
                    LabelKind::Stash => Vec::new(),
                }
            }
            MenuTarget::Commit(id) => {
                let summary = self.summary_of(id).unwrap_or_default();
                let web = match self.repo.as_ref().map(|repo| &repo.phase) {
                    Some(Phase::Ready(view)) => view.web.clone(),
                    _ => None,
                };
                let mut links = Vec::new();
                if let Some(web) = &web {
                    if let Some(number) = gitgui_core::subject_pr(&summary) {
                        let label = format!("Open {} #{number} in Browser", web.pull_request_name());
                        links.push(item(&label, Action::OpenUrl(web.pull_request(number)), true));
                    }
                    links.push(item("Open Commit in Browser", Action::OpenUrl(web.commit(id)), true));
                    links.push(separator());
                }
                links.extend(vec![
                    item("Checkout Commit (detached HEAD)", Action::Checkout(CheckoutTarget::Detached(id.clone())), true),
                    item("Create Branch Here…", Action::CreateBranch(id.clone()), true),
                    item("Create Tag Here…", Action::CreateTag(id.clone()), true),
                    item("Cherry-pick onto Current Branch…", Action::CherryPick(id.clone()), true),
                    separator(),
                    item("Copy Commit SHA", copy(id, "commit id"), true),
                    item("Copy Commit Message", copy(&summary, "commit message"), !summary.is_empty()),
                ]);
                links
            }
        }
    }

    /// What clicking a menu item does: copy and checkout happen at once; the rest ask first.
    pub fn choose(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if let Action::Copy { text, what } = action {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            return self.say(Notice::info(format!("Copied the {what}.")), cx);
        }
        if let Action::OpenUrl(url) = action {
            return cx.open_url(&url);
        }
        // Limiting the graph is not an operation on the repository: it rewrites the search box.
        match action {
            Action::FilterAuthor(value) => return self.set_search_term("author", value, cx),
            Action::FilterDate(value) => return self.set_search_term("date", value, cx),
            _ => {}
        }
        if self.busy.is_some() {
            return self.say(Notice::warn("Another operation is still running."), cx);
        }
        // A pull that has nothing to pull from is not worth a question.
        if action == Action::PullRebase
            && let Some(branch) = self.current_branch_name()
            && let Some(why) = nothing_to_pull(&branch, &self.sync_of(&branch).0)
        {
            return self.say(Notice::warn(why), cx);
        }
        match action {
            Action::Checkout(target) => {
                let name = match &target {
                    CheckoutTarget::Branch(n) | CheckoutTarget::RemoteBranch(n) | CheckoutTarget::Detached(n) => n.clone(),
                };
                let shown = name.chars().take(40).collect::<String>();
                self.run(
                    format!("Checking out {shown}…"),
                    format!("Checked out {shown}."),
                    None,
                    move |git| git.checkout(&target).map(Outcome::Done),
                    cx,
                );
            }
            other => self.open_dialog(other, window, cx),
        }
    }

    // ---- the questions ------------------------------------------------------------------------

    pub(crate) fn open_dialog(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_branch_name().unwrap_or_else(|| "the current branch".to_owned());
        let (title, body, confirm, danger, prompt): (String, String, &str, bool, Option<String>) = match &action {
            Action::DeleteBranch(name) => {
                let body = match self.clue_for(name) {
                    Some(clue) => {
                        let how = match clue.evidence {
                            Evidence::Contained => "merged into",
                            Evidence::SamePatch => "squash-merged into",
                            _ => "probably squash-merged into",
                        };
                        format!(
                            "`{name}` was already {how} `{}`, so its work is kept there. Deleting the branch removes its name only. \
                             Git cannot see a squash merge, so this will be a forced delete.",
                            clue.into
                        )
                    }
                    None => format!(
                        "Delete the branch `{name}`? Git refuses if it holds commits that are not merged anywhere; \
                         you will be asked again before any such work is thrown away."
                    ),
                };
                (format!("Delete {name}?"), body, "Delete Branch", true, None)
            }
            Action::Merge(branch) => (
                format!("Merge {branch} into {current}?"),
                format!(
                    "Brings the commits of `{branch}` into `{current}`: a merge commit, or a fast-forward when possible. \
                     If there are conflicts the merge stops: resolve them here, or abort to put everything back."
                ),
                "Merge",
                false,
                None,
            ),
            Action::Rebase(onto) => (
                format!("Rebase {current} onto {onto}?"),
                format!(
                    "Replays the commits of `{current}` on top of `{onto}`, which rewrites them. If `{current}` is already pushed, \
                     pushing it again would need a force-push, and this app never force-pushes. \
                     If there are conflicts it stops at each commit that has them: resolve them here, or abort and nothing changes."
                ),
                "Rebase",
                true,
                None,
            ),
            Action::Push(branch) => {
                let (upstream, remotes) = self.sync_of(branch);
                let (title, body, confirm) = push_question(branch, &upstream, &remotes);
                (title, body, confirm, false, None)
            }
            Action::PullRebase => {
                let (upstream, _) = self.sync_of(&current);
                (
                    format!("Pull {current} with rebase?"),
                    format!(
                        "{}Fetches the upstream of `{current}` and replays the commits that are only on your side on top of it \
                         (`git pull --rebase`), so history stays a straight line with no merge commit. Your unpushed commits are \
                         rewritten, which is safe because they are not shared yet. Nothing is forced. If there are conflicts it \
                         stops: resolve them here, or abort to put everything back. Git refuses if you have uncommitted changes.",
                        pull_lead(&current, &upstream)
                    ),
                    "Pull",
                    false,
                    None,
                )
            }
            Action::CherryPick(id) => {
                let summary = self.summary_of(id).unwrap_or_default();
                (
                    format!("Cherry-pick onto {current}?"),
                    format!(
                        "Applies the changes of {} `{summary}` as a new commit on `{current}`. \
                         If there are conflicts it stops: resolve them here, or abort.",
                        id.chars().take(7).collect::<String>()
                    ),
                    "Cherry-pick",
                    false,
                    None,
                )
            }
            Action::RenameBranch(old) => (
                format!("Rename {old}"),
                "Type the new name. Remote branches are not renamed.".to_owned(),
                "Rename",
                false,
                Some(old.clone()),
            ),
            Action::CreateBranch(at) => (
                "New branch here".to_owned(),
                format!("Makes a branch at {}. It does not switch to it.", at.chars().take(7).collect::<String>()),
                "Create Branch",
                false,
                Some(String::new()),
            ),
            Action::CreateTag(at) => (
                "New tag here".to_owned(),
                format!("Makes a tag at {}.", at.chars().take(7).collect::<String>()),
                "Create Tag",
                false,
                Some(String::new()),
            ),
            Action::AbortOperation(operation) => {
                let word = operation.name();
                (
                    format!("Abort the {word}?"),
                    format!(
                        "Puts the repository back exactly as it was before the {word} started. Whatever you resolved so far is thrown \
                         away; the {word} can be started again."
                    ),
                    match operation {
                        Operation::Merge => "Abort Merge",
                        Operation::Rebase => "Abort Rebase",
                        Operation::CherryPick => "Abort Cherry-pick",
                    },
                    true,
                    None,
                )
            }
            Action::UndoRebase(rebased) => {
                let old = rebased.old_tip.chars().take(7).collect::<String>();
                (
                    format!("Undo the rebase of {}?", rebased.branch),
                    format!(
                        "Moves {} back to {old}, where it was before the rebase. Nothing is deleted: the rebased commits stay in \
                         the reflog for a while, so the rebase can be done again. It stops, and changes nothing, if you have \
                         changes that are not committed.",
                        rebased.branch
                    ),
                    "Undo Rebase",
                    false,
                    None,
                )
            }
            Action::SkipCommit(operation) => {
                let commit = self.side_names().1.title;
                (
                    "Skip this commit?".to_owned(),
                    format!(
                        "Leaves {commit} out of the {}, with anything you resolved in it, and goes on. Its changes stay in its \
                         original commit; nothing else is lost.",
                        operation.name()
                    ),
                    "Skip Commit",
                    true,
                    None,
                )
            }
            Action::StartOver(path) => (
                format!("Start over with {}?", path.rsplit('/').next().unwrap_or(path)),
                "Puts back the conflict as git made it, with what both sides started from in each block, over what is in the file \
                 now. Edits made to it since are replaced."
                    .to_owned(),
                "Start Over",
                true,
                None,
            ),
            Action::DeleteConflicted(path) => (
                format!("Delete {}?", path.rsplit('/').next().unwrap_or(path)),
                "Resolves the conflict by deleting the file, here and in the next commit. Its versions stay in the history of each \
                 side."
                    .to_owned(),
                "Delete File",
                true,
                None,
            ),
            Action::Checkout(_) | Action::Clone | Action::Copy { .. } | Action::OpenUrl(_) | Action::FilterAuthor(_) | Action::FilterDate(_) => return,
        };
        if let Some(initial) = &prompt {
            let initial = initial.clone();
            self.dialog_input.update(cx, |input, cx| {
                input.set_text(&initial, cx);
                input.focus(window);
            });
        }
        let check = matches!(action, Action::Merge(_) | Action::Rebase(_) | Action::PullRebase | Action::CherryPick(_)).then_some(Check::Running);
        if check.is_some() {
            self.start_check(action.clone(), cx);
        }
        self.dialog = Some(Dialog { title: title.into(), body: body.into(), confirm: confirm.into(), danger, prompt, folder: None, check, action });
        cx.notify();
    }

    /// Runs the test merge for the question about `action` in the background; its answer is shown if the question is
    /// still open, and the line goes away when git cannot say.
    fn start_check(&mut self, action: Action, cx: &mut Context<Self>) {
        let Some(path) = self.repo.as_ref().map(|repo| repo.project.path.clone()) else { return };
        let asked = action.clone();
        self.spawn_load(
            cx,
            move || {
                let git = GitCli::new(path);
                match &asked {
                    Action::Merge(branch) => git.preflight_merge(branch),
                    Action::Rebase(onto) => git.preflight_rebase(onto),
                    Action::PullRebase => git.preflight_pull(),
                    Action::CherryPick(id) => git.preflight_cherry_pick(id),
                    _ => None,
                }
            },
            move |this, found, cx| {
                if let Some(dialog) = this.dialog.as_mut().filter(|dialog| dialog.action == action) {
                    dialog.check = found.map(Check::Found);
                    cx.notify();
                }
            },
        );
    }

    pub fn cancel_dialog(&mut self, cx: &mut Context<Self>) {
        if self.dialog.take().is_some() {
            cx.notify();
        }
    }

    pub fn confirm_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.take() else { return };
        let typed = self.dialog_input.read(cx).text().trim().to_owned();
        if dialog.prompt.is_some() && typed.is_empty() {
            self.dialog = Some(dialog);
            return;
        }
        cx.notify();

        let short = |id: &str| id.chars().take(7).collect::<String>();
        match dialog.action {
            Action::DeleteBranch(name) => {
                // A branch the scan saw merged is safe to remove; git only needs to be told so.
                let force = self.clue_for(&name).is_some();
                let offer = (!force).then(|| name.clone());
                self.run(format!("Deleting {name}…"), format!("Deleted {name}."), offer, move |git| git.delete_branch(&name, force).map(Outcome::Done), cx);
            }
            Action::Merge(branch) => {
                let into = self.current_branch_name().unwrap_or_default();
                self.run(format!("Merging {branch}…"), format!("Merged {branch} into {into}."), None, move |git| git.merge(&branch), cx);
            }
            Action::Rebase(onto) => {
                let on = self.current_branch_name().unwrap_or_default();
                self.run(format!("Rebasing onto {onto}…"), format!("Rebased {on} onto {onto}."), None, move |git| git.rebase(&onto), cx);
            }
            Action::Push(branch) => {
                let (upstream, remotes) = self.sync_of(&branch);
                let done = match (&upstream, default_remote(&remotes)) {
                    (Upstream::None, Some(remote)) => format!("Published {branch} to {remote}."),
                    _ => format!("Pushed {branch}."),
                };
                self.run(format!("Pushing {branch}…"), done, None, move |git| git.push_branch(&branch).map(Outcome::Done), cx);
            }
            Action::PullRebase => {
                let on = self.current_branch_name().unwrap_or_default();
                self.run("Pulling…".to_owned(), format!("Pulled {on} (rebase)."), None, |git| git.pull_rebase(), cx);
            }
            Action::CherryPick(id) => {
                let shown = short(&id);
                self.run(format!("Cherry-picking {shown}…"), format!("Cherry-picked {shown}."), None, move |git| git.cherry_pick(&id), cx);
            }
            Action::RenameBranch(old) => {
                self.run(format!("Renaming {old}…"), format!("Renamed {old} to {typed}."), None, move |git| git.rename_branch(&old, &typed).map(Outcome::Done), cx);
            }
            Action::CreateBranch(at) => {
                let shown = short(&at);
                self.run(format!("Creating {typed}…"), format!("Created branch {typed} at {shown}."), None, move |git| git.create_branch(&typed, Some(&at), false).map(Outcome::Done), cx);
            }
            Action::CreateTag(at) => {
                let shown = short(&at);
                self.run(format!("Tagging {shown}…"), format!("Tagged {shown} as {typed}."), None, move |git| git.create_tag(&typed, &at).map(Outcome::Done), cx);
            }
            Action::AbortOperation(operation) => self.run(
                format!("Aborting the {}…", operation.name()),
                format!("Aborted the {}. Everything is as it was.", operation.name()),
                None,
                move |git| git.abort(operation).map(Outcome::Done),
                cx,
            ),
            Action::UndoRebase(rebased) => {
                let old = rebased.old_tip.chars().take(7).collect::<String>();
                let branch = rebased.branch.clone();
                self.run(
                    format!("Moving {branch} back…"),
                    format!("Moved {branch} back to {old}, where it was before the rebase."),
                    None,
                    move |git| git.undo_rebase(&rebased).map(Outcome::Done),
                    cx,
                )
            }
            Action::SkipCommit(operation) => self.run(
                "Skipping the commit…".to_owned(),
                format!("Skipped the commit and finished the {}.", operation.name()),
                None,
                move |git| git.skip_commit(operation),
                cx,
            ),
            Action::StartOver(path) => self.restart_conflict(path, cx),
            Action::DeleteConflicted(path) => self.delete_conflicted(path, cx),
            Action::Checkout(_) | Action::Copy { .. } | Action::OpenUrl(_) | Action::FilterAuthor(_) | Action::FilterDate(_) => {}
            Action::Clone => {
                let folder = dialog.folder.unwrap_or_else(|| self.clone_folder());
                self.clone_repo(typed, folder, cx);
            }
        }
    }

    // ---- limiting the graph ---------------------------------------------------------------------

    /// Puts `key:value` in the search box (taking out any other `key:` word), or only takes it out. The box is
    /// the one place the author and date live, so what the menus choose can be read, edited and typed over.
    pub(crate) fn set_search_term(&mut self, key: &str, value: Option<String>, cx: &mut Context<Self>) {
        let current = self.search_input.read(cx).text().to_owned();
        let next = gitgui_core::with_term(&current, key, value.as_deref());
        self.search_input.update(cx, |input, cx| input.replace_text(&next, cx));
    }

    // ---- cloning ------------------------------------------------------------------------------

    /// Where a clone goes: the folder the last one went into, else the home folder.
    pub(crate) fn clone_folder(&self) -> std::path::PathBuf {
        self.settings
            .clone_dir
            .clone()
            .filter(|dir| dir.is_dir())
            .or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from))
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    }

    /// Asks for the address of a repository to clone (the clipboard's, when it holds one) and the folder it
    /// goes into.
    pub fn start_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return self.say(Notice::warn("Another operation is still running."), cx);
        }
        let pasted = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .map(|text| text.trim().to_owned())
            .filter(|text| gitgui_core::clone::check_url(text).is_ok())
            .unwrap_or_default();
        self.dialog_input.update(cx, |input, cx| {
            input.set_text(&pasted, cx);
            input.focus(window);
        });
        self.dialog = Some(Dialog {
            title: "Clone a repository".into(),
            body: "Paste the address of a repository: https://github.com/owner/repo.git, or git@github.com:owner/repo.git for ssh. \
                   It is cloned into the folder below and opened here. Git asks for no password: a private repository needs \
                   a credential helper or an ssh key you already use."
                .into(),
            confirm: "Clone".into(),
            danger: false,
            prompt: Some(pasted),
            folder: Some(self.clone_folder()),
            check: None,
            action: Action::Clone,
        });
        cx.notify();
    }

    /// Clones as the dialog's Clone button would, with the address and folder given.
    #[cfg(test)]
    pub(crate) fn start_clone_for_test(&mut self, url: String, folder: std::path::PathBuf, cx: &mut Context<Self>) {
        self.clone_repo(url, folder, cx);
    }

    /// Picks another folder for the clone in the open dialog.
    pub fn choose_clone_folder(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(folder) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| {
                if let Some(dialog) = this.dialog.as_mut() {
                    dialog.folder = Some(folder);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Clones `url` into a folder of its name inside `folder` in the background, then opens it.
    pub(crate) fn clone_repo(&mut self, url: String, folder: std::path::PathBuf, cx: &mut Context<Self>) {
        let url = url.trim().to_owned();
        if let Err(error) = gitgui_core::clone::check_url(&url) {
            return self.fail(explain(&error), cx);
        }
        let Some(name) = gitgui_core::clone::repo_name(&url) else {
            return self.fail("Could not tell a folder name from that address.", cx);
        };
        self.settings.clone_dir = Some(folder.clone());
        self.save_settings();
        self.busy = Some(format!("Cloning {name}…").into());
        cx.notify();
        self.spawn_load(
            cx,
            move || gitgui_core::clone::clone(&url, &folder, &name).map(|dest| (dest, name)),
            move |this, result, cx| {
                this.busy = None;
                match result {
                    Ok((dest, name)) => {
                        let note = format!("Cloned {name} into {}.", dest.display());
                        this.add_folder_noting(&dest, Some(note), cx);
                    }
                    Err(error) => this.fail(explain(&error), cx),
                }
            },
        );
    }

    // ---- doing it -----------------------------------------------------------------------------

    pub fn say(&mut self, notice: Notice, cx: &mut Context<Self>) {
        self.notice = Some(notice);
        cx.notify();
    }

    /// Runs `job` on a background thread, then reads the repository again and reports. `if_unmerged`
    /// names a branch to offer a forced delete for when git says it is not fully merged.
    pub(crate) fn run(
        &mut self,
        busy: String,
        done: String,
        if_unmerged: Option<String>,
        job: impl FnOnce(GitCli) -> Result<Outcome, Error> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.repo.as_ref().map(|repo| repo.project.path.clone()) else { return };
        self.busy = Some(busy.into());
        cx.notify();
        self.spawn_load(
            cx,
            move || job(GitCli::new(path)),
            move |this, result, cx| {
                this.busy = None;
                this.notice = Some(match result {
                    Ok(Outcome::Done(_)) => Notice::info(done),
                    Ok(Outcome::Conflicts { operation, files }) => {
                        // The first file opens in the resolver once the repository has been read again.
                        this.open_next_conflict = true;
                        Notice::warn(format!(
                            "The {} stopped: {files} file{} ha{} conflicts. Choose what to keep in each, then Continue {}; or abort \
                             to put everything back.",
                            operation.name(),
                            if files == 1 { "" } else { "s" },
                            if files == 1 { "s" } else { "ve" },
                            operation.name()
                        ))
                    }
                    Err(error) => {
                        let text = explain(&error);
                        let force = if_unmerged.filter(|_| text.contains("not fully merged"));
                        Notice {
                            action: force.map(|branch| ("Delete anyway".into(), NoticeAction::ForceDelete(branch))),
                            ..Notice::warn(text)
                        }
                    }
                });
                this.refresh_keeping_notice(cx);
            },
        );
    }

    /// Brings in what the remotes have (no local branch moves), then reads the repository again.
    pub fn fetch(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return self.say(Notice::warn("Another operation is still running."), cx);
        }
        // One the app started on its own is already doing it.
        if self.auto_fetching {
            return;
        }
        self.note_fetched(cx);
        self.run("Fetching…".into(), "Fetched: the remote branches are up to date.".into(), None, |git| git.fetch().map(Outcome::Done), cx);
    }

    /// The header's Push: asks, then pushes the checked-out branch, publishing it when it is not on a remote yet.
    pub fn push_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.current_branch_name() {
            Some(branch) => self.choose(Action::Push(branch), window, cx),
            None => self.say(Notice::warn("Switch to a branch first: a detached HEAD has no branch to push."), cx),
        }
    }

    /// The banner's button.
    pub fn run_notice_action(&mut self, action: NoticeAction, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        match action {
            NoticeAction::ForceDelete(name) => self.run(
                format!("Deleting {name}…"),
                format!("Deleted {name}."),
                None,
                move |git| git.delete_branch(&name, true).map(Outcome::Done),
                cx,
            ),
        }
    }

    // ---- drawing ------------------------------------------------------------------------------

    pub fn render_overlays(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        [
            self.render_menu(window, cx),
            self.render_dialog(cx),
            self.render_new_branch(window, cx),
            self.render_branch_picker(cx),
            self.render_settings(window, cx),
        ]
            .into_iter()
            .flatten()
            .collect()
    }

    fn render_menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let items = self.menu_items(&menu.target);
        if items.is_empty() {
            return None;
        }
        let height: f32 = items.iter().map(|i| if i.action.is_some() { ITEM_HEIGHT } else { SEPARATOR_HEIGHT }).sum::<f32>() + 8.;
        let viewport = window.viewport_size();
        let x = f32::from(menu.position.x).min(f32::from(viewport.width) - MENU_WIDTH - 8.).max(8.);
        let y = f32::from(menu.position.y).min(f32::from(viewport.height) - height - 8.).max(8.);

        let rows: Vec<AnyElement> = items
            .into_iter()
            .enumerate()
            .map(|(i, item)| match item.action {
                None => div().h(px(SEPARATOR_HEIGHT)).flex().items_center().child(div().w_full().h(px(1.)).bg(rgb(t().border))).into_any_element(),
                Some(action) => {
                    let enabled = item.enabled;
                    div()
                        .id(("menu-item", i))
                        .h(px(ITEM_HEIGHT))
                        .px_3()
                        .flex()
                        .items_center()
                        .rounded_sm()
                        .text_color(rgb(if enabled { t().text } else { t().muted }))
                        .when(enabled, |row| {
                            row.cursor_pointer()
                                .hover(|style| style.bg(rgb(t().selected)).text_color(rgb(t().text_strong)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.close_menu(cx);
                                    this.choose(action.clone(), window, cx);
                                }))
                        })
                        .child(item.label)
                        .into_any_element()
                }
            })
            .collect();

        let dismiss = |this: &mut Self, cx: &mut Context<Self>| this.close_menu(cx);
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(
                    div()
                        .id("menu-backdrop")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| dismiss(this, cx)))
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| dismiss(this, cx))),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(MENU_WIDTH))
                        .p_1()
                        .rounded_md()
                        .bg(rgb(t().card))
                        .border_1()
                        .border_color(rgb(t().border))
                        .shadow_lg()
                        .occlude()
                        .text_sm()
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog.as_ref()?;
        let confirm = button("dialog-confirm", dialog.confirm.clone())
            .bg(rgb(if dialog.danger { 0xc0392b } else { t().accent }))
            .text_color(rgb(if dialog.danger { 0xffffff } else { t().on_accent }))
            .font_weight(FontWeight::BOLD)
            .on_click(cx.listener(|this, _, _, cx| this.confirm_dialog(cx)));
        Some(modal(
            div()
                .w(px(460.))
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(div().text_base().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child(dialog.title.clone()))
                .child(div().text_color(rgb(t().text)).child(dialog.body.clone()))
                .when_some(dialog.check.as_ref().map(|check| check_line(&dialog.action, check)), |panel, (text, tone)| {
                    panel.child(
                        div()
                            .debug_selector(|| "dialog-check".to_owned())
                            .px_2()
                            .py_1p5()
                            .rounded_md()
                            .bg(rgb(crate::theme::mix(t().panel, tone, 0.12)))
                            .text_xs()
                            .text_color(rgb(if tone == t().muted { t().muted } else { t().text_strong }))
                            .child(text),
                    )
                })
                .when(dialog.prompt.is_some(), |panel| panel.child(self.dialog_input.clone()))
                // A branch name that is not how this team names them, said while it is typed.
                .when_some(
                    matches!(dialog.action, Action::CreateBranch(_) | Action::RenameBranch(_))
                        .then(|| self.workflow_hint(self.dialog_input.read(cx).text()))
                        .flatten(),
                    |panel, hint| panel.child(div().text_xs().text_color(rgb(t().warning)).child(hint)),
                )
                .when_some(dialog.folder.clone(), |panel, folder| {
                    panel.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .child(div().flex_none().text_color(rgb(t().muted)).child("Into"))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .line_clamp(1)
                                    .text_ellipsis()
                                    .font_family(crate::ui::MONO)
                                    .child(SharedString::from(folder.display().to_string())),
                            )
                            .child(button("dialog-folder", "Change…").on_click(cx.listener(|this, _, _, cx| this.choose_clone_folder(cx)))),
                    )
                })
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(button("dialog-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| this.cancel_dialog(cx))))
                        .child(confirm),
                ),
        ))
    }
}

/// The question before pushing `branch`: its title, what will happen, and the confirm button's word, from where the
/// branch stood against its upstream when last read. It names where the push goes as `push_branch` does: the same
/// name, on the upstream's remote, or `origin` (else the first remote) when it has no upstream yet.
pub(crate) fn push_question(branch: &str, upstream: &Upstream, remotes: &[String]) -> (String, String, &'static str) {
    let commits = |n: usize| if n == 1 { "1 commit".to_owned() } else { format!("{n} commits") };
    let never = "Nothing is forced: if the remote has commits you do not, the push is rejected and nothing changes there.";
    match upstream {
        Upstream::None => match default_remote(remotes) {
            Some(remote) => (
                format!("Publish {branch} to {remote}?"),
                format!(
                    "`{branch}` is not on a remote yet. This pushes it to `{remote}/{branch}` and makes that its upstream, \
                     so others can see it and Pull follows it. {never}"
                ),
                "Publish",
            ),
            None => (format!("Push {branch}?"), "This repository has no remote to push to; add one first.".to_owned(), "Push"),
        },
        Upstream::Gone { name, remote } => (
            format!("Push {branch} to {remote} again?"),
            format!("`{name}` was deleted from the remote. This pushes `{branch}` there again, so it is back for others. {never}"),
            "Push",
        ),
        Upstream::Tracking { name, remote, ahead, behind } => {
            let target = format!("{remote}/{branch}");
            let what = if *behind > 0 {
                format!(
                    "`{name}` has {} that `{branch}` does not, as of the last fetch, so the push will be rejected: Pull first, then push.",
                    commits(*behind)
                )
            } else if *ahead > 0 {
                format!("Sends {} that `{target}` does not have yet.", commits(*ahead))
            } else {
                format!("`{target}` already has everything on `{branch}`, as of the last fetch: there is nothing new to send.")
            };
            // A branch can track a remote branch of another name; the push still goes to its own name, said in full.
            let (to, elsewhere) =
                if *name == target { (remote.clone(), String::new()) } else { (target.clone(), format!(" Its upstream stays `{name}`.")) };
            (format!("Push {branch} to {to}?"), format!("{what}{elsewhere} {never}"), "Push")
        }
    }
}

/// What the pull question says first: how far apart the branch and its upstream were at the last fetch.
fn pull_lead(branch: &str, upstream: &Upstream) -> String {
    let commits = |n: usize| if n == 1 { "1 commit".to_owned() } else { format!("{n} commits") };
    match upstream {
        Upstream::Tracking { name, behind: 0, .. } => format!("`{name}` had nothing new at the last fetch; this fetches again to be sure. "),
        Upstream::Tracking { name, ahead: 0, behind, .. } => format!("`{name}` has {} that `{branch}` does not. ", commits(*behind)),
        Upstream::Tracking { name, ahead, behind, .. } => {
            format!("`{name}` has {} that `{branch}` does not, and your {} go on top of them. ", commits(*behind), commits(*ahead))
        }
        Upstream::None | Upstream::Gone { .. } => String::new(),
    }
}

/// What the question says about its test merge, and the color to say it in. A merge and a cherry-pick are tested as
/// git will run them, so their answer is certain; a rebase or pull replays commits one by one, so the test of the two
/// tips is only a hint, and says so.
pub(crate) fn check_line(action: &Action, check: &Check) -> (String, u32) {
    let files = |paths: &[String]| {
        let named: Vec<&str> = paths.iter().take(3).map(|p| p.rsplit('/').next().unwrap_or(p)).collect();
        let more = paths.len().saturating_sub(3);
        let list = named.join(", ");
        let count = if paths.len() == 1 { "1 file".to_owned() } else { format!("{} files", paths.len()) };
        if more > 0 { format!("{count}: {list} and {more} more") } else { format!("{count}: {list}") }
    };
    let exact = matches!(action, Action::Merge(_) | Action::CherryPick(_));
    let lead = if *action == Action::PullRebase { "As of the last fetch, a" } else { "A" };
    match check {
        Check::Running => ("Checking for conflicts with a test merge…".to_owned(), t().muted),
        Check::Found(Preflight::Clean) if exact => (
            format!("✓ {} cleanly: a test merge found no conflicts.", if matches!(action, Action::Merge(_)) { "Merges" } else { "Applies" }),
            t().added,
        ),
        Check::Found(Preflight::Clean) => (
            format!("{lead} test merge of the two tips is clean, so this will probably go through; replaying commit by commit can still stop."),
            t().added,
        ),
        Check::Found(Preflight::Conflicts(paths)) if exact => {
            (format!("Would conflict in {}. You can resolve them here, file by file.", files(paths)), t().warning)
        }
        Check::Found(Preflight::Conflicts(paths)) => {
            (format!("May conflict in {} ({} test merge of the two tips conflicts there).", files(paths), lead.to_lowercase()), t().warning)
        }
    }
}

/// A dimmed backdrop with `content` centered on a panel.
pub(crate) fn modal(content: impl IntoElement) -> AnyElement {
    div()
        .id("modal-backdrop")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x00000099))
        .occlude()
        .child(
            div()
                .rounded_lg()
                .flex()
                .flex_col()
                .bg(rgb(t().panel))
                .border_1()
                .border_color(rgb(t().border))
                .shadow_lg()
                .text_sm()
                .text_color(rgb(t().text))
                .child(content),
        )
        .into_any_element()
}
