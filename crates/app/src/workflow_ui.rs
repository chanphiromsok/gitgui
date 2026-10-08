//! The team's branching workflow in the app: what the repository's branches show of it, a "New branch" window that
//! builds a name the way the team does and starts it from the right branch, a Settings page to correct what was
//! learned, and a hint when a name does not follow the habit.

use std::collections::HashSet;

use gitgui_core::{Detected, Error, Evidence, Outcome, RefKind, Shape, branch_name, detect, name_problem};
use gitgui_store::WorkflowSetting;
use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px, rgb};

use crate::menu::{explain, modal};
use crate::settings_view::card;
use crate::theme::t;
use crate::ui::{MONO, button, segment, segmented, toggle};
use crate::workspace::{Phase, Workspace};

/// Types that a "New branch" does not offer: a release is named by its version, which is its own thing.
const VERSIONED: [&str; 2] = ["release", "releases"];
/// What is offered to choose from beside the types the repository shows.
const COMMON_TYPES: [&str; 9] = ["feature", "feat", "bugfix", "fix", "hotfix", "chore", "docs", "refactor", "test"];
/// Trunk-like branches, best first.
const TRUNKS: [&str; 4] = ["develop", "main", "master", "trunk"];

/// The window for a new branch, while it is open.
pub struct NewBranch {
    /// The type whose prefix the name starts with; none until one is clicked, because a name does not have to have one.
    pub kind: String,
    /// The branch (or remote branch) it starts from.
    pub base: String,
    /// Switch to it once made.
    pub switch: bool,
    /// Push it to the remote once made, so it is there for others and has a remote branch to pull from.
    pub publish: bool,
}

/// The list of branches to start from, while it is open.
pub struct BranchPicker {
    /// Which of the branches listed (not counting headings) Enter would choose.
    pub highlight: usize,
}

/// One line of that list.
pub(crate) enum PickRow {
    Heading(&'static str),
    Branch { name: String, remote: bool, note: Option<&'static str> },
}

/// How many branches the list draws at most; the search narrows the rest.
const PICK_LIMIT: usize = 150;

/// The branches there are, local and remote, read from every commit's refs.
#[derive(Default)]
pub struct Branches {
    pub locals: Vec<String>,
    /// With the remote's name: `origin/develop`.
    pub remotes: Vec<String>,
}

impl Branches {
    /// `name` as something to start a branch from: the local branch, or else the remote one of that name.
    pub fn resolve(&self, name: &str) -> Option<String> {
        if self.locals.iter().any(|b| b == name) {
            return Some(name.to_owned());
        }
        self.remotes.iter().find(|r| r.split_once('/').is_some_and(|(_, short)| short == name) || *r == name).cloned()
    }

    pub fn exists(&self, name: &str) -> bool {
        self.locals.iter().any(|b| b == name)
    }

    /// A branch's name without the remote it is on.
    fn short<'a>(&self, name: &'a str) -> &'a str {
        match name.split_once('/') {
            Some((remote, rest)) if self.remotes.iter().any(|r| r.split_once('/').is_some_and(|(r, _)| r == remote)) => rest,
            _ => name,
        }
    }
}

impl Workspace {
    /// Every branch of the open project.
    pub fn branches(&self) -> Option<Branches> {
        let Phase::Ready(view) = &self.repo.as_ref()?.phase else { return None };
        let mut branches = Branches::default();
        let (mut seen_local, mut seen_remote) = (HashSet::new(), HashSet::new());
        for commit in view.commits.iter() {
            for r in &commit.refs {
                match r.kind {
                    RefKind::LocalBranch if seen_local.insert(r.name.as_str()) => branches.locals.push(r.name.clone()),
                    // `origin/HEAD` is a pointer, not a branch.
                    RefKind::RemoteBranch if !r.name.ends_with("/HEAD") && seen_remote.insert(r.name.as_str()) => {
                        branches.remotes.push(r.name.clone())
                    }
                    _ => {}
                }
            }
        }
        Some(branches)
    }

    /// What the branches show of how the team works.
    pub fn detected_workflow(&self) -> Option<Detected> {
        let branches = self.branches()?;
        let Phase::Ready(view) = &self.repo.as_ref()?.phase else { return None };
        // Each branch once, without its remote's name.
        let mut names: Vec<&str> = Vec::new();
        for name in branches.locals.iter().map(String::as_str).chain(branches.remotes.iter().map(|r| branches.short(r))) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        let into: Vec<&str> = view.clues.iter().map(|clue| branches.short(&clue.into)).collect();
        let contained = view.clues.iter().filter(|clue| clue.evidence == Evidence::Contained).count();
        let squashed = view.clues.len() - contained;
        let trunks: Vec<&str> = TRUNKS.iter().copied().filter(|t| names.contains(t)).collect();
        Some(detect(&names, &into, contained, squashed, &trunks))
    }

    /// What this project's workflow was set to, if it was.
    pub fn saved_workflow(&self) -> Option<WorkflowSetting> {
        let path = &self.repo.as_ref()?.project.path;
        self.projects.iter().find(|p| &p.path == path)?.workflow.clone()
    }

    /// The workflow in use: the one that was saved, else what the branches show; and whether it was saved.
    pub fn workflow_in_use(&self) -> Option<(WorkflowSetting, bool)> {
        if let Some(saved) = self.saved_workflow() {
            return Some((saved, true));
        }
        let detected = self.detected_workflow()?;
        Some((setting_of(&detected), false))
    }

    /// Keeps `workflow` as this project's, or forgets it (so what the branches show is used again).
    pub fn save_workflow(&mut self, workflow: Option<WorkflowSetting>, cx: &mut Context<Self>) {
        let (Some(store), Some(repo)) = (self.store.as_ref(), self.repo.as_ref()) else { return };
        let path = repo.project.path.clone();
        if let Err(err) = store.set_workflow(&path, workflow) {
            return self.fail(err, cx);
        }
        if let Ok(projects) = store.projects() {
            self.projects = projects;
        }
        cx.notify();
    }

    /// Branches to offer to start from: the workflow's base, the current branch, then the usual trunks, each as the
    /// thing to start from (a remote branch when there is no local one).
    pub fn base_choices(&self, preferred: Option<&str>) -> Vec<String> {
        let Some(branches) = self.branches() else { return Vec::new() };
        let mut choices: Vec<String> = Vec::new();
        let current = self.current_branch_name();
        for name in preferred.into_iter().chain(current.as_deref()).chain(TRUNKS) {
            if let Some(found) = branches.resolve(name)
                && !choices.contains(&found)
            {
                choices.push(found);
            }
        }
        choices.truncate(4);
        choices
    }

    // ---- the New branch window ------------------------------------------------------------------

    pub fn open_new_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return self.say(crate::menu::Notice::warn("Another operation is still running."), cx);
        }
        let Some((workflow, _)) = self.workflow_in_use() else { return };
        let base = self
            .base_choices(workflow.base.as_deref())
            .into_iter()
            .next()
            .or_else(|| self.current_branch_name())
            .unwrap_or_default();
        for input in [&self.branch_ticket, &self.branch_title] {
            input.update(cx, |input, cx| input.clear(cx));
        }
        let shape = Shape::from_id(&workflow.shape).unwrap_or(Shape::TypeSlug);
        let first = if shape.uses_ticket() { &self.branch_ticket } else { &self.branch_title };
        first.update(cx, |input, _| input.focus(window));
        self.new_branch = Some(NewBranch { kind: String::new(), base, switch: true, publish: false });
        cx.notify();
    }

    pub fn close_new_branch(&mut self, cx: &mut Context<Self>) {
        self.branch_picker = None;
        if self.new_branch.take().is_some() {
            cx.notify();
        }
    }

    /// Starts the name with `kind`'s prefix; the empty string takes it off.
    pub fn set_branch_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        if let Some(wizard) = self.new_branch.as_mut() {
            wizard.kind = kind.to_owned();
            cx.notify();
        }
    }

    /// A click on a type: fills in its prefix, or takes it off when it is the one already there.
    pub fn toggle_branch_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        let on = self.new_branch.as_ref().is_some_and(|wizard| wizard.kind == kind);
        self.set_branch_kind(if on { "" } else { kind }, cx);
    }

    pub fn set_branch_base(&mut self, base: &str, cx: &mut Context<Self>) {
        if let Some(wizard) = self.new_branch.as_mut() {
            wizard.base = base.to_owned();
            cx.notify();
        }
    }

    pub fn toggle_branch_switch(&mut self, cx: &mut Context<Self>) {
        if let Some(wizard) = self.new_branch.as_mut() {
            wizard.switch = !wizard.switch;
            cx.notify();
        }
    }

    pub fn toggle_branch_publish(&mut self, cx: &mut Context<Self>) {
        if let Some(wizard) = self.new_branch.as_mut() {
            wizard.publish = !wizard.publish;
            cx.notify();
        }
    }

    /// The name the window would make from what is typed in it.
    pub fn typed_branch_name(&self, cx: &gpui::App) -> String {
        let (Some(wizard), Some((workflow, _))) = (self.new_branch.as_ref(), self.workflow_in_use()) else { return String::new() };
        let shape = Shape::from_id(&workflow.shape).unwrap_or(Shape::TypeSlug);
        branch_name(shape, &wizard.kind, self.branch_ticket.read(cx).text(), self.branch_title.read(cx).text())
    }

    /// Makes the branch as the window says, starting from the chosen branch, and keeps these choices as the project's
    /// workflow if none was saved yet.
    pub fn create_new_branch(&mut self, cx: &mut Context<Self>) {
        let name = self.typed_branch_name(cx);
        let Some(wizard) = self.new_branch.as_ref() else { return };
        if name.is_empty() {
            return;
        }
        if self.branches().is_some_and(|branches| branches.exists(&name)) {
            return;
        }
        let has_remote = self.branches().is_some_and(|branches| !branches.remotes.is_empty());
        let (base, switch, publish) = (wizard.base.clone(), wizard.switch, wizard.publish && has_remote);
        if self.saved_workflow().is_none()
            && let Some((workflow, _)) = self.workflow_in_use()
        {
            self.save_workflow(Some(WorkflowSetting { base: Some(self.branches().map_or(base.clone(), |b| b.short(&base).to_owned())), ..workflow }), cx);
        }
        self.new_branch = None;
        let done = format!(
            "Created {name} from {base}{}{}.",
            if switch { " and switched to it" } else { "" },
            if publish { ", and pushed it" } else { "" }
        );
        let shown = name.clone();
        self.run(
            format!("Creating {shown}…"),
            done,
            None,
            move |git| {
                git.create_branch(&name, Some(&base), switch)?;
                if publish {
                    // It exists now; a push that fails leaves it made, and says so.
                    git.push_branch(&name).map_err(|err| Error::Parse(format!("Created {name}, but pushing it failed: {}", explain(&err))))?;
                }
                Ok(Outcome::Done(String::new()))
            },
            cx,
        );
    }

    pub(crate) fn render_new_branch(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let wizard = self.new_branch.as_ref()?;
        let (workflow, saved) = self.workflow_in_use()?;
        let shape = Shape::from_id(&workflow.shape).unwrap_or(Shape::TypeSlug);
        let branches = self.branches().unwrap_or_default();
        let name = self.typed_branch_name(cx);
        let exists = !name.is_empty() && branches.exists(&name);
        // The remote a new branch would be pushed to: the one `push_branch` picks.
        let remote_name = {
            let remotes: Vec<&str> = branches.remotes.iter().filter_map(|r| r.split_once('/').map(|(remote, _)| remote)).collect();
            remotes.iter().copied().find(|r| *r == "origin").or_else(|| remotes.first().copied()).map(str::to_owned)
        };

        // Types are shortcuts: a click puts the prefix on the name, another takes it off.
        let kinds: Vec<_> = offered_types(&workflow)
            .into_iter()
            .map(|kind| {
                let chosen = wizard.kind == kind;
                let value = kind.clone();
                let id = format!("kind-{kind}");
                button(SharedString::from(id.clone()), kind)
                    .debug_selector(move || id.clone())
                    .when(chosen, |chip| chip.bg(rgb(t().accent)).text_color(rgb(t().on_accent)).font_weight(FontWeight::BOLD))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_branch_kind(&value, cx)))
            })
            .collect();
        // Where it starts: the branch, and a click opens the list to search.
        let base_note = self.branches().and_then(|b| workflow.base.as_deref().and_then(|base| b.resolve(base))).filter(|base| *base == wizard.base).map(|_| "team's base");
        let start = div()
            .id("branch-base")
            .debug_selector(|| "branch-base".to_owned())
            .h(px(28.))
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded_sm()
            .border_1()
            .border_color(rgb(t().input_border))
            .bg(rgb(t().input_bg))
            .cursor_pointer()
            .hover(|style| style.border_color(rgb(t().accent)))
            .on_click(cx.listener(|this, _, window, cx| this.open_branch_picker(window, cx)))
            .child(div().flex_1().min_w_0().overflow_hidden().line_clamp(1).text_ellipsis().font_family(MONO).text_color(rgb(t().text_strong)).child(SharedString::from(wizard.base.clone())))
            .children(base_note.map(|note| div().flex_none().text_xs().text_color(rgb(t().muted)).child(note)))
            .child(div().flex_none().text_xs().text_color(rgb(t().accent)).child("Change"))
            .into_any_element();
        let label = |text: &'static str| div().w(px(92.)).flex_none().text_xs().text_color(rgb(t().muted)).child(text);
        let field = |text: &'static str, control: AnyElement| div().flex().items_center().gap_2().child(label(text)).child(div().min_w_0().flex_1().child(control));
        // After "Enter" in the ticket, the title.
        if self.branch_focus_title.replace(false) {
            self.branch_title.update(cx, |input, _| input.focus(window));
        }

        let source = if saved { "this project's workflow" } else { "what this repository's branches show" };
        let preview = if name.is_empty() {
            div().font_family(MONO).text_color(rgb(t().muted)).child("type what the work is…")
        } else {
            div().font_family(MONO).font_weight(FontWeight::BOLD).text_color(rgb(if exists { t().warning } else { t().text_strong })).child(SharedString::from(name.clone()))
        };

        Some(modal(
            div()
                .w(px(560.))
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(div().text_base().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child("New branch"))
                .child(div().text_xs().text_color(rgb(t().muted)).child(format!(
                    "Named like {} — from {source}.{}",
                    shape.example(offered_types(&workflow).first().map_or("", String::as_str)),
                    if shape.uses_type() { " Click a type to add its prefix, or leave it out." } else { "" }
                )))
                .when(shape.uses_type(), |panel| panel.child(field("Type", div().flex().flex_wrap().gap_1().children(kinds).into_any_element())))
                .when(shape.uses_ticket(), |panel| panel.child(field("Ticket", self.branch_ticket.clone().into_any_element())))
                .child(field("Title", self.branch_title.clone().into_any_element()))
                .child(field("Name", preview.into_any_element()))
                .when(exists, |panel| panel.child(div().pl(px(100.)).text_xs().text_color(rgb(t().warning)).child("A branch with this name already exists.")))
                .child(field("Start from", start))
                .child(
                    div()
                        .pl(px(100.))
                        .flex()
                        .gap_2()
                        .child(toggle("branch-switch", "Switch to it", wizard.switch).on_click(cx.listener(|this, _, _, cx| this.toggle_branch_switch(cx))))
                        .children(remote_name.map(|remote| {
                            toggle("branch-publish", format!("Push to {remote}"), wizard.publish)
                                .debug_selector(|| "branch-publish".to_owned())
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_branch_publish(cx)))
                        })),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(button("new-branch-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| this.close_new_branch(cx))))
                        .child(
                            button("new-branch-create", "Create branch")
                                .debug_selector(|| "new-branch-create".to_owned())
                                .bg(rgb(if name.is_empty() || exists { t().element } else { t().accent }))
                                .text_color(rgb(if name.is_empty() || exists { t().muted } else { t().on_accent }))
                                .font_weight(FontWeight::BOLD)
                                .on_click(cx.listener(|this, _, _, cx| this.create_new_branch(cx))),
                        ),
                ),
        ))
    }

    // ---- the list of branches to start from -----------------------------------------------------

    /// What the list shows for what is typed in its search: first what the workflow suggests (while nothing is typed),
    /// then the local branches, then the remote ones; each branch once.
    pub(crate) fn branch_pick_rows(&self, query: &str) -> Vec<PickRow> {
        let Some(branches) = self.branches() else { return Vec::new() };
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let fits = |name: &str| {
            let lower = name.to_lowercase();
            terms.iter().all(|term| lower.contains(term.as_str()))
        };
        let current = self.current_branch_name();
        let team_base = self.workflow_in_use().and_then(|(workflow, _)| workflow.base).and_then(|base| branches.resolve(&base));
        let note = |name: &str| {
            if current.as_deref() == Some(name) {
                Some("current")
            } else if team_base.as_deref() == Some(name) {
                Some("team's base")
            } else {
                None
            }
        };

        let mut rows = Vec::new();
        let mut listed: HashSet<String> = HashSet::new();
        let mut count = 0;
        let mut section = |rows: &mut Vec<PickRow>, heading: &'static str, names: Vec<(String, bool)>| {
            let names: Vec<_> = names.into_iter().filter(|(name, _)| fits(name) && !listed.contains(name)).collect();
            if names.is_empty() || count >= PICK_LIMIT {
                return;
            }
            rows.push(PickRow::Heading(heading));
            for (name, remote) in names.into_iter().take(PICK_LIMIT - count) {
                listed.insert(name.clone());
                rows.push(PickRow::Branch { note: note(&name), name, remote });
                count += 1;
            }
        };
        if terms.is_empty() {
            let suggested = self.base_choices(team_base.as_deref().map(|base| branches.short(base)));
            section(&mut rows, "Suggested", suggested.into_iter().map(|name| { let remote = !branches.exists(&name); (name, remote) }).collect());
        }
        section(&mut rows, "Local", branches.locals.iter().map(|name| (name.clone(), false)).collect());
        section(&mut rows, "Remote", branches.remotes.iter().map(|name| (name.clone(), true)).collect());
        rows
    }

    /// The branches of `rows`, without the headings.
    fn pick_names(rows: &[PickRow]) -> Vec<&str> {
        rows.iter().filter_map(|row| if let PickRow::Branch { name, .. } = row { Some(name.as_str()) } else { None }).collect()
    }

    pub fn open_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wizard) = self.new_branch.as_ref() else { return };
        let base = wizard.base.clone();
        self.branch_search.update(cx, |input, cx| input.clear(cx));
        // Starts on the branch that is chosen now.
        let rows = self.branch_pick_rows("");
        let highlight = Self::pick_names(&rows).iter().position(|name| *name == base).unwrap_or(0);
        self.branch_picker = Some(BranchPicker { highlight });
        self.branch_pick_reveal.set(true);
        self.branch_search.update(cx, |input, _| input.focus(window));
        cx.notify();
    }

    /// Back to the "New branch" window, with the title ready to type in.
    pub fn close_branch_picker(&mut self, cx: &mut Context<Self>) {
        if self.branch_picker.take().is_some() {
            self.branch_focus_title.set(true);
            cx.notify();
        }
    }

    /// What is typed in the search changed: the first match is the one Enter would choose.
    pub(crate) fn search_branches_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(picker) = self.branch_picker.as_mut() {
            picker.highlight = 0;
            self.branch_pick_reveal.set(true);
            cx.notify();
        }
    }

    /// Moves the highlight; it stops at the first and last branch.
    pub fn step_branch_pick(&mut self, delta: isize, cx: &mut Context<Self>) {
        let query = self.branch_search.read(cx).text().to_owned();
        let count = Self::pick_names(&self.branch_pick_rows(&query)).len();
        if let Some(picker) = self.branch_picker.as_mut() {
            picker.highlight = picker.highlight.saturating_add_signed(delta).min(count.saturating_sub(1));
            self.branch_pick_reveal.set(true);
            cx.notify();
        }
    }

    /// Starts the new branch from the highlighted one.
    pub fn pick_highlighted_branch(&mut self, cx: &mut Context<Self>) {
        let query = self.branch_search.read(cx).text().to_owned();
        let rows = self.branch_pick_rows(&query);
        let at = self.branch_picker.as_ref().map_or(0, |picker| picker.highlight);
        if let Some(name) = Self::pick_names(&rows).get(at).map(|name| (*name).to_owned()) {
            self.pick_branch(&name, cx);
        }
    }

    pub fn pick_branch(&mut self, name: &str, cx: &mut Context<Self>) {
        self.set_branch_base(name, cx);
        self.close_branch_picker(cx);
    }

    pub(crate) fn render_branch_picker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.branch_picker.as_ref()?;
        let wizard = self.new_branch.as_ref()?;
        let query = self.branch_search.read(cx).text().to_owned();
        let rows = self.branch_pick_rows(&query);
        let names = Self::pick_names(&rows);
        let highlight = picker.highlight.min(names.len().saturating_sub(1));
        if self.branch_pick_reveal.replace(false)
            && let Some(at) = rows.iter().position(|row| matches!(row, PickRow::Branch { name, .. } if names.get(highlight) == Some(&name.as_str())))
        {
            self.branch_pick_scroll.scroll_to_item(at);
        }

        let mut seen = 0;
        let lines: Vec<AnyElement> = rows
            .iter()
            .enumerate()
            .map(|(at, row)| match row {
                PickRow::Heading(text) => div()
                    .h(px(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(t().muted))
                    .child(*text)
                    .into_any_element(),
                PickRow::Branch { name, remote, note } => {
                    let this_one = seen;
                    seen += 1;
                    let (chosen, current) = (this_one == highlight, wizard.base == *name);
                    let value = name.clone();
                    div()
                        .id(("pick-branch", at))
                        .debug_selector(move || format!("pick-branch-{this_one}"))
                        .h(px(30.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded_sm()
                        .cursor_pointer()
                        .when(chosen, |row| row.bg(rgb(t().selected)).text_color(rgb(t().text_strong)))
                        .when(!chosen, |row| row.hover(|style| style.bg(rgb(t().hover))))
                        .on_click(cx.listener(move |this, _, _, cx| this.pick_branch(&value, cx)))
                        // A solid dot for a branch of this repository, a ring for one that is only on a remote.
                        .child(if *remote {
                            div().flex_none().size(px(8.)).rounded_full().border_1().border_color(rgb(t().muted))
                        } else {
                            div().flex_none().size(px(8.)).rounded_full().bg(rgb(t().accent))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .line_clamp(1)
                                .text_ellipsis()
                                .font_family(MONO)
                                .when(current, |name| name.font_weight(FontWeight::BOLD))
                                .child(SharedString::from(name.clone())),
                        )
                        .children(note.map(|note| div().flex_none().text_xs().text_color(rgb(t().muted)).child(note)))
                        .when(current, |row| row.child(div().flex_none().text_xs().text_color(rgb(t().accent)).child("chosen")))
                        .into_any_element()
                }
            })
            .collect();

        let empty = names.is_empty();
        Some(modal(
            div()
                .w(px(480.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .p_4()
                        .pb_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_base().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child("Start from"))
                        .child(div().text_xs().text_color(rgb(t().muted)).child("The new branch begins at the latest commit of the branch you pick.")),
                )
                .child(div().px_4().pb_2().child(self.branch_search.clone()))
                .child(
                    div()
                        .id("branch-pick-list")
                        .h(px(280.))
                        .px_2()
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.branch_pick_scroll)
                        .children(lines)
                        .when(empty, |list| {
                            list.child(
                                div()
                                    .p_4()
                                    .text_color(rgb(t().muted))
                                    .child(if query.trim().is_empty() { "There are no branches yet.".to_owned() } else { format!("No branch matches “{}”.", query.trim()) }),
                            )
                        }),
                )
                .child(
                    div()
                        .px_4()
                        .py_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_t_1()
                        .border_color(rgb(t().border))
                        .child(div().flex_1().min_w_0().line_clamp(1).text_ellipsis().text_xs().text_color(rgb(t().muted)).child("Remote branches are as of the last fetch."))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                // The remote's branches are as of the last fetch; this brings them up to date.
                                .child(match self.busy.as_ref() {
                                    Some(busy) => div().text_xs().text_color(rgb(t().muted)).child(busy.clone()).into_any_element(),
                                    None => button("branch-pick-fetch", "Fetch remotes")
                                        .debug_selector(|| "branch-pick-fetch".to_owned())
                                        .on_click(cx.listener(|this, _, _, cx| this.fetch(cx)))
                                        .into_any_element(),
                                })
                                .child(button("branch-pick-back", "Back").on_click(cx.listener(|this, _, _, cx| this.close_branch_picker(cx)))),
                        ),
                ),
        ))
    }

    /// Said under the name being typed for a new or renamed branch, when it does not follow the project's workflow.
    pub(crate) fn workflow_hint(&self, typed: &str) -> Option<String> {
        let workflow = self.saved_workflow()?;
        let shape = Shape::from_id(&workflow.shape)?;
        name_problem(shape, &workflow.types, typed)
    }

    // ---- the Settings page ----------------------------------------------------------------------

    pub(crate) fn workflow_page(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(detected) = self.detected_workflow() else {
            return vec![card(
                Some("Workflow"),
                vec![div().p_4().text_color(rgb(t().muted)).child("Open a project to see how its team names and merges branches.").into_any_element()],
            )];
        };
        let saved = self.saved_workflow();
        let learned = setting_of(&detected);
        let (current, is_saved) = match &saved {
            Some(saved) => (saved.clone(), true),
            None => (learned.clone(), false),
        };
        let shape = Shape::from_id(&current.shape).unwrap_or(Shape::TypeSlug);

        let stacked = |title: &'static str, detail: &'static str, control: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .px_4()
                .py_3()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(rgb(t().text_strong)).child(title))
                .child(div().text_xs().text_color(rgb(t().muted)).child(detail))
                .child(control)
                .into_any_element()
        };
        let edit = |workflow: WorkflowSetting| {
            move |this: &mut Workspace, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Workspace>| {
                this.save_workflow(Some(workflow.clone()), cx)
            }
        };

        let shapes = segmented(
            Shape::ALL
                .into_iter()
                .map(|candidate| {
                    let next = WorkflowSetting { shape: candidate.id().to_owned(), ..current.clone() };
                    segment(SharedString::from(format!("shape-{}", candidate.id())), candidate.example("feature"), candidate == shape)
                        .on_click(cx.listener(edit(next)))
                })
                .collect(),
        );

        // The types the repository shows, then the usual ones.
        let mut kinds: Vec<String> = learned.types.clone();
        for common in COMMON_TYPES {
            if !kinds.iter().any(|k| k == common) && !VERSIONED.contains(&common) {
                kinds.push(common.to_owned());
            }
        }
        for kind in &current.types {
            if !kinds.contains(kind) {
                kinds.push(kind.clone());
            }
        }
        let types = div().flex().flex_wrap().gap_1().children(kinds.into_iter().map(|kind| {
            let on = current.types.contains(&kind);
            let mut types = current.types.clone();
            if on {
                types.retain(|k| *k != kind);
            } else {
                types.push(kind.clone());
            }
            toggle(SharedString::from(format!("type-{kind}")), kind, on)
                .on_click(cx.listener(edit(WorkflowSetting { types, ..current.clone() })))
        }));

        let starts: Vec<AnyElement> = self
            .branches()
            .map(|branches| {
                let mut names: Vec<String> = Vec::new();
                for name in current.base.iter().map(String::as_str).chain(TRUNKS).chain(branches.locals.iter().map(String::as_str)) {
                    let short = branches.short(name).to_owned();
                    if branches.resolve(&short).is_some() && !names.contains(&short) {
                        names.push(short);
                    }
                }
                names.truncate(8);
                names
            })
            .unwrap_or_default()
            .into_iter()
            .map(|name| {
                let on = current.base.as_deref() == Some(name.as_str());
                toggle(SharedString::from(format!("start-{name}")), name.clone(), on)
                    .on_click(cx.listener(edit(WorkflowSetting { base: Some(name), ..current.clone() })))
                    .into_any_element()
            })
            .collect();

        let found = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .child(div().text_color(rgb(t().text)).child(detected.describe()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        button("workflow-use", if is_saved && saved.as_ref() == Some(&learned) { "Using this" } else { "Use this" })
                            .debug_selector(|| "workflow-use".to_owned())
                            .bg(rgb(if is_saved && saved.as_ref() == Some(&learned) { t().element } else { t().accent }))
                            .text_color(rgb(if is_saved && saved.as_ref() == Some(&learned) { t().muted } else { t().on_accent }))
                            .on_click(cx.listener(edit(learned.clone()))),
                    )
                    .when(is_saved, |line| {
                        line.child(button("workflow-forget", "Forget what I set").on_click(cx.listener(|this, _, _, cx| this.save_workflow(None, cx))))
                    })
                    .child(div().text_xs().text_color(rgb(t().muted)).child(if is_saved {
                        "Saved for this project, on this computer."
                    } else {
                        "Not saved: used as it is until you change something."
                    })),
            )
            .into_any_element();

        vec![
            card(Some("What this repository shows"), vec![found]),
            card(
                Some("This project's workflow"),
                vec![
                    stacked("Branch names", "How a new branch is named. The ticket is optional when you make one.", shapes),
                    stacked("Types", "The words a branch name can start with. “New branch” offers these.", types.into_any_element()),
                    stacked("Start from", "Where new work begins. “New branch” offers it first.", div().flex().flex_wrap().gap_1().children(starts).into_any_element()),
                ],
            ),
        ]
    }
}

/// A workflow as the settings keep it, from what the branches show.
pub fn setting_of(detected: &Detected) -> WorkflowSetting {
    WorkflowSetting {
        types: detected.types.iter().map(|(kind, _)| kind.clone()).filter(|kind| !VERSIONED.contains(&kind.as_str())).collect(),
        shape: detected.shape.id().to_owned(),
        base: detected.base.clone(),
    }
}

/// The types "New branch" offers: the workflow's, or the usual ones when it has none.
fn offered_types(workflow: &WorkflowSetting) -> Vec<String> {
    let mut kinds: Vec<String> = workflow.types.iter().filter(|k| !VERSIONED.contains(&k.as_str())).cloned().collect();
    if kinds.is_empty() {
        kinds = ["feature", "bugfix", "hotfix", "chore"].map(str::to_owned).to_vec();
    }
    kinds.truncate(8);
    kinds
}
