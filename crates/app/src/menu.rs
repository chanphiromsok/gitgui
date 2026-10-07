//! Right-click menus on branches and commits, the confirmations behind them, the settings panel, and
//! running what was chosen.
//!
//! Anything that changes history or touches a remote asks first, in words that say what will happen.
//! The work runs off the UI thread; when it ends the repository is read again and the result is shown
//! in the banner at the bottom: what was done, why git refused, or (for a merge, rebase or cherry-pick
//! that hit conflicts) a button that puts everything back.

use gitgui_core::{CheckoutTarget, Error, Evidence, GitCli, Label, LabelKind, Operation, Outcome};
use gpui::{
    AnyElement, ClipboardItem, Context, FontWeight, MouseButton, Pixels, Point, SharedString, Window, div, prelude::*, px,
    rgb, rgba,
};

use crate::ui::button;
use crate::workspace::{Phase, Workspace};
use crate::icons;
use crate::theme::{Origin, t};

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
    /// Make a branch at this commit.
    CreateBranch(String),
    /// Make a tag at this commit.
    CreateTag(String),
    CherryPick(String),
    Copy { text: String, what: &'static str },
    /// Open a page (a pull request, a commit) in the browser.
    OpenUrl(String),
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
    pub action: Action,
}

/// What the banner's button does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeAction {
    /// Give up a merge, rebase or cherry-pick that stopped on conflicts.
    Abort(Operation),
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

    fn current_branch_name(&self) -> Option<String> {
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
        if self.busy.is_some() {
            return self.say(Notice::warn("Another operation is still running."), cx);
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

    fn open_dialog(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
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
                     If there are conflicts the merge stops and you can abort it to put everything back."
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
                     If there are conflicts you can abort and nothing changes."
                ),
                "Rebase",
                true,
                None,
            ),
            Action::Push(branch) => (
                format!("Push {branch}?"),
                format!(
                    "Sends `{branch}` to its remote (`origin` when it has no upstream yet, which then becomes its upstream). \
                     The push is never forced: if the remote has commits you do not, it is rejected."
                ),
                "Push",
                false,
                None,
            ),
            Action::CherryPick(id) => {
                let summary = self.summary_of(id).unwrap_or_default();
                (
                    format!("Cherry-pick onto {current}?"),
                    format!(
                        "Applies the changes of {} `{summary}` as a new commit on `{current}`. \
                         If there are conflicts it stops and you can abort.",
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
            Action::Checkout(_) | Action::Copy { .. } | Action::OpenUrl(_) => return,
        };
        if let Some(initial) = &prompt {
            let initial = initial.clone();
            self.dialog_input.update(cx, |input, cx| {
                input.set_text(&initial, cx);
                input.focus(window);
            });
        }
        self.dialog = Some(Dialog { title: title.into(), body: body.into(), confirm: confirm.into(), danger, prompt, action });
        cx.notify();
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
                self.run(format!("Pushing {branch}…"), format!("Pushed {branch}."), None, move |git| git.push_branch(&branch).map(Outcome::Done), cx);
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
            Action::Checkout(_) | Action::Copy { .. } | Action::OpenUrl(_) => {}
        }
    }

    // ---- doing it -----------------------------------------------------------------------------

    pub fn say(&mut self, notice: Notice, cx: &mut Context<Self>) {
        self.notice = Some(notice);
        cx.notify();
    }

    /// Runs `job` on a background thread, then reads the repository again and reports. `if_unmerged`
    /// names a branch to offer a forced delete for when git says it is not fully merged.
    fn run(
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
                    Ok(Outcome::Conflicts { operation, files }) => Notice {
                        text: format!(
                            "The {} stopped: {files} file{} ha{} conflicts. Resolve them in your editor and finish the {} there, \
                             or abort to put everything back as it was.",
                            operation.name(),
                            if files == 1 { "" } else { "s" },
                            if files == 1 { "s" } else { "ve" },
                            operation.name()
                        )
                        .into(),
                        warn: true,
                        action: Some((format!("Abort {}", operation.name()).into(), NoticeAction::Abort(operation))),
                    },
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

    /// The banner's button.
    pub fn run_notice_action(&mut self, action: NoticeAction, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        match action {
            NoticeAction::Abort(operation) => self.run(
                format!("Aborting the {}…", operation.name()),
                format!("Aborted the {}. Everything is as it was.", operation.name()),
                None,
                move |git| git.abort(operation).map(Outcome::Done),
                cx,
            ),
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

    pub fn render_overlays(&self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        [self.render_menu(window, cx), self.render_dialog(cx), self.render_settings(cx)].into_iter().flatten().collect()
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
                .when(dialog.prompt.is_some(), |panel| panel.child(self.dialog_input.clone()))
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

    fn render_settings(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.settings_open {
            return None;
        }
        let on = self.settings.group_by_parent;
        Some(modal(
            div()
                .w(px(520.))
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(div().text_base().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child("Settings"))
                .child(setting_toggle(
                    "setting-group",
                    on,
                    "Group commits under their pull request",
                    "List the commits of a pull request or merged branch indented under it, with a guide line, \
                     and fold them away with the chevron on the merge in the graph. Squash-merged branches go under \
                     their squash commit. Turn off to list every commit in date order.",
                    cx.listener(|this, _, _, cx| this.toggle_group_by_parent(cx)),
                ))
                .child(setting_toggle(
                    "setting-avatars",
                    self.settings.fetch_avatars,
                    "Show authors' pictures",
                    "Fetch profile pictures from GitHub (for GitHub no-reply emails) and Gravatar, which is sent a \
                     SHA-256 of each author's email. Pictures are kept for a week. Off shows initials only.",
                    cx.listener(|this, _, _, cx| this.toggle_fetch_avatars(cx)),
                ))
                .child(setting_toggle(
                    "setting-compact",
                    self.settings.compact_graph,
                    "Compact graph",
                    "Narrow lanes and thin lines, so a busy history leaves more room for the messages.",
                    cx.listener(|this, _, _, cx| this.toggle_compact_graph(cx)),
                ))
                .child(self.render_theme_picker(cx))
                .child(self.render_icon_picker(cx))
                .child(div().flex().justify_end().child(button("settings-done", "Done").on_click(cx.listener(|this, _, _, cx| this.close_settings(cx))))),
        ))
    }

    /// The built-in icons, then each icon theme installed in Zed.
    fn render_icon_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.settings.icon_theme.clone();
        let names: Vec<Option<String>> =
            std::iter::once(None).chain(self.icon_themes.iter().map(|t| Some(t.name.clone()))).collect();
        let rows = names.into_iter().enumerate().map(|(ix, name)| {
            let chosen = name == current;
            let label = name.clone().unwrap_or_else(|| icons::BUILT_IN.to_owned());
            div()
                .id(("icon-theme", ix))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .when(chosen, |row| row.bg(rgb(t().selected)))
                .hover(|style| style.bg(rgb(t().hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.set_icon_theme(name.as_deref(), cx)))
                .child(div().flex_1().child(label))
                .when(ix > 0, |row| row.child(div().text_xs().text_color(rgb(t().muted)).child("from Zed")))
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("File icons"))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(t().muted))
                    .child("Icon themes installed in Zed (Material, Catppuccin, …) show up here."),
            )
            .child(
                div()
                    .id("icon-theme-list")
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .border_1()
                    .border_color(rgb(t().border))
                    .rounded_sm()
                    .p_1()
                    .children(rows),
            )
            .into_any_element()
    }

    /// Every theme found, each with a small preview of its colors; click one to use it.
    fn render_theme_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = t().name.clone();
        let where_from = |origin: &Origin| match origin {
            Origin::BuiltIn => "built in",
            Origin::User(_) => "your themes",
            Origin::Zed(_) => "from Zed",
        };
        let rows = self.themes.iter().enumerate().map(|(ix, theme)| {
            let chosen = theme.name == current;
            let name = theme.name.clone();
            let swatch = div()
                .flex_none()
                .w(px(44.))
                .h(px(18.))
                .rounded_sm()
                .border_1()
                .border_color(rgb(t().border))
                .bg(rgb(theme.editor_bg))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(3.))
                .children([theme.accent, theme.syntax_hint(), theme.added, theme.removed].map(|color| {
                    div().size(px(6.)).rounded_full().bg(rgb(color))
                }));
            div()
                .id(("theme", ix))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .when(chosen, |row| row.bg(rgb(t().selected)))
                .hover(|style| style.bg(rgb(t().hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.set_theme(&name, cx)))
                .child(swatch)
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(theme.name.clone()))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(rgb(t().muted))
                        .child(format!("{} · {}", if theme.dark { "dark" } else { "light" }, where_from(&theme.origin))),
                )
        });
        let folder = self.store_dir().map(|dir| dir.join("themes").display().to_string()).unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Theme"))
            .child(div().text_xs().text_color(rgb(t().muted)).child(format!(
                "Zed themes work as they are: themes installed in Zed show up here, or put a Zed theme file in {folder}."
            )))
            .child(
                div()
                    .id("theme-list")
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .border_1()
                    .border_color(rgb(t().border))
                    .rounded_sm()
                    .p_1()
                    .children(rows),
            )
            .into_any_element()
    }
}

/// A checkbox with a title and a line saying what it does.
fn setting_toggle(
    id: &'static str,
    on: bool,
    title: &'static str,
    detail: &'static str,
    toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .gap_3()
        .cursor_pointer()
        .on_click(toggle)
        .child(
            div()
                .flex_none()
                .mt(px(2.))
                .size(px(16.))
                .rounded_sm()
                .border_1()
                .border_color(rgb(if on { t().accent } else { t().muted }))
                .when(on, |b| b.bg(rgb(t().accent)))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(t().on_accent))
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .child(if on { "✓" } else { "" }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(div().text_xs().text_color(rgb(t().muted)).child(detail)),
        )
}

/// A dimmed backdrop with `content` centered on a panel.
fn modal(content: impl IntoElement) -> AnyElement {
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
