//! Pieces of the file pane: the commit overview, and the rows of the changed-files list (as a
//! folder tree or flat).

use std::collections::HashSet;
use gitgui_core::{CommitDetail, FileChange, FileStatus, TreeRow};
use gpui::{AnyElement, Context, FontWeight, Rgba, SharedString, div, prelude::*, px, rgb};

use crate::icons;
use crate::ui::{self, MONO};
use crate::workspace::{CommitView, Phase, Workspace};
use crate::theme::t;

const ROW_H: f32 = 24.0;

pub fn status_color(status: FileStatus) -> Rgba {
    rgb(match status {
        FileStatus::Added | FileStatus::Copied => t().added,
        FileStatus::Deleted => t().removed,
        FileStatus::Modified | FileStatus::TypeChanged => t().modified,
        FileStatus::Renamed => t().link,
    })
}

/// `(+12 | -1)`, or `(binary)` for a file with no line counts.
pub fn stats(change: &FileChange) -> AnyElement {
    match (change.additions, change.deletions) {
        (Some(adds), Some(dels)) => div()
            .flex()
            .flex_none()
            .gap_1()
            .text_xs()
            .text_color(rgb(t().muted))
            .child("(")
            .child(div().text_color(rgb(t().added)).child(format!("+{adds}")))
            .child("|")
            .child(div().text_color(rgb(t().removed)).child(format!("-{dels}")))
            .child(")")
            .into_any_element(),
        _ => div().flex_none().text_xs().text_color(rgb(t().muted)).child("(binary)").into_any_element(),
    }
}

impl Workspace {
    /// The loaded details of the selected commit, if there are any yet.
    pub fn commit_view(&self) -> Option<&CommitView> {
        match &self.repo.as_ref()?.commit.as_ref()?.phase {
            Phase::Ready(view) => Some(view),
            _ => None,
        }
    }

    /// What the merge scan says about the selected commit, each with a link to the commit it points at.
    fn render_merge_notes(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let repo = self.repo.as_ref()?;
        let Phase::Ready(view) = &repo.phase else { return None };
        let entry = view.entries.get(repo.selected?)?;
        if entry.notes.is_empty() && entry.forks.is_empty() {
            return None;
        }
        let notes: Vec<AnyElement> = entry
            .notes
            .iter()
            .enumerate()
            .map(|(i, note)| {
                let jump = note.jump.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(rgb(t().added)).child(note.text.clone()))
                    .child(div().text_xs().text_color(rgb(t().muted)).child(format!(
                        "{}{}",
                        note.detail,
                        if note.probable { " This is a guess from the messages, not a certainty." } else { "" }
                    )))
                    .when_some(jump, |col, id| {
                        col.child(
                            ui::button(("show-merge", i), "Show that commit")
                                .on_click(cx.listener(move |this, _, _, cx| this.select_commit_id(&id, cx))),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .mt_4()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(t().border))
                .flex()
                .flex_col()
                .gap_3()
                .when(!entry.forks.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(rgb(t().muted))
                            .child(format!("Branch point: {} {} from this commit.", entry.forks.join(", "), if entry.forks.len() == 1 { "was branched" } else { "were branched" })),
                    )
                })
                .children(notes)
                .into_any_element(),
        )
    }

    /// The commit overview: ids, people, date and the full message.
    pub fn render_info(&self, detail: &CommitDetail, cx: &mut Context<Self>) -> AnyElement {
        let field = |label: &'static str, value: AnyElement| {
            div()
                .flex()
                .gap_2()
                .text_sm()
                .child(div().w(px(84.)).flex_none().whitespace_nowrap().text_color(rgb(t().muted)).child(label))
                .child(div().min_w_0().flex_1().child(value))
        };
        let text = |value: String| div().font_family(MONO).text_xs().child(SharedString::from(value)).into_any_element();

        let parents: Vec<AnyElement> = detail
            .parents
            .iter()
            .enumerate()
            .map(|(i, parent)| {
                let target = parent.clone();
                div()
                    .id(("parent", i))
                    .font_family(MONO)
                    .text_xs()
                    .text_color(rgb(t().link))
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .on_click(cx.listener(move |this, _, _, cx| this.select_commit_id(&target, cx)))
                    .child(SharedString::from(parent.clone()))
                    .into_any_element()
            })
            .collect();

        div()
            .id("commit-info")
            .size_full()
            .overflow_y_scroll()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(field("Commit:", text(detail.id.clone())))
            .when(!detail.parents.is_empty(), |panel| {
                panel.child(field("Parents:", div().flex().flex_col().children(parents).into_any_element()))
            })
            .child(field(
                "Author:",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child({
                        // One picture and color for the person, whichever of their identities this is.
                        let person = match self.repo.as_ref().map(|repo| &repo.phase) {
                            Some(Phase::Ready(view)) => view.people.of(&detail.author_email).cloned(),
                            _ => None,
                        };
                        let (name, email, avatar_email) = match &person {
                            Some(p) => (p.name.as_str(), p.email.as_str(), p.avatar_email.as_str()),
                            None => (detail.author.as_str(), detail.author_email.as_str(), detail.author_email.as_str()),
                        };
                        ui::avatar(name, email, self.avatar_for(avatar_email, cx), 22.)
                    })
                    .child(format!("{} <{}>", detail.author, detail.author_email))
                    .into_any_element(),
            ))
            // Said only when it is someone else (or another identity): usually it is the author again.
            .when(detail.committer != detail.author || detail.committer_email != detail.author_email, |panel| {
                panel.child(field(
                    "Committer:",
                    div().child(format!("{} <{}>", detail.committer, detail.committer_email)).into_any_element(),
                ))
            })
            .child(field("Date:", div().child(SharedString::from(detail.date.clone())).into_any_element()))
            .children(self.pr_link(detail, cx).map(|(label, link)| field(label, link)))
            .child(div().pt_2().text_sm().child(SharedString::from(detail.message.clone())))
            .children(self.render_merge_notes(cx))
            .child(div().pt_4().text_xs().text_color(rgb(t().muted)).child("Pick a file on the left to see what changed."))
            .into_any_element()
    }
}

impl Workspace {
    /// The pull request a commit merged or squashed, as a link to its page; `None` when the commit
    /// names none or the remote is not on a site this knows.
    fn pr_link(&self, detail: &CommitDetail, _cx: &mut Context<Self>) -> Option<(&'static str, AnyElement)> {
        let Phase::Ready(view) = &self.repo.as_ref()?.phase else { return None };
        let web = view.web.as_ref()?;
        let number = gitgui_core::subject_pr(detail.message.lines().next()?)?;
        let url = web.pull_request(number);
        let label = if web.pull_request_name() == "Merge Request" { "Merge request:" } else { "Pull request:" };
        let link = div()
            .id("detail-pr")
            .text_color(rgb(t().link))
            .cursor_pointer()
            .hover(|style| style.underline())
            .on_click(move |_, _, cx| cx.open_url(&url))
            .child(format!("#{number} ↗"))
            .into_any_element();
        Some((label, link))
    }
}

/// One row of the changed-files list. `selected` is the file whose diff is open.
pub fn file_row(
    ix: usize,
    row: &TreeRow,
    files: &[FileChange],
    selected: Option<usize>,
    folded: &HashSet<String>,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    let indent = |depth: usize| px(10. + depth as f32 * 14.);
    Some(match row {
        TreeRow::Dir { depth, name, path } => {
            let open = !folded.contains(path);
            let path = path.clone();
            div()
            .id(("dir", ix))
            .w_full()
            .h(px(ROW_H))
            .pl(indent(*depth))
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .text_color(rgb(t().muted))
            .hover(|style| style.bg(rgb(t().hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_dir(&path, cx)))
            .child(div().flex_none().w(px(10.)).child(if open { "▾" } else { "▸" }))
            .child(ui::file_icon(icons::folder(name, open)))
            .child(div().overflow_hidden().line_clamp(1).text_ellipsis().child(SharedString::from(name.clone())))
            .into_any_element()
        },
        TreeRow::File { depth, name, dir, index } => {
            let change = files.get(*index)?;
            let deleted = change.status == FileStatus::Deleted;
            let index = *index;
            let is_selected = selected == Some(index);
            div()
                .id(("file", ix))
                .w_full()
                .h(px(ROW_H))
                .pl(indent(*depth))
                .pr_2()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .when(is_selected, |row| row.bg(rgb(t().selected)))
                .hover(|style| style.bg(rgb(if is_selected { t().selected } else { t().hover })))
                .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    this.open_file(index, cx);
                    // Double-click: read it in full view, with the whole window for the code.
                    if event.click_count() >= 2 {
                        this.expand_pane(cx);
                    }
                }))
                .child(ui::file_icon(icons::file(name)))
                .child(
                    // Gives way (with an ellipsis) before the counts do, so a long name never pushes them out of view.
                    div()
                        .min_w(px(40.))
                        .line_clamp(1)
                        .text_ellipsis()
                        .text_color(status_color(change.status))
                        .when(deleted, |name| name.line_through())
                        .child(SharedString::from(name.clone())),
                )
                .when(deleted, |row| row.child(deleted_tag()))
                .when(!dir.is_empty(), |row| {
                    row.child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .line_clamp(1)
                            .text_ellipsis()
                            .text_xs()
                            .text_color(rgb(t().muted))
                            .child(SharedString::from(dir.clone())),
                    )
                })
                .when(dir.is_empty(), |row| row.child(div().flex_1()))
                .when_some(change.old_path.clone(), |row, old| {
                    row.child(div().text_xs().text_color(rgb(t().muted)).child(format!("← {old}")))
                })
                .child(stats(change))
                .into_any_element()
        }
    })
}

/// "(Deleted)" after the name of a file the commit removed.
pub fn added_tag() -> impl IntoElement {
    div().flex_none().text_xs().text_color(rgb(t().added)).child("(New file)")
}

pub fn deleted_tag() -> impl IntoElement {
    div().flex_none().text_xs().text_color(rgb(t().removed)).child("(Deleted)")
}

/// A hairline between panes.
#[allow(dead_code)]
pub fn divider() -> impl IntoElement {
    div().w(px(1.)).h_full().flex_none().bg(rgb(t().border))
}
