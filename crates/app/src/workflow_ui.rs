//! The team's branching workflow in the app: what the repository's branches show of it, a "New branch" window that
//! builds a name the way the team does and starts it from the right branch, a Settings page to correct what was
//! learned, and a hint when a name does not follow the habit.

use std::collections::HashSet;

use gitgui_core::{Detected, Evidence, Outcome, RefKind, Shape, branch_name, detect, name_problem};
use gitgui_store::WorkflowSetting;
use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px, rgb};

use crate::menu::modal;
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
    pub kind: String,
    /// The branch (or remote branch) it starts from.
    pub base: String,
    /// Switch to it once made.
    pub switch: bool,
}

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
        let kind = offered_types(&workflow).into_iter().next().unwrap_or_default();
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
        self.new_branch = Some(NewBranch { kind, base, switch: true });
        cx.notify();
    }

    pub fn close_new_branch(&mut self, cx: &mut Context<Self>) {
        if self.new_branch.take().is_some() {
            cx.notify();
        }
    }

    pub fn set_branch_kind(&mut self, kind: &str, cx: &mut Context<Self>) {
        if let Some(wizard) = self.new_branch.as_mut() {
            wizard.kind = kind.to_owned();
            cx.notify();
        }
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
        let (base, switch) = (wizard.base.clone(), wizard.switch);
        if self.saved_workflow().is_none()
            && let Some((workflow, _)) = self.workflow_in_use()
        {
            self.save_workflow(Some(WorkflowSetting { base: Some(self.branches().map_or(base.clone(), |b| b.short(&base).to_owned())), ..workflow }), cx);
        }
        self.new_branch = None;
        let done = format!("Created {name} from {base}{}.", if switch { " and switched to it" } else { "" });
        let shown = name.clone();
        self.run(format!("Creating {shown}…"), done, None, move |git| git.create_branch(&name, Some(&base), switch).map(Outcome::Done), cx);
    }

    pub(crate) fn render_new_branch(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let wizard = self.new_branch.as_ref()?;
        let (workflow, saved) = self.workflow_in_use()?;
        let shape = Shape::from_id(&workflow.shape).unwrap_or(Shape::TypeSlug);
        let branches = self.branches().unwrap_or_default();
        let name = self.typed_branch_name(cx);
        let exists = !name.is_empty() && branches.exists(&name);

        let kinds: Vec<_> = offered_types(&workflow)
            .into_iter()
            .map(|kind| {
                let chosen = wizard.kind == kind;
                let value = kind.clone();
                segment(SharedString::from(format!("kind-{kind}")), kind, chosen)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_branch_kind(&value, cx)))
            })
            .collect();
        let bases: Vec<_> = self
            .base_choices(workflow.base.as_deref())
            .into_iter()
            .map(|base| {
                let chosen = wizard.base == base;
                let value = base.clone();
                segment(SharedString::from(format!("base-{base}")), base, chosen)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_branch_base(&value, cx)))
            })
            .collect();
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
                .child(div().text_xs().text_color(rgb(t().muted)).child(format!("Named like {} — from {source}.", shape.example(&wizard.kind))))
                .when(shape.uses_type(), |panel| panel.child(field("Type", div().flex().flex_wrap().gap_1().children(kinds).into_any_element())))
                .when(shape.uses_ticket(), |panel| panel.child(field("Ticket", self.branch_ticket.clone().into_any_element())))
                .child(field("Title", self.branch_title.clone().into_any_element()))
                .child(field("Name", preview.into_any_element()))
                .when(exists, |panel| panel.child(div().pl(px(100.)).text_xs().text_color(rgb(t().warning)).child("A branch with this name already exists.")))
                .child(field("Start from", div().flex().flex_wrap().gap_1().children(bases).into_any_element()))
                .child(div().pl(px(100.)).flex().child(toggle("branch-switch", "Switch to it", wizard.switch).on_click(cx.listener(|this, _, _, cx| this.toggle_branch_switch(cx)))))
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
