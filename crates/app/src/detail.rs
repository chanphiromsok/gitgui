//! Pieces of the file pane: the commit overview, and the rows of the changed-files list (as a
//! folder tree or flat).

use gitgui_core::{CommitDetail, FileChange, FileStatus, TreeRow};
use gpui::{AnyElement, Context, FontWeight, Rgba, SharedString, div, prelude::*, px, rgb};

use crate::ui::{self, ADDED, BORDER, LINK, MODIFIED, MONO, MUTED, REMOVED};
use crate::workspace::{CommitView, Phase, Workspace};

const ROW_H: f32 = 24.0;

pub fn status_color(status: FileStatus) -> Rgba {
    rgb(match status {
        FileStatus::Added | FileStatus::Copied => ADDED,
        FileStatus::Deleted => REMOVED,
        FileStatus::Modified | FileStatus::TypeChanged => MODIFIED,
        FileStatus::Renamed => LINK,
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
            .text_color(rgb(MUTED))
            .child("(")
            .child(div().text_color(rgb(ADDED)).child(format!("+{adds}")))
            .child("|")
            .child(div().text_color(rgb(REMOVED)).child(format!("-{dels}")))
            .child(")")
            .into_any_element(),
        _ => div().flex_none().text_xs().text_color(rgb(MUTED)).child("(binary)").into_any_element(),
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
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(rgb(ADDED)).child(note.text.clone()))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
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
                .border_color(rgb(BORDER))
                .flex()
                .flex_col()
                .gap_3()
                .when(!entry.forks.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
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
                .child(div().w(px(76.)).flex_none().font_weight(FontWeight::BOLD).child(label))
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
                    .text_color(rgb(LINK))
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
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .child(field("Commit:", text(detail.id.clone())))
            .when(!detail.parents.is_empty(), |panel| {
                panel.child(field("Parents:", div().flex().flex_col().children(parents).into_any_element()))
            })
            .child(field("Author:", div().child(format!("{} <{}>", detail.author, detail.author_email)).into_any_element()))
            .child(field(
                "Committer:",
                div().child(format!("{} <{}>", detail.committer, detail.committer_email)).into_any_element(),
            ))
            .child(field("Date:", div().child(SharedString::from(detail.date.clone())).into_any_element()))
            .child(div().pt_3().child(SharedString::from(detail.message.clone())))
            .children(self.render_merge_notes(cx))
            .child(div().pt_4().text_xs().text_color(rgb(MUTED)).child("Pick a file on the left to see what changed."))
            .into_any_element()
    }
}

/// One row of the changed-files list. `selected` is the file whose diff is open.
pub fn file_row(
    ix: usize,
    row: &TreeRow,
    files: &[FileChange],
    selected: Option<usize>,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    let indent = |depth: usize| px(10. + depth as f32 * 14.);
    Some(match row {
        TreeRow::Dir { depth, name } => div()
            .h(px(ROW_H))
            .pl(indent(*depth))
            .flex()
            .items_center()
            .gap_1()
            .text_color(rgb(MUTED))
            .child("▾")
            .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(name.clone())))
            .into_any_element(),
        TreeRow::File { depth, name, dir, index } => {
            let change = files.get(*index)?;
            let index = *index;
            let is_selected = selected == Some(index);
            div()
                .id(("file", ix))
                .h(px(ROW_H))
                .pl(indent(*depth))
                .pr_2()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .when(is_selected, |row| row.bg(rgb(ui::SELECTED)))
                .hover(|style| style.bg(rgb(if is_selected { ui::SELECTED } else { ui::HOVER })))
                .on_click(cx.listener(move |this, _, _, cx| this.open_file(index, cx)))
                .child(
                    div()
                        .flex_none()
                        .text_color(status_color(change.status))
                        .child(SharedString::from(name.clone())),
                )
                .when(!dir.is_empty(), |row| {
                    row.child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(SharedString::from(dir.clone())),
                    )
                })
                .when(dir.is_empty(), |row| row.child(div().flex_1()))
                .when_some(change.old_path.clone(), |row, old| {
                    row.child(div().text_xs().text_color(rgb(MUTED)).child(format!("← {old}")))
                })
                .child(stats(change))
                .into_any_element()
        }
    })
}

/// A hairline between panes.
#[allow(dead_code)]
pub fn divider() -> impl IntoElement {
    div().w(px(1.)).h_full().flex_none().bg(rgb(BORDER))
}
