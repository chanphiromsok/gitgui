//! The file pane on the right: a header, the commit's changed files (tree or flat, with a filter),
//! and beside them either the commit overview or the diff of the open file. It can fill the whole
//! area ("full view") and close.

use gitgui_core::Layout;
use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px, rgb, uniform_list};

use crate::changes::WORKTREE;
use crate::detail::file_row;
use crate::ui::{MONO, button};
use crate::workspace::{Phase, Workspace};
use crate::theme::t;

const FILES_WIDTH: f32 = 230.0;

impl Workspace {
    /// `width` is used beside the graph; in full view the pane fills the area instead.
    pub fn render_pane(&mut self, window: &mut Window, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let Some(commit) = repo.commit.as_ref() else { return div().into_any_element() };
        let expanded = repo.expanded;
        // The working tree's files are listed in the sidebar, under the project.
        let work = commit.id == WORKTREE;
        let files_visible = repo.files_visible && !work;

        let (short, summary) = match (&repo.phase, repo.selected) {
            _ if work => (SharedString::default(), SharedString::from(format!("Uncommitted changes · {} files", repo.work.len()))),
            (Phase::Ready(view), Some(ix)) => match view.entries.get(ix) {
                Some(entry) => (entry.short_id.clone(), entry.summary.clone()),
                None => Default::default(),
            },
            _ => Default::default(),
        };

        let header = div()
            .h(px(34.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(rgb(t().border))
            .when(!work, |header| {
                header.child(
                    button("toggle-files", "Files")
                        .when(files_visible, |b| b.bg(rgb(t().accent)).text_color(rgb(t().on_accent)))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_files_visible(cx))),
                )
            })
            .child(div().flex_none().font_family(MONO).text_xs().text_color(rgb(t().muted)).child(short))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(summary),
            )
            .child(
                button("expand", if expanded { "Collapse" } else { "Expand" })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_expanded(cx))),
            )
            .child(button("close-pane", "Close").on_click(cx.listener(|this, _, _, cx| this.close_pane(cx))));

        let body: AnyElement = match &commit.phase {
            Phase::Loading => text_panel("Loading commit…", t().muted),
            Phase::Failed(message) => text_panel(message.clone(), t().removed),
            Phase::Ready(_) => {
                let files = files_visible.then(|| self.render_files_column(cx));
                let content = if repo.file.is_some() {
                    self.render_diff(window, cx)
                } else if work {
                    text_panel("Pick a changed file under the project in the sidebar.", t().muted)
                } else {
                    match self.commit_view() {
                        Some(view) => self.render_info(&view.detail, cx),
                        None => div().into_any_element(),
                    }
                };
                div()
                    .size_full()
                    .flex()
                    .children(files)
                    .child(div().flex_1().min_w_0().h_full().child(content))
                    .into_any_element()
            }
        };

        let pane = div()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(t().bg))
            .child(header)
            .child(div().flex_1().min_h_0().child(body));
        if expanded {
            pane.flex_1().min_w_0().border_l_1().border_color(rgb(t().border)).into_any_element()
        } else {
            pane.flex_none().w(px(width)).into_any_element()
        }
    }

    fn render_files_column(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let layout = repo.layout;
        let rows = repo.file_rows.len();
        let (files, additions, deletions) = match self.commit_view() {
            Some(view) => (view.files.len(), view.additions, view.deletions),
            None => (0, 0, 0),
        };
        let shown_all = !repo.filter.trim().is_empty() && rows == 0;

        let toggle = |id: &'static str, label: &'static str, this: Layout| {
            button(id, label)
                .when(layout == this, |b| b.bg(rgb(t().accent)).text_color(rgb(t().on_accent)).font_weight(FontWeight::BOLD))
                .on_click(cx.listener(move |workspace, _, _, cx| workspace.set_layout(this, cx)))
        };

        div()
            .w(px(FILES_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(t().panel))
            .border_r_1()
            .border_color(rgb(t().border))
            .child(
                div()
                    .flex_none()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(t().border))
                    .child(div().flex().gap_1().child(toggle("layout-tree", "Tree", Layout::Tree)).child(toggle("layout-flat", "Flat", Layout::Flat)))
                    .child(self.filter_input.clone()),
            )
            .child(if shown_all {
                div().p_3().text_xs().text_color(rgb(t().muted)).child("No file matches the filter.").into_any_element()
            } else {
                uniform_list(
                    "pane-files",
                    rows,
                    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                        let Some(repo) = this.repo.as_ref() else { return Vec::new() };
                        let Some(view) = this.commit_view() else { return Vec::new() };
                        let selected = repo.file.as_ref().map(|file| file.index);
                        range
                            .filter_map(|ix| file_row(ix, repo.file_rows.get(ix)?, &view.files, selected, &repo.folded_dirs, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .into_any_element()
            })
            .child(
                div()
                    .h(px(26.))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(t().border))
                    .text_xs()
                    .child(div().text_color(rgb(t().muted)).child(format!("{files} file{}", if files == 1 { "" } else { "s" })))
                    .child(div().text_color(rgb(t().added)).child(format!("+{additions}")))
                    .child(div().text_color(rgb(t().removed)).child(format!("-{deletions}"))),
            )
            .into_any_element()
    }
}

fn text_panel(text: impl Into<SharedString>, color: u32) -> AnyElement {
    div().size_full().flex().items_center().justify_center().text_color(rgb(color)).child(text.into()).into_any_element()
}
