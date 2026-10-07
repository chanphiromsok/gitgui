//! The file diff: a toolbar, the commit's files on the left, and the diff on the right, unified or
//! side by side. Comments sit under their lines; a "+" appears on hover to start one.

use std::time::{SystemTime, UNIX_EPOCH};

use gitgui_core::{DiffLine, FileStatus, LineKind};
use gitgui_store::{Comment, Side};
use gpui::{
    AnyElement, Context, ElementId, FontWeight, ListOffset, MouseButton, SharedString, StyledText, Window, canvas, div, list,
    prelude::*, px, relative, rgb, rgba,
};

use crate::changes::WORKTREE;
use crate::detail::{deleted_tag, stats, status_color};
use crate::icons;
use crate::rows::{Anchor, DisplayRow, Mode, Notice, anchor_of};
use crate::preview::{self, Images, Preview};
use crate::minimap::MarkKind;
use crate::syntax::{FileColors, Span};
use crate::ui::{self, MONO, button};
use crate::workspace::{Phase, WHOLE_FILE, Workspace};
use crate::theme::t;

pub const LINE_H: f32 = 20.0;
/// How wide one character of the code font is at the diff's text size; a little over, so the longest
/// line is never cut off.
const CHAR_W: f32 = 7.4;
const MINIMAP_W: f32 = 14.;
/// A comment stays readable however wide the code beside it is.
const CARD_MAX_W: f32 = 760.;

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
                .when(mode == this, |b| b.bg(rgb(t().accent)).text_color(rgb(t().on_accent)).font_weight(FontWeight::BOLD))
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
            .border_color(rgb(t().border))
            .child(button("overview", "Overview").on_click(cx.listener(|this, _, _, cx| this.close_file(cx))))
            .child(ui::file_icon(icons::file(change.path.rsplit('/').next().unwrap_or(&change.path))))
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(status_color(change.status)).child(SharedString::from(title)))
            .when(change.status == FileStatus::Deleted, |bar| bar.child(deleted_tag()))
            .child(stats(change))
            .child(
                div().min_w_0().flex_1().text_xs().text_color(rgb(t().muted)).child(match count {
                    0 => "Click + beside a line to comment".to_owned(),
                    1 => "1 comment on this file".to_owned(),
                    n => format!("{n} comments on this file"),
                }),
            )
            .when(!file.diff.binary && !file.diff.hunks.is_empty(), |bar| {
                bar.child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            button("more-context", if file.context >= WHOLE_FILE { "Whole file" } else { "Show more lines" })
                                .debug_selector(|| "more-context".to_owned())
                                .on_click(cx.listener(|this, _, _, cx| this.more_context(cx))),
                        )
                        .when(file.context != 3, |group| {
                            group.child(
                                button("less-context", "Collapse")
                                    .debug_selector(|| "less-context".to_owned())
                                    .on_click(cx.listener(|this, _, _, cx| this.set_diff_context(None, cx))),
                            )
                        }),
                )
            })
            .child(div().flex().gap_1().child(toggle("mode-unified", "Unified", Mode::Unified)).child(toggle("mode-split", "Split", Mode::Split)));

        let body: AnyElement = match &file.phase {
            Phase::Loading => centered_text("Loading diff…", t().muted),
            Phase::Failed(message) => centered_text(message.clone(), t().removed),
            Phase::Ready(()) => {
                let diff = self.diff_body(file, mode, cx);
                match &file.images {
                    // A picture: before and after, then (for an SVG) its text diff below.
                    Some(images) => div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(image_compare(images))
                        .when(!file.diff.binary && !file.diff.hunks.is_empty(), |col| {
                            col.child(div().flex_1().min_h_0().border_t_1().border_color(rgb(t().border)).child(diff))
                        })
                        .into_any_element(),
                    None => diff,
                }
            }
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(div().flex_1().min_h_0().bg(rgb(t().editor_bg)).child(body))
            .into_any_element()
    }

    /// The rows, wide enough for the longest line and scrolling sideways when that is wider than the
    /// pane, with the minimap over the right edge.
    fn diff_body(&self, file: &crate::workspace::FileState, mode: Mode, cx: &mut Context<Self>) -> AnyElement {
        // Gutters and the "+" cell, then the code. In split view each side has its own.
        let side = |gutter_cols: f32| 18. + (gutter_cols + 3. + file.max_cols as f32) * CHAR_W + 24.;
        let width = match mode {
            Mode::Unified => side(11.),
            Mode::Split => 2. * side(5.),
        };
        let rows = list(file.list.clone(), cx.processor(|this, ix: usize, _window, cx| this.render_diff_row(ix, cx))).size_full();
        let scrolling = div()
            .id("diff-x")
            .size_full()
            .overflow_x_scroll()
            .child(div().debug_selector(|| "diff-list".to_owned()).h_full().w(px(width)).min_w_full().child(rows));
        div().size_full().relative().child(scrolling).child(self.minimap(file, cx)).into_any_element()
    }

    /// A strip down the right edge: a tick for every run of changed rows and a box for what is in view.
    /// Click or drag on it to go there.
    fn minimap(&self, file: &crate::workspace::FileState, cx: &mut Context<Self>) -> AnyElement {
        let map = &file.minimap;
        let total = map.total().max(1.);
        let marks = map.marks.clone();
        // What is in view, from the list's real pixel positions.
        let viewport = file.list.viewport_bounds().size.height;
        let content = (file.list.max_offset_for_scrollbar().height + viewport).max(px(1.));
        let top = (-file.list.scroll_px_offset_for_scrollbar().y / content).clamp(0., 1.);
        let height = (viewport / content).clamp(0., 1.);

        let bounds = file.minimap_bounds.clone();
        div()
            .id("minimap")
            .debug_selector(|| "minimap".to_owned())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(MINIMAP_W))
            .bg(rgba(0x00000030))
            .border_l_1()
            .border_color(rgb(t().border))
            .cursor_pointer()
            .occlude()
            // One layer paints every tick: a diff with thousands of changed runs must not be thousands of elements.
            .child(
                canvas(
                    move |b, _, _| bounds.set(b),
                    move |b, _, window, _| {
                        let theme = t();
                        // Ticks smaller than a few pixels are merged first: every quad costs the same however
                        // small, and a diff with thousands of changes would otherwise paint thousands.
                        let cell = px(3.);
                        let cells = ((b.size.height / cell) as usize).clamp(1, 600);
                        let mut filled: Vec<Option<MarkKind>> = vec![None; cells];
                        for mark in marks.iter() {
                            let from = ((mark.top / total) * cells as f32) as usize;
                            let to = (((mark.top + mark.height) / total) * cells as f32).ceil() as usize;
                            for slot in &mut filled[from.min(cells - 1)..to.clamp(from.min(cells - 1) + 1, cells)] {
                                // A comment shows over a change, and a change over its neighbor in the same cell.
                                if *slot != Some(MarkKind::Comment) {
                                    *slot = Some(mark.kind);
                                }
                            }
                        }
                        let step = b.size.height / cells as f32;
                        let mut at = 0;
                        while at < cells {
                            let Some(kind) = filled[at] else {
                                at += 1;
                                continue;
                            };
                            let run = filled[at..].iter().take_while(|slot| **slot == Some(kind)).count();
                            let color = match kind {
                                MarkKind::Added => theme.added,
                                MarkKind::Removed => theme.removed,
                                MarkKind::Changed => theme.modified,
                                MarkKind::Comment => theme.accent,
                            };
                            let tick = gpui::Bounds::new(
                                gpui::point(b.origin.x + px(3.), b.origin.y + step * at as f32),
                                gpui::size(b.size.width - px(6.), (step * run as f32).max(px(2.))),
                            );
                            window.paint_quad(gpui::fill(tick, rgb(color)));
                            at += run;
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(relative(top))
                    .h(relative(height))
                    .min_h(px(10.))
                    .bg(rgba(0xffffff1c))
                    .border_y_1()
                    .border_color(rgba(0xffffff44)),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| this.minimap_jump(event.position.y, cx)))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if event.dragging() {
                    this.minimap_jump(event.position.y, cx);
                }
            }))
            .into_any_element()
    }

    /// Scrolls the diff so the row at height `y` (a window position over the minimap) is mid-screen.
    fn minimap_jump(&mut self, y: gpui::Pixels, cx: &mut Context<Self>) {
        let Some(file) = self.repo.as_ref().and_then(|repo| repo.file.as_ref()) else { return };
        let bounds = file.minimap_bounds.get();
        if bounds.size.height <= px(0.) {
            return;
        }
        let fraction = (y - bounds.origin.y) / bounds.size.height;
        let target = file.minimap.row_at(fraction);
        let visible = (file.list.viewport_bounds().size.height / px(LINE_H)) as usize;
        file.list.scroll_to(ListOffset { item_ix: target.saturating_sub(visible / 2), offset_in_item: px(0.) });
        cx.notify();
    }

    fn render_diff_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(repo) = self.repo.as_ref() else { return div().into_any_element() };
        let Some(file) = repo.file.as_ref() else { return div().into_any_element() };
        let Some(row) = file.rows.get(ix).copied() else { return div().into_any_element() };
        let mode = repo.mode;
        // Uncommitted lines have no commit to hang a comment on.
        let comments = repo.commit.as_ref().is_some_and(|commit| commit.id != WORKTREE);

        match row {
            DisplayRow::Hunk(h) => hunk_header(h, &file.diff.hunks[h].header, file.context < WHOLE_FILE, cx),
            DisplayRow::Line { hunk, line } => {
                let line = &file.diff.hunks[hunk].lines[line];
                unified_line(ix, line, file.colors.of(line), comments, cx)
            }
            DisplayRow::Pair { hunk, left, right } => {
                let lines = &file.diff.hunks[hunk].lines;
                split_row(ix, left.map(|l| &lines[l]), right.map(|r| &lines[r]), &file.colors, comments, cx)
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
                    .max_w(px(CARD_MAX_W))
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(t().accent))
                    .bg(rgb(t().card))
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(t().muted))
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
                                    .bg(rgb(t().accent))
                                    .text_color(rgb(t().on_accent))
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

/// The image before and after, side by side, each with its size in pixels and bytes.
fn image_compare(images: &Images) -> AnyElement {
    let side = |label: &'static str, preview: Option<&Preview>, change: Option<String>| {
        let caption = match preview {
            Some(p) => format!("{} × {} px · {}", p.width, p.height, preview::human_size(p.bytes)),
            None => "not there".to_owned(),
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(rgb(t().text_strong)).child(label))
                    .child(div().text_color(rgb(t().muted)).child(caption))
                    .children(change.map(|text| div().text_color(rgb(t().muted)).child(format!("({text})")))),
            )
            .child(
                div()
                    .h(px(320.))
                    .w_full()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(t().border))
                    .bg(rgb(t().empty_bg))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(match preview {
                        Some(p) => gpui::img(p.source.clone()).size_full().object_fit(gpui::ObjectFit::ScaleDown).into_any_element(),
                        None => div().text_xs().text_color(rgb(t().muted)).child("—").into_any_element(),
                    }),
            )
    };
    let change = images.old.as_ref().zip(images.new.as_ref()).map(|(old, new)| preview::size_change(old.bytes, new.bytes));
    let mut row = div().flex_none().p_3().flex().gap_3();
    match (&images.old, &images.new) {
        (Some(_), Some(_)) => {
            row = row.child(side("Before", images.old.as_ref(), None)).child(side("After", images.new.as_ref(), change))
        }
        (None, Some(_)) => row = row.child(side("Added", images.new.as_ref(), None)),
        (Some(_), None) => row = row.child(side("Deleted", images.old.as_ref(), None)),
        (None, None) => row = row.child(div().text_xs().text_color(rgb(t().muted)).child("This image could not be read.")),
    }
    row.into_any_element()
}

fn centered_text(text: impl Into<SharedString>, color: u32) -> AnyElement {
    div().size_full().flex().items_center().justify_center().text_color(rgb(color)).child(text.into()).into_any_element()
}

fn hunk_header(h: usize, header: &str, can_expand: bool, cx: &mut Context<Workspace>) -> AnyElement {
    div()
        .w_full()
        .h(px(LINE_H + 4.))
        .px_3()
        .flex()
        .items_center()
        .gap_3()
        .bg(rgb(t().hunk_bg))
        .text_color(rgb(t().hunk_fg))
        .font_family(MONO)
        .text_xs()
        .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().child(SharedString::from(header.to_owned())))
        .when(can_expand, |row| {
            // More unchanged lines around every change, like "expand" beside a hunk on GitHub.
            row.child(
                div()
                    .id(("hunk-more", h))
                    .flex_none()
                    .px_1p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(t().element_hover)))
                    .on_click(cx.listener(|this, _, _, cx| this.more_context(cx)))
                    .child("↕ Show more lines"),
            )
        })
        .into_any_element()
}

fn notice_row(notice: Notice) -> AnyElement {
    let (text, color) = match notice {
        Notice::Binary => ("Binary file: there is no text diff to show.".to_owned(), t().muted),
        Notice::NoChanges => ("No textual changes (an empty file, or only the file mode changed).".to_owned(), t().muted),
        Notice::Truncated => (format!("The diff was cut after {} lines.", gitgui_core::diff::MAX_LINES), t().warning),
        Notice::HiddenComments(n) => (
            format!("{n} comment{} on lines outside the changes shown here.", if n == 1 { " is" } else { "s are" }),
            t().warning,
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
            .text_color(rgb(t().accent))
            .font_weight(FontWeight::BOLD)
            .opacity(0.)
            .group_hover(group, |style| style.opacity(1.))
            .hover(|style| style.bg(rgb(t().accent)).text_color(rgb(t().on_accent)))
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
        LineKind::Added => Some(t().added_bg),
        LineKind::Removed => Some(t().removed_bg),
        LineKind::Context => None,
    }
}

fn marker(kind: LineKind) -> (&'static str, u32) {
    match kind {
        LineKind::Added => ("+", t().added),
        LineKind::Removed => ("-", t().removed),
        LineKind::Context => (" ", t().muted),
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
fn diff_text(gutter: &str, sign: &str, sign_color: u32, code: &str, spans: &[Span]) -> StyledText {
    let theme = t();
    let text = format!("{gutter} {sign} {}", code.replace('\t', "    "));
    let end = gutter.len();
    // Where code begins in the row's text, and where each tab before a byte pushed it along.
    let start = end + sign.len() + 2;
    let tabs: Vec<usize> = code.match_indices('\t').map(|(at, _)| at).collect();
    let at = |byte: usize| start + byte + 3 * tabs.partition_point(|&tab| tab < byte);
    let mut highlights = vec![(0..end, tint(theme.line_number)), (end + 1..end + 1 + sign.len(), tint(sign_color))];
    for (range, name) in spans {
        let (from, to) = (range.start as usize, (range.end as usize).min(code.len()));
        if from >= to || !code.is_char_boundary(from) || !code.is_char_boundary(to) {
            continue;
        }
        if let Some(style) = theme.syntax_style(*name) {
            highlights.push((at(from)..at(to), style));
        }
    }
    StyledText::new(text).with_highlights(highlights)
}

fn unified_line(ix: usize, line: &DiffLine, spans: &[Span], comments: bool, cx: &mut Context<Workspace>) -> AnyElement {
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
        .child(plus(("plus", ix), anchor_of(line).filter(|_| comments), "diff-line", cx))
        .text_color(rgb(t().editor_fg))
        .child(diff_text(&gutter, sign, sign_color, &line.text, spans))
        .into_any_element()
}

fn split_row(
    ix: usize,
    left: Option<&DiffLine>,
    right: Option<&DiffLine>,
    colors: &FileColors,
    comments: bool,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let half = |left_side: bool, line: Option<&DiffLine>, cx: &mut Context<Workspace>| -> AnyElement {
        let group = if left_side { "split-left" } else { "split-right" };
        let cell = div().h_full().flex_1().min_w_0().overflow_hidden().whitespace_nowrap();
        let Some(line) = line else {
            return cell.bg(rgb(t().empty_bg)).into_any_element();
        };
        let (sign, sign_color) = marker(line.kind);
        let shown = if left_side { line.old_no } else { line.new_no };
        cell.group(group)
            .flex()
            .items_center()
            .when_some(line_bg(line.kind), |cell, bg| cell.bg(rgb(bg)))
            .child(plus((if left_side { "plus-l" } else { "plus-r" }, ix), anchor_of(line).filter(|_| comments), group, cx))
            .child(diff_text(&column(shown, 5), sign, sign_color, &line.text, colors.side(line, left_side)))
            .into_any_element()
    };

    let left = half(true, left, cx);
    let right = half(false, right, cx);
    div()
        .w_full()
        .h(px(LINE_H))
        .flex()
        .font_family(MONO)
        .text_color(rgb(t().editor_fg))
        .text_xs()
        .child(left)
        .child(div().h_full().flex_1().min_w_0().border_l_1().border_color(rgb(t().border)).flex().child(right))
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
                .max_w(px(CARD_MAX_W))
                .rounded_md()
                .border_1()
                .border_color(rgb(t().border))
                .bg(rgb(t().card))
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
                        .text_color(rgb(t().muted))
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
        .hover(|style| style.bg(rgb(t().hover)).opacity(1.))
        .into_any_element()
}
