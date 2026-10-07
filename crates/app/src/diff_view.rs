//! The file diff: a toolbar, the commit's files on the left, and the diff on the right, unified or
//! side by side. Comments sit under their lines; a "+" appears on hover to start one.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gitgui_core::{DiffLine, FileStatus, GitCli, LineKind};
use gitgui_store::{Comment, Side};
use gpui::{
    AnyElement, Context, ElementId, FontWeight, ListOffset, MouseButton, SharedString, StyledText, Window, canvas, div, list,
    prelude::*, px, relative, rgb, rgba,
};

use crate::changes::WORKTREE;
use crate::detail::{added_tag, deleted_tag, stats, status_color};
use crate::icons;
use crate::rows::{Anchor, DisplayRow, Mode, Notice, anchor_of};
use crate::preview::{self, Images, Preview};
use crate::minimap::MarkKind;
use crate::syntax::{FileColors, Span};
use crate::ui::{self, MONO, button, ghost, segment, segmented};
use crate::workspace::{BlameState, Phase, WHOLE_FILE, Workspace};
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

        // The file's name leads; its folder (and, for a rename, where it came from) follows, dimmer, and is
        // what gives way when there is no room.
        let (folder, name) = change.path.rsplit_once('/').map_or(("", change.path.as_str()), |(folder, name)| (folder, name));
        let place = match &change.old_path {
            Some(old) => format!("{folder}  ← {old}"),
            None => folder.to_owned(),
        };
        let mode = file.mode(repo.mode);
        let count = file.comments.len();

        let choice = |id: &'static str, label: &'static str, this: Mode| {
            segment(id, label, mode == this).on_click(cx.listener(move |workspace, _, _, cx| workspace.set_mode(this, cx)))
        };

        let toolbar = div()
            .h(px(32.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(rgb(t().border))
            .child(ghost("overview", "‹ Overview").on_click(cx.listener(|this, _, _, cx| this.close_file(cx))))
            .child(ui::file_icon(icons::file(change.path.rsplit('/').next().unwrap_or(&change.path))))
            .child(
                div()
                    .flex_none()
                    .max_w(px(360.))
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(status_color(change.status))
                    .child(SharedString::from(name.to_owned())),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .line_clamp(1)
                    .text_ellipsis()
                    .text_xs()
                    .text_color(rgb(t().muted))
                    .child(SharedString::from(place)),
            )
            .when(change.status == FileStatus::Added, |bar| bar.child(added_tag()))
            .when(change.status == FileStatus::Deleted, |bar| bar.child(deleted_tag()))
            .child(stats(change))
            .child(
                div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_xs().text_color(rgb(t().muted)).child(match count {
                    0 => String::new(),
                    1 => "1 comment".to_owned(),
                    n => format!("{n} comments"),
                }),
            )
            // A new or deleted file is shown whole already: there is nothing more to show.
            .when(!file.diff.binary && !file.diff.hunks.is_empty() && !file.single_column, |bar| {
                bar.child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            ghost("more-context", if file.context >= WHOLE_FILE { "Whole file" } else { "↕ More lines" })
                                .debug_selector(|| "more-context".to_owned())
                                .on_click(cx.listener(|this, _, _, cx| this.more_context(cx))),
                        )
                        .when(file.context != 3, |group| {
                            group.child(
                                ghost("less-context", "Collapse")
                                    .debug_selector(|| "less-context".to_owned())
                                    .on_click(cx.listener(|this, _, _, cx| this.set_diff_context(None, cx))),
                            )
                        }),
                )
            })
            .child(segmented(vec![choice("mode-unified", "Unified", Mode::Unified), choice("mode-split", "Split", Mode::Split)]));

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

    /// The rows, scrolling sideways when a line is longer than the pane, with the minimap over the right edge.
    fn diff_body(&self, file: &crate::workspace::FileState, mode: Mode, cx: &mut Context<Self>) -> AnyElement {
        // What the longest line needs beside the gutter, in each layout.
        let gutter_cols = match mode {
            Mode::Unified => 11.,
            Mode::Split => 5.,
        };
        file.content_w.set((gutter_cols + 3. + file.max_cols as f32) * CHAR_W + 24.);
        let rows = list(file.list.clone(), cx.processor(|this, ix: usize, _window, cx| this.render_diff_row(ix, cx))).size_full();
        let bounds = file.diff_bounds.clone();
        // Sideways scrolling is done here, not by the pane, so the two directions do not fight: a swipe that
        // is mostly sideways moves the code sideways only, and one that is mostly down only scrolls down.
        let scrolling = div()
            .id("diff-x")
            .size_full()
            .relative()
            .overflow_hidden()
            .child(canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().size_full())
            .child(div().debug_selector(|| "diff-list".to_owned()).size_full().child(rows))
            .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| this.scroll_diff_sideways(event, cx)));
        div().size_full().relative().child(scrolling).child(self.minimap(file, cx)).into_any_element()
    }

    /// A wheel or trackpad event over the code: sideways when it is mostly sideways (or shift is held).
    fn scroll_diff_sideways(&mut self, event: &gpui::ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(file) = self.repo.as_ref().and_then(|repo| repo.file.as_ref()) else { return };
        let delta = event.delta.pixel_delta(px(LINE_H));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        let sideways = event.modifiers.shift || dx.abs() > dy.abs();
        if !sideways {
            return;
        }
        // The list scrolls by the vertical part of any wheel event; take that back so the gesture stays level.
        if dy != 0. && !event.modifiers.shift {
            file.list.scroll_by(delta.y);
        }
        let amount = if event.modifiers.shift && dx == 0. { dy } else { dx };
        let mode = file.mode(self.repo.as_ref().map_or(Mode::Unified, |repo| repo.mode));
        let most = max_scroll_x(file, mode);
        file.scroll_x.set((file.scroll_x.get() - amount).clamp(0., most));
        cx.stop_propagation();
        cx.notify();
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
            .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                // The strip sits over the code, so a wheel over it scrolls the code as it would beside it.
                if let Some(file) = this.repo.as_ref().and_then(|repo| repo.file.as_ref()) {
                    file.list.scroll_by(-event.delta.pixel_delta(px(LINE_H)).y);
                    cx.notify();
                }
            }))
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
        let mode = file.mode(repo.mode);
        let sx = file.scroll_x.get().clamp(0., max_scroll_x(file, mode));
        // Uncommitted lines have no commit to hang a comment on.
        let comments = repo.commit.as_ref().is_some_and(|commit| commit.id != WORKTREE);
        // Who last changed the line the pointer is on, said beside that line only.
        let note = self.blame_note(ix);

        match row {
            DisplayRow::Hunk(h) => hunk_header(h, &file.diff.hunks[h].header, file.context < WHOLE_FILE && !file.single_column, cx),
            DisplayRow::Line { hunk, line } => {
                let line = &file.diff.hunks[hunk].lines[line];
                unified_line(ix, line, file.colors.of(line), comments, sx, note.as_ref(), cx)
            }
            DisplayRow::Pair { hunk, left, right } => {
                let lines = &file.diff.hunks[hunk].lines;
                split_row(ix, left.map(|l| &lines[l]), right.map(|r| &lines[r]), &file.colors, comments, sx, note.as_ref(), cx)
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

/// How far the code can be scrolled sideways: what the longest line needs, less the room there is for code
/// (beside the "+" and, in split view, in each half).
fn max_scroll_x(file: &crate::workspace::FileState, mode: Mode) -> f32 {
    let viewport = f32::from(file.diff_bounds.get().size.width);
    let room = match mode {
        Mode::Unified => viewport - 18. - MINIMAP_W,
        Mode::Split => viewport / 2. - 18.,
    };
    (file.content_w.get() - room).max(0.)
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
        .flex()
        .items_center()
        .bg(rgb(t().hunk_bg))
        .text_color(rgb(t().hunk_fg))
        .font_family(MONO)
        .text_xs()
        // Where the line numbers are: a button to show more of the unchanged lines around every change.
        .child(if can_expand {
            div()
                .id(("hunk-more", h))
                .debug_selector(move || format!("hunk-more-{h}"))
                .flex_none()
                .w(px(42.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(rgb(t().accent))
                .font_weight(FontWeight::BOLD)
                .hover(|style| style.bg(rgb(t().accent)).text_color(rgb(t().on_accent)))
                .on_click(cx.listener(|this, _, _, cx| this.more_context(cx)))
                .child("↕")
                .into_any_element()
        } else {
            div().flex_none().w(px(42.)).into_any_element()
        })
        .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().child(SharedString::from(header.to_owned())))
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

/// What to say beside a line: who last changed it, when, and why.
pub struct BlameNote {
    /// Which part of the row: 0 the whole of it, 1 its left half, 2 its right half.
    pub side: u8,
    pub text: SharedString,
    /// The commit to go to when the note is clicked; none for a line not committed yet or from this very commit.
    pub commit: Option<String>,
}

/// The note, drawn right after the line's text, where the eye already is (it is cut off with the row when the
/// line is too long to leave room).
fn blame_chip(ix: usize, side: u8, note: &BlameNote, cx: &mut Context<Workspace>) -> AnyElement {
    let commit = note.commit.clone();
    div()
        .id(("blame", ix * 3 + side as usize))
        .debug_selector(|| "blame-note".to_owned())
        .flex_none()
        .ml_4()
        .h(px(LINE_H - 4.))
        .px_2()
        .flex()
        .items_center()
        .rounded_sm()
        .border_1()
        .border_color(rgb(t().border))
        .bg(rgb(t().card))
        .text_color(rgb(t().muted))
        .text_xs()
        .italic()
        .when(commit.is_some(), |chip| chip.cursor_pointer().hover(|style| style.text_color(rgb(t().text_strong))))
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            if let Some(commit) = &commit {
                this.select_commit_id(commit, cx);
            }
        }))
        .child(note.text.clone())
        .into_any_element()
}

/// `text` cut to `most` characters, with an ellipsis when it was longer.
fn clip(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(most.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

impl Workspace {
    /// The pointer came onto a line of the diff, or left it. The first time one is pointed at, who wrote the file's
    /// lines is read.
    pub fn hover_diff_line(&mut self, row: usize, side: u8, on: bool, cx: &mut Context<Self>) {
        if on {
            if self.hover_line == Some((row, side)) {
                return;
            }
            self.hover_line = Some((row, side));
            self.ensure_blame(cx);
            cx.notify();
        } else if self.hover_line == Some((row, side)) {
            self.hover_line = None;
            cx.notify();
        }
    }

    /// Reads who last changed each line of the open file, once, in the background: the file as of the commit, for the
    /// lines it has, and as of the commit's parent, for the lines it removed.
    fn ensure_blame(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.as_mut() else { return };
        let Some(commit) = repo.commit.as_ref() else { return };
        let Phase::Ready(view) = &commit.phase else { return };
        let (id, project) = (commit.id.clone(), repo.project.path.clone());
        let Some(file) = repo.file.as_mut() else { return };
        if !matches!(file.blame, BlameState::NotAsked) {
            return;
        }
        let Some(change) = view.files.get(file.index).cloned() else { return };
        file.blame = BlameState::Loading;
        let index = file.index;
        let work = id == WORKTREE;
        let (read_project, read_id) = (project.clone(), id.clone());
        self.spawn_load(
            cx,
            move || {
                let git = GitCli::new(&read_project);
                let (now, before) = if work { (None, "HEAD".to_owned()) } else { (Some(read_id.as_str()), format!("{read_id}^1")) };
                let new = (change.status != FileStatus::Deleted).then(|| git.blame(now, &change.path).ok()).flatten().map(Arc::new);
                let old_path = change.old_path.as_deref().unwrap_or(&change.path);
                let old = (change.status != FileStatus::Added).then(|| git.blame(Some(&before), old_path).ok()).flatten().map(Arc::new);
                (new, old)
            },
            move |this, (new, old), cx| {
                let Some(repo) = this.repo.as_mut().filter(|repo| repo.project.path == project) else { return };
                if repo.commit.as_ref().map(|commit| commit.id.as_str()) != Some(id.as_str()) {
                    return;
                }
                let Some(file) = repo.file.as_mut().filter(|file| file.index == index) else { return };
                file.blame = if new.is_none() && old.is_none() { BlameState::Failed } else { BlameState::Ready(new, old) };
                cx.notify();
            },
        );
    }

    /// For the screenshot script: rests the pointer on the first line of the diff whose text contains `text`.
    pub(crate) fn script_hover(&mut self, text: &str, cx: &mut Context<Self>) {
        let found = self.repo.as_ref().and_then(|repo| repo.file.as_ref()).and_then(|file| {
            file.rows.iter().enumerate().find_map(|(ix, row)| {
                let lines = |hunk: usize| &file.diff.hunks[hunk].lines;
                match *row {
                    DisplayRow::Line { hunk, line } => lines(hunk)[line].text.contains(text).then_some((ix, 0u8)),
                    DisplayRow::Pair { hunk, left, right } => right
                        .filter(|&r| lines(hunk)[r].text.contains(text))
                        .map(|_| (ix, 2u8))
                        .or_else(|| left.filter(|&l| lines(hunk)[l].text.contains(text)).map(|_| (ix, 1u8))),
                    _ => None,
                }
            })
        });
        match found {
            Some((row, side)) => {
                if let Some(file) = self.repo.as_ref().and_then(|repo| repo.file.as_ref()) {
                    file.list.scroll_to_reveal_item(row);
                }
                self.hover_diff_line(row, side, true, cx)
            }
            None => {
                if let Some((row, side)) = self.hover_line {
                    self.hover_diff_line(row, side, false, cx);
                }
            }
        }
    }

    /// What to say beside the line the pointer is on, if it is on line `row`.
    pub(crate) fn blame_note(&self, row: usize) -> Option<BlameNote> {
        let (hovered, side) = self.hover_line?;
        if hovered != row {
            return None;
        }
        let repo = self.repo.as_ref()?;
        let file = repo.file.as_ref()?;
        let line = match *file.rows.get(row)? {
            DisplayRow::Line { hunk, line } => &file.diff.hunks[hunk].lines[line],
            DisplayRow::Pair { hunk, left, right } => &file.diff.hunks[hunk].lines[if side == 1 { left? } else { right? }],
            _ => return None,
        };
        let note = |text: String, commit: Option<String>| Some(BlameNote { side, text: text.into(), commit });
        match &file.blame {
            BlameState::NotAsked | BlameState::Loading => note("Reading who changed this…".to_owned(), None),
            BlameState::Failed => None,
            BlameState::Ready(new, old) => {
                // A removed line is in the parent's version of the file; every other line is in the commit's.
                let (blame, number) = if line.kind == LineKind::Removed { (old, line.old_no?) } else { (new, line.new_no?) };
                let info = blame.as_ref()?.line(number)?;
                if info.uncommitted() {
                    return note("Not committed yet".to_owned(), None);
                }
                if repo.commit.as_ref().is_some_and(|commit| commit.id == info.commit) {
                    return note("This change".to_owned(), None);
                }
                let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
                // A half of a split row is narrow: only who and when fit beside the line (click for the rest).
                let who = format!("{} · {}", info.author, ui::ago(now - info.time));
                let text = if side == 0 { format!("{who} · {}", clip(&info.summary, 56)) } else { who };
                note(text, Some(info.commit.clone()))
            }
        }
    }
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

fn unified_line(
    ix: usize,
    line: &DiffLine,
    spans: &[Span],
    comments: bool,
    sx: f32,
    note: Option<&BlameNote>,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let (sign, sign_color) = marker(line.kind);
    let gutter = format!("{} {}", column(line.old_no, 5), column(line.new_no, 5));
    div()
        .id(("line", ix))
        .group("diff-line")
        .relative()
        .on_hover(cx.listener(move |this, on: &bool, _, cx| this.hover_diff_line(ix, 0, *on, cx)))
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
        .child(div().flex_none().ml(px(-sx)).child(diff_text(&gutter, sign, sign_color, &line.text, spans)))
        .when_some(note, |row, note| row.child(blame_chip(ix, 0, note, cx)))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn split_row(
    ix: usize,
    left: Option<&DiffLine>,
    right: Option<&DiffLine>,
    colors: &FileColors,
    comments: bool,
    sx: f32,
    note: Option<&BlameNote>,
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
        let side = if left_side { 1u8 } else { 2 };
        cell.id(("half", ix * 3 + side as usize))
            .group(group)
            .relative()
            .on_hover(cx.listener(move |this, on: &bool, _, cx| this.hover_diff_line(ix, side, *on, cx)))
            .flex()
            .items_center()
            .when_some(line_bg(line.kind), |cell, bg| cell.bg(rgb(bg)))
            .child(plus((if left_side { "plus-l" } else { "plus-r" }, ix), anchor_of(line).filter(|_| comments), group, cx))
            .child(div().flex_none().ml(px(-sx)).child(diff_text(&column(shown, 5), sign, sign_color, &line.text, colors.side(line, left_side))))
            .when_some(note.filter(|note| note.side == side), |cell, note| cell.child(blame_chip(ix, side, note, cx)))
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
