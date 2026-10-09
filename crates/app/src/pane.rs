//! The file pane on the right: a header, the commit's changed files (tree or flat, with a filter),
//! and beside them either the commit overview or the diff of the open file. It can fill the whole
//! area ("full view") and close.

use gitgui_core::Layout;
use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px, rgb, uniform_list};

use crate::changes::WORKTREE;
use crate::detail::file_row;
use crate::ui::{MONO, ghost, segment, segmented, toggle};
use crate::workspace::{Panel, Phase, Splitter, Workspace};
use crate::theme::t;


impl Workspace {
    /// `width` is used beside the graph; in full view the pane fills the area instead.
    pub fn render_pane(&mut self, window: &mut Window, width: f32, below: Option<f32>, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let Some(commit) = repo.commit.as_ref() else { return div().into_any_element() };
        let expanded = repo.expanded;
        // Beside the graph unless the graph is hidden (by hand, or in full view): then the pane fills the area.
        let fill = expanded || self.graph_hidden;
        // The working tree's files are listed in the sidebar, under the project.
        let work = commit.id == WORKTREE;
        let files_visible = repo.files_visible && !work;

        let (short, summary) = match (&repo.phase, repo.selected) {
            _ if work => {
                let n = repo.work.len();
                (SharedString::default(), SharedString::from(format!("Uncommitted changes · {n} file{}", if n == 1 { "" } else { "s" })))
            }
            (Phase::Ready(view), Some(ix)) => match view.entries.get(ix) {
                Some(entry) => (entry.short_id.clone(), entry.summary.clone()),
                None => Default::default(),
            },
            _ => Default::default(),
        };

        // Which panels show: switches in one track, soft when on. (The accent color is for a single choice.)
        let mut panels = Vec::new();
        if !expanded {
            panels.push(
                toggle("toggle-graph", "Graph", !self.graph_hidden)
                    .debug_selector(|| "toggle-graph".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_graph_hidden(cx))),
            );
        }
        if !work {
            panels.push(
                toggle("toggle-files", "Files", files_visible)
                    .debug_selector(|| "toggle-files".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_files_visible(cx))),
            );
        }
        let header = div()
            .h(px(if below.is_some() { 32. } else { 38. }))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(t().border))
            // With the graph gone (hidden, or in full view) its header, and the sidebar button in it, is gone: keep
            // one here.
            .when(fill, |header| header.child(div().debug_selector(|| "pane-sidebar-button".to_owned()).child(self.sidebar_button(cx))))
            .children((!panels.is_empty()).then(|| segmented(panels)))
            .child(div().flex_none().font_family(MONO).text_xs().text_color(rgb(t().muted)).child(short))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(t().text_strong))
                    .child(summary),
            )
            .child(
                ghost("expand", if expanded { "Collapse  Esc".to_owned() } else { format!("Expand  {}", crate::ui::shortcut("E")) })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_expanded(cx))),
            )
            .child(ghost("close-pane", "Close").on_click(cx.listener(|this, _, _, cx| this.close_pane(cx))));

        let body: AnyElement = match &commit.phase {
            Phase::Loading => text_panel("Loading commit…", t().muted),
            Phase::Failed(message) => text_panel(message.clone(), t().removed),
            Phase::Ready(_) => {
                let files = files_visible.then(|| self.render_files_column(cx));
                let files_divider = files_visible.then(|| self.splitter("files-divider", Splitter::Files, cx));
                // Hidden, the file list can be reached by pointing at the left edge of the code.
                let files_hidden = !files_visible && !work && repo.file.is_some();
                let files_rail = files_hidden.then(|| self.rail("files-rail", Panel::Files, None, cx));
                let files_peek = (files_hidden && self.peek == Some(Panel::Files)).then(|| {
                    let content = self.render_files_column(cx);
                    self.peeking("files-peek", Panel::Files, 0., self.files_width, content, cx)
                });
                let content = if repo.file.is_some() {
                    self.render_diff(window, cx)
                } else if work {
                    self.render_work_overview(cx)
                } else {
                    match self.commit_view() {
                        Some(view) => self.render_info(&view.detail, cx),
                        None => div().into_any_element(),
                    }
                };
                div()
                    .size_full()
                    .relative()
                    .flex()
                    .children(files)
                    .children(files_divider)
                    .children(files_rail)
                    .child(div().flex_1().min_w_0().h_full().child(content))
                    .children(files_peek)
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
        if let Some(height) = below {
            // Under the graph, across the whole width, at the height it was given.
            pane.w_full().h(px(height)).flex_none().into_any_element()
        } else if fill {
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

        let choice = |id: &'static str, label: &'static str, this: Layout| {
            segment(id, label, layout == this).on_click(cx.listener(move |workspace, _, _, cx| workspace.set_layout(this, cx)))
        };

        let left = self.files_left.clone();
        div()
            .w(px(self.files_width))
            .flex_none()
            .h_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(t().panel))
            .child(gpui::canvas(move |bounds, _, _| left.set(f32::from(bounds.origin.x)), |_, _, _, _| {}).absolute().size_full())
            .child(
                // One row: the filter takes what room there is, the tree/flat choice sits beside it.
                div()
                    .w_full()
                    .flex_none()
                    .px_2()
                    .py_1p5()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(t().border))
                    // Sized outright: beside the choice, a `flex_1` input collapses to nothing. What it gets is the
                    // column less the row's padding, the gap and the (fixed-size) Tree/Flat choice.
                    .child(div().flex_none().w(px((self.files_width - 16. - 8. - 92.).max(60.))).child(self.filter_input.clone()))
                    .child(segmented(vec![
                        choice("layout-tree", "Tree", Layout::Tree).px_2(),
                        choice("layout-flat", "Flat", Layout::Flat).px_2(),
                    ])),
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
