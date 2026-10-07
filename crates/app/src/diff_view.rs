//! The file diff: a toolbar, the commit's files on the left, and the diff on the right, unified or
//! side by side. Comments sit under their lines; a "+" appears on hover to start one.

use std::time::{SystemTime, UNIX_EPOCH};

use gitgui_core::{DiffLine, LineKind};
use gitgui_store::{Comment, Side};
use gpui::{AnyElement, Context, ElementId, FontWeight, SharedString, StyledText, Window, div, list, prelude::*, px, rgb};

use crate::detail::{stats, status_color};
use crate::rows::{Anchor, DisplayRow, Mode, Notice, anchor_of};
use crate::ui::{self, ACCENT, ADDED, BG, BORDER, HOVER, MONO, MUTED, REMOVED, WARNING, button};
use crate::workspace::{Phase, Workspace};

const LINE_H: f32 = 20.0;
const ADDED_BG: u32 = 0x1d3b2c;
const REMOVED_BG: u32 = 0x4b2326;
const EMPTY_BG: u32 = 0x232323;

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Old => "old",
        Side::New => "new",
    }
}

impl Workspace {
    pub fn render_diff(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let Some(file) = repo.file.as_ref() else { return div().into_any_element() };
        let Some(view) = self.commit_view() else { return div().into_any_element() };
        let Some(change) = view.files.get(file.index) else { return div().into_any_element() };

        let title = match &change.old_path {
            Some(old) => format!("{old} → {}", change.path),
            None => change.path.clone(),
        };
        let mode = repo.mode;
        let count = file.comments.len();

        let toggle = |id: &'static str, label: &'static str, this: Mode| {
            button(id, label)
                .when(mode == this, |b| b.bg(rgb(ACCENT)).text_color(rgb(0x111111)).font_weight(FontWeight::BOLD))
                .on_click(cx.listener(move |workspace, _, _, cx| workspace.set_mode(this, cx)))
        };

        let toolbar = div()
            .h(px(36.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(button("overview", "Overview").on_click(cx.listener(|this, _, _, cx| this.close_file(cx))))
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(status_color(change.status)).child(SharedString::from(title)))
            .child(stats(change))
            .child(
                div().min_w_0().flex_1().text_xs().text_color(rgb(MUTED)).child(match count {
                    0 => "Click + beside a line to comment".to_owned(),
                    1 => "1 comment on this file".to_owned(),
                    n => format!("{n} comments on this file"),
                }),
            )
            .child(div().flex().gap_1().child(toggle("mode-unified", "Unified", Mode::Unified)).child(toggle("mode-split", "Split", Mode::Split)));

        let body: AnyElement = match &file.phase {
            Phase::Loading => centered_text("Loading diff…", MUTED),
            Phase::Failed(message) => centered_text(message.clone(), REMOVED),
            Phase::Ready(()) => list(
                file.list.clone(),
                cx.processor(|this, ix: usize, _window, cx| this.render_diff_row(ix, cx)),
            )
            .size_full()
            .into_any_element(),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(div().flex_1().min_h_0().bg(rgb(BG)).child(body))
            .into_any_element()
    }

    fn render_diff_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let Some(file) = repo.file.as_ref() else { return div().into_any_element() };
        let Some(row) = file.rows.get(ix).copied() else { return div().into_any_element() };
        let mode = repo.mode;

        match row {
            DisplayRow::Hunk(h) => hunk_header(&file.diff.hunks[h].header),
            DisplayRow::Line { hunk, line } => unified_line(ix, &file.diff.hunks[hunk].lines[line], cx),
            DisplayRow::Pair { hunk, left, right } => {
                let lines = &file.diff.hunks[hunk].lines;
                split_row(ix, left.map(|l| &lines[l]), right.map(|r| &lines[r]), cx)
            }
            DisplayRow::Comment(i) => match file.comments.get(i) {
                Some(comment) => comment_card(ix, comment, mode, cx),
                None => div().into_any_element(),
            },
            DisplayRow::Composer(anchor) => self.composer(anchor, mode, cx),
            DisplayRow::Notice(notice) => notice_row(notice),
        }
    }

    fn composer(&self, anchor: Anchor, mode: Mode, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .py_1()
            .pl(card_indent(mode))
            .pr_3()
            .child(
                div()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(ACCENT))
                    .bg(rgb(0x252a31))
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("Comment on {} line {}", side_name(anchor.side), anchor.line)),
                    )
                    .child(self.input.clone())
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(button("cancel-comment", "Cancel").on_click(cx.listener(|this, _, _, cx| this.cancel_comment(cx))))
                            .child(
                                button("save-comment", "Comment")
                                    .bg(rgb(ACCENT))
                                    .text_color(rgb(0x111111))
                                    .on_click(cx.listener(|this, _, _, cx| this.submit_comment(cx))),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// Where a comment card starts: under the code in unified view, past the gutters.
fn card_indent(mode: Mode) -> gpui::Pixels {
    match mode {
        Mode::Unified => px(120.),
        Mode::Split => px(24.),
    }
}

fn centered_text(text: impl Into<SharedString>, color: u32) -> AnyElement {
    div().size_full().flex().items_center().justify_center().text_color(rgb(color)).child(text.into()).into_any_element()
}

fn hunk_header(header: &str) -> AnyElement {
    div()
        .w_full()
        .h(px(LINE_H + 4.))
        .px_3()
        .flex()
        .items_center()
        .bg(rgb(0x1f2a36))
        .text_color(rgb(0x7aa2c8))
        .font_family(MONO)
        .text_xs()
        .child(SharedString::from(header.to_owned()))
        .into_any_element()
}

fn notice_row(notice: Notice) -> AnyElement {
    let (text, color) = match notice {
        Notice::Binary => ("Binary file: there is no text diff to show.".to_owned(), MUTED),
        Notice::NoChanges => ("No textual changes (an empty file, or only the file mode changed).".to_owned(), MUTED),
        Notice::Truncated => (format!("The diff was cut after {} lines.", gitgui_core::diff::MAX_LINES), WARNING),
        Notice::HiddenComments(n) => (
            format!("{n} comment{} on lines outside the changes shown here.", if n == 1 { " is" } else { "s are" }),
            WARNING,
        ),
    };
    div().w_full().py_2().px_3().text_xs().text_color(rgb(color)).child(text).into_any_element()
}

/// The "+" that appears when the pointer is over a row; clicking it starts a comment on `anchor`.
fn plus(id: impl Into<ElementId>, anchor: Option<Anchor>, group: &'static str, cx: &mut Context<Workspace>) -> AnyElement {
    let cell = div().id(id).w(px(18.)).h_full().flex_none().flex().items_center().justify_center();
    match anchor {
        Some(anchor) => cell
            .cursor_pointer()
            .text_color(rgb(ACCENT))
            .font_weight(FontWeight::BOLD)
            .opacity(0.)
            .group_hover(group, |style| style.opacity(1.))
            .hover(|style| style.bg(rgb(ACCENT)).text_color(rgb(0x111111)))
            .on_click(cx.listener(move |this, _, window, cx| this.start_comment(anchor, window, cx)))
            .child("+")
            .into_any_element(),
        None => cell.into_any_element(),
    }
}

/// The background of a changed line. Unchanged lines have none: painting one costs as much as any
/// other element, and there are far more of them.
fn line_bg(kind: LineKind) -> Option<u32> {
    match kind {
        LineKind::Added => Some(ADDED_BG),
        LineKind::Removed => Some(REMOVED_BG),
        LineKind::Context => None,
    }
}

fn marker(kind: LineKind) -> (&'static str, u32) {
    match kind {
        LineKind::Added => ("+", ADDED),
        LineKind::Removed => ("-", REMOVED),
        LineKind::Context => (" ", MUTED),
    }
}

/// A line number right-aligned in `width` columns, or blanks for a side that has none.
fn column(n: Option<u32>, width: usize) -> String {
    match n {
        Some(n) => format!("{n:>width$}"),
        None => " ".repeat(width),
    }
}

fn tint(color: u32) -> gpui::HighlightStyle {
    gpui::HighlightStyle { color: Some(rgb(color).into()), ..Default::default() }
}

/// One text element for a whole row: `gutter  sign  code`, with the gutter and sign colored. The font is
/// monospace, so padding the numbers lines the columns up, and a row costs one element, not six.
fn diff_text(gutter: &str, sign: &str, sign_color: u32, code: &str) -> StyledText {
    let text = format!("{gutter} {sign} {}", code.replace('\t', "    "));
    let end = gutter.len();
    StyledText::new(text).with_highlights([(0..end, tint(MUTED)), (end + 1..end + 2, tint(sign_color))])
}

fn unified_line(ix: usize, line: &DiffLine, cx: &mut Context<Workspace>) -> AnyElement {
    let (sign, sign_color) = marker(line.kind);
    let gutter = format!("{} {}", column(line.old_no, 5), column(line.new_no, 5));
    div()
        .id(("line", ix))
        .group("diff-line")
        .w_full()
        .h(px(LINE_H))
        .flex()
        .items_center()
        .overflow_hidden()
        .whitespace_nowrap()
        .when_some(line_bg(line.kind), |row, bg| row.bg(rgb(bg)))
        .font_family(MONO)
        .text_xs()
        .child(plus(("plus", ix), anchor_of(line), "diff-line", cx))
        .child(diff_text(&gutter, sign, sign_color, &line.text))
        .into_any_element()
}

fn split_row(ix: usize, left: Option<&DiffLine>, right: Option<&DiffLine>, cx: &mut Context<Workspace>) -> AnyElement {
    let half = |left_side: bool, line: Option<&DiffLine>, cx: &mut Context<Workspace>| -> AnyElement {
        let group = if left_side { "split-left" } else { "split-right" };
        let cell = div().h_full().flex_1().min_w_0().overflow_hidden().whitespace_nowrap();
        let Some(line) = line else {
            return cell.bg(rgb(EMPTY_BG)).into_any_element();
        };
        let (sign, sign_color) = marker(line.kind);
        let shown = if left_side { line.old_no } else { line.new_no };
        cell.group(group)
            .flex()
            .items_center()
            .when_some(line_bg(line.kind), |cell, bg| cell.bg(rgb(bg)))
            .child(plus((if left_side { "plus-l" } else { "plus-r" }, ix), anchor_of(line), group, cx))
            .child(diff_text(&column(shown, 5), sign, sign_color, &line.text))
            .into_any_element()
    };

    let left = half(true, left, cx);
    let right = half(false, right, cx);
    div()
        .w_full()
        .h(px(LINE_H))
        .flex()
        .font_family(MONO)
        .text_xs()
        .child(left)
        .child(div().h_full().flex_1().min_w_0().border_l_1().border_color(rgb(BORDER)).flex().child(right))
        .into_any_element()
}

fn comment_card(ix: usize, comment: &Comment, mode: Mode, cx: &mut Context<Workspace>) -> AnyElement {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (resolve_id, delete_id) = (comment.id.clone(), comment.id.clone());
    let resolved = comment.resolved;

    div()
        .w_full()
        .py_1()
        .pl(card_indent(mode))
        .pr_3()
        .child(
            div()
                .rounded_md()
                .border_1()
                .border_color(rgb(BORDER))
                .bg(rgb(0x252a31))
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(format!("{} line {}", side_name(comment.side), comment.line))
                        .child(ui::ago(now - comment.created))
                        .when(resolved, |row| row.child(div().px_1().rounded_sm().bg(rgb(0x23553a)).text_color(rgb(0xb7e4c7)).child("Resolved")))
                        .child(div().flex_1())
                        .child(
                            button(("resolve", ix), if resolved { "Reopen" } else { "Resolve" })
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle_resolved(&resolve_id, !resolved, cx))),
                        )
                        .child(button(("delete", ix), "Delete").on_click(cx.listener(move |this, _, _, cx| this.delete_comment(&delete_id, cx)))),
                )
                .child(div().when(resolved, |text| text.opacity(0.6)).child(SharedString::from(comment.text.clone()))),
        )
        .hover(|style| style.bg(rgb(HOVER)).opacity(1.))
        .into_any_element()
}
