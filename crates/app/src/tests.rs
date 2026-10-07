//! Drives the real workspace headlessly: loads a real git repository, walks the whole review flow, and
//! draws a frame after each step so a view that would crash on real data fails here.

use std::path::{Path, PathBuf};
use std::process::Command;

use gitgui_core::Layout;
use gitgui_store::{Side, Store};
use gpui::{
    AnyView, AvailableSpace, Entity, EntityInputHandler, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Point, TestAppContext, VisualTestContext, point, px, size,
};

use crate::rows::{Anchor, DisplayRow, Mode, Notice};
use crate::layout;
use crate::workspace::{Phase, Splitter, Workspace};

struct Fixture(PathBuf, std::cell::Cell<u64>);

impl Fixture {
    fn repo(&self) -> PathBuf {
        self.0.join("repo")
    }

    fn data(&self) -> PathBuf {
        self.0.join("data")
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(self.repo())
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Ada")
            .env("GIT_AUTHOR_EMAIL", "ada@example.com")
            .env("GIT_COMMITTER_NAME", "Ada")
            .env("GIT_COMMITTER_EMAIL", "ada@example.com")
            // Each command is a second later than the one before, so history has a clear date order.
            .env("GIT_AUTHOR_DATE", format!("{} +0000", 1_700_000_000 + self.1.get()))
            .env("GIT_COMMITTER_DATE", format!("{} +0000", 1_700_000_000 + self.1.get()))
            .status()
            .unwrap();
        self.1.set(self.1.get() + 1);
        assert!(status.success(), "git {args:?} failed");
    }

    fn write(&self, path: &str, text: &str) {
        let file = self.repo().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Two commits: `base`, then `change`, which edits a file, adds one, and removes one.
fn fixture(name: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("gitgui-app-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let fx = Fixture(root, std::cell::Cell::new(0));
    fx.git(&["init", "-q", "-b", "main"]);
    fx.write("src/a.rs", "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n");
    fx.write("README.md", "hello\n");
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "base"]);
    fx.write("src/a.rs", "one\nTWO\nthree\nfour\nfive\nsix\nseven\nEIGHT\nnine\n");
    fx.write("src/b/c.rs", "fn c() {}\n");
    fx.git(&["rm", "-q", "README.md"]);
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "change"]);
    fx
}

fn draw(cx: &mut VisualTestContext, ws: &Entity<Workspace>) {
    let view = AnyView::from(ws.clone());
    cx.draw(
        point(px(0.), px(0.)),
        size(AvailableSpace::Definite(px(1280.)), AvailableSpace::Definite(px(800.))),
        move |_, _| view,
    );
}

fn summaries(ws: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<String> {
    ws.read_with(cx, |ws, _| match ws.repo.as_ref().map(|repo| &repo.phase) {
        Some(Phase::Ready(view)) => view.entries.iter().map(|e| e.summary.to_string()).collect(),
        _ => Vec::new(),
    })
}

fn index_of(ws: &Entity<Workspace>, cx: &VisualTestContext, summary: &str) -> usize {
    summaries(ws, cx).iter().position(|s| s == summary).unwrap_or_else(|| panic!("no commit {summary:?}"))
}

fn file_names(ws: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<String> {
    ws.read_with(cx, |ws, _| {
        let Some(repo) = ws.repo.as_ref() else { return Vec::new() };
        let Some(view) = ws.commit_view() else { return Vec::new() };
        repo.file_rows
            .iter()
            .filter_map(|row| match row {
                gitgui_core::TreeRow::File { index, .. } => Some(view.files[*index].path.clone()),
                gitgui_core::TreeRow::Dir { .. } => None,
            })
            .collect()
    })
}

fn open_project(ws: &Entity<Workspace>, cx: &mut VisualTestContext, repo: &Path) {
    ws.update(cx, |ws, cx| ws.add_folder(repo, cx));
    cx.run_until_parked();
}

fn select(ws: &Entity<Workspace>, cx: &mut VisualTestContext, summary: &str) {
    let ix = index_of(ws, cx, summary);
    ws.update(cx, |ws, cx| ws.select_entry(ix, cx));
    cx.run_until_parked();
}

#[gpui::test]
async fn the_whole_review_flow(cx: &mut TestAppContext) {
    let fx = fixture("flow");
    let store = Store::at(fx.data());
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    draw(cx, &ws); // the empty state

    // Open a project: it loads, it shows in the sidebar, and it is remembered.
    open_project(&ws, cx, &fx.repo());
    assert_eq!(summaries(&ws, cx), ["change", "base"]);
    assert_eq!(ws.read_with(cx, |ws, _| ws.projects.len()), 1);
    assert_eq!(store.projects().unwrap().len(), 1);
    draw(cx, &ws);

    // Pick a commit: the pane loads its files.
    select(&ws, cx, "change");
    let mut names = file_names(&ws, cx);
    names.sort();
    assert_eq!(names, ["README.md", "src/a.rs", "src/b/c.rs"]);
    draw(cx, &ws);

    // Tree and flat, and the filter.
    ws.update(cx, |ws, cx| ws.set_layout(Layout::Flat, cx));
    assert_eq!(file_names(&ws, cx).len(), 3);
    ws.update(cx, |ws, cx| ws.set_filter("c.rs".into(), cx));
    assert_eq!(file_names(&ws, cx), ["src/b/c.rs"]);
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.set_filter("zzz".into(), cx));
    assert!(file_names(&ws, cx).is_empty());
    draw(cx, &ws); // "No file matches"
    ws.update(cx, |ws, cx| {
        ws.set_filter(String::new(), cx);
        ws.set_layout(Layout::Tree, cx);
    });
    assert_eq!(file_names(&ws, cx).len(), 3);

    // Open the edited file: a unified diff with two separate hunks.
    let a_rs = ws.read_with(cx, |ws, _| ws.commit_view().unwrap().files.iter().position(|f| f.path == "src/a.rs").unwrap());
    ws.update(cx, |ws, cx| ws.open_file(a_rs, cx));
    cx.run_until_parked();
    let hunks = ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().unwrap().diff.hunks.len());
    assert_eq!(hunks, 1, "the two edits are close enough to share a hunk with three lines of context");
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.set_mode(Mode::Split, cx));
    draw(cx, &ws);

    // Comment on the new side of line 2 (`TWO`).
    let anchor = Anchor { side: Side::New, line: 2 };
    cx.update(|window, app| ws.update(app, |ws, cx| ws.start_comment(anchor, window, cx)));
    assert!(ws.read_with(cx, |ws, _| {
        ws.repo.as_ref().unwrap().file.as_ref().unwrap().rows.iter().any(|r| matches!(r, DisplayRow::Composer(a) if *a == anchor))
    }));
    draw(cx, &ws); // the composer
    cx.update(|window, app| {
        let input = ws.read(app).input.clone();
        input.update(app, |input, cx| input.replace_text_in_range(None, "why upper case?", window, cx));
    });
    ws.update(cx, |ws, cx| ws.submit_comment(cx));

    let commit_id = ws.read_with(cx, |ws, _| ws.commit_view().unwrap().detail.id.clone());
    let saved = store.comments_on(&fx.repo(), &commit_id, "src/a.rs").unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!((saved[0].side, saved[0].line, saved[0].text.as_str()), (Side::New, 2, "why upper case?"));
    let (shown, composing) = ws.read_with(cx, |ws, _| {
        let file = ws.repo.as_ref().unwrap().file.as_ref().unwrap();
        (file.rows.iter().filter(|r| matches!(r, DisplayRow::Comment(_))).count(), file.composing)
    });
    assert_eq!((shown, composing), (1, None), "the comment shows and the composer closes");
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.set_mode(Mode::Unified, cx));
    draw(cx, &ws);

    // Resolve it, then delete it.
    ws.update(cx, |ws, cx| ws.toggle_resolved(&saved[0].id, true, cx));
    assert!(store.comments_on(&fx.repo(), &commit_id, "src/a.rs").unwrap()[0].resolved);
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.delete_comment(&saved[0].id, cx));
    assert!(store.comments_on(&fx.repo(), &commit_id, "src/a.rs").unwrap().is_empty());
    draw(cx, &ws);

    // A comment with no text is not saved.
    cx.update(|window, app| ws.update(app, |ws, cx| ws.start_comment(anchor, window, cx)));
    ws.update(cx, |ws, cx| ws.submit_comment(cx));
    assert!(store.comments(&fx.repo()).unwrap().is_empty());
    ws.update(cx, |ws, cx| ws.cancel_comment(cx));

    // Deleted file and added file open too.
    for path in ["README.md", "src/b/c.rs"] {
        let ix = ws.read_with(cx, |ws, _| ws.commit_view().unwrap().files.iter().position(|f| f.path == path).unwrap());
        ws.update(cx, |ws, cx| ws.open_file(ix, cx));
        cx.run_until_parked();
        draw(cx, &ws);
    }

    // Full view and back; Escape goes from full view to the pane, then to the overview.
    ws.update(cx, |ws, cx| ws.toggle_expanded(cx));
    assert!(ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().expanded));
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.back(cx));
    assert!(!ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().expanded));
    assert!(ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.is_some()));
    ws.update(cx, |ws, cx| ws.back(cx));
    assert!(ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.is_none()));
    draw(cx, &ws);

    // The parent link selects the parent in the graph.
    let parent = ws.read_with(cx, |ws, _| ws.commit_view().unwrap().detail.parents[0].clone());
    ws.update(cx, |ws, cx| ws.select_commit_id(&parent, cx));
    cx.run_until_parked();
    assert_eq!(ws.read_with(cx, |ws, _| ws.commit_view().unwrap().detail.id.clone()), parent);
    draw(cx, &ws);

    // Close the pane, refresh, remove the project.
    ws.update(cx, |ws, cx| ws.close_pane(cx));
    assert!(ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().commit.is_none()));
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.refresh(cx));
    cx.run_until_parked();
    assert_eq!(summaries(&ws, cx), ["change", "base"]);
    ws.update(cx, |ws, cx| ws.remove_project(&std::fs::canonicalize(fx.repo()).unwrap(), cx));
    assert!(ws.read_with(cx, |ws, _| ws.repo.is_none() && ws.projects.is_empty()));
    assert!(store.projects().unwrap().is_empty());
    draw(cx, &ws);
}

#[gpui::test]
async fn an_answer_for_a_commit_you_already_left_is_dropped(cx: &mut TestAppContext) {
    let fx = fixture("stale");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());

    let (first, second) = (index_of(&ws, cx, "change"), index_of(&ws, cx, "base"));
    // Both loads are in flight before either answer is applied.
    ws.update(cx, |ws, cx| ws.select_entry(first, cx));
    ws.update(cx, |ws, cx| ws.select_entry(second, cx));
    cx.run_until_parked();

    let message = ws.read_with(cx, |ws, _| ws.commit_view().unwrap().detail.message.clone());
    assert_eq!(message, "base", "the pane shows the commit picked last");
    draw(cx, &ws);
}

#[gpui::test]
async fn a_folder_that_is_not_a_repository_is_refused_with_a_message(cx: &mut TestAppContext) {
    let fx = fixture("plain");
    let plain = fx.0.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));

    ws.update(cx, |ws, cx| ws.add_folder(&plain, cx));
    cx.run_until_parked();

    assert!(ws.read_with(cx, |ws, _| ws.projects.is_empty() && ws.repo.is_none()));
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.text.contains("not inside a git repository"))));
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.dismiss_notice(cx));
    assert!(ws.read_with(cx, |ws, _| ws.notice.is_none()));
}

#[gpui::test]
async fn a_subfolder_adds_its_repository_and_a_second_add_changes_nothing(cx: &mut TestAppContext) {
    let fx = fixture("sub");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));

    open_project(&ws, cx, &fx.repo().join("src"));
    open_project(&ws, cx, &fx.repo());
    let projects = ws.read_with(cx, |ws, _| ws.projects.clone());
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].path, std::fs::canonicalize(fx.repo()).unwrap());
}

#[gpui::test]
async fn a_project_saved_last_time_is_there_on_the_next_launch(cx: &mut TestAppContext) {
    let fx = fixture("restart");
    Store::at(fx.data()).add_project(&fx.repo()).unwrap();

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    assert_eq!(ws.read_with(cx, |ws, _| ws.projects.len()), 1);
    let path = ws.read_with(cx, |ws, _| ws.projects[0].path.clone());
    ws.update(cx, |ws, cx| ws.select_project(path, cx));
    cx.run_until_parked();
    assert_eq!(summaries(&ws, cx), ["change", "base"]);
}

#[gpui::test]
async fn a_binary_file_shows_a_notice_not_lines(cx: &mut TestAppContext) {
    let fx = fixture("binary");
    std::fs::write(fx.repo().join("pic.bin"), [0u8, 159, 146, 150, 0, 1]).unwrap();
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "binary"]);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "binary");

    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    let rows = ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().unwrap().rows.clone());
    assert_eq!(rows, [DisplayRow::Notice(Notice::Binary)]);
    draw(cx, &ws);
}

fn drag_to(ws: &Entity<Workspace>, cx: &mut VisualTestContext, x: f32, button: Option<MouseButton>) {
    let event = MouseMoveEvent { position: point(px(x), px(200.)), pressed_button: button, modifiers: Modifiers::default() };
    cx.update(|window, app| ws.update(app, |ws, cx| ws.drag_divider(&event, window, cx)));
}

#[gpui::test]
async fn dragging_the_dividers_resizes_the_panes_within_their_limits(cx: &mut TestAppContext) {
    let fx = fixture("resize");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "change"); // opens the file pane
    draw(cx, &ws);
    let total = cx.update(|window, _| f32::from(window.viewport_size().width));
    let width = |cx: &VisualTestContext| ws.read_with(cx, |ws, _| (ws.sidebar_width, ws.pane_width));
    let sidebar_limit = layout::sidebar_width(f32::MAX, total, true);

    // Hold the sidebar's divider and move: the sidebar follows the pointer.
    ws.update(cx, |ws, cx| ws.begin_resize(Splitter::Sidebar, cx));
    drag_to(&ws, cx, 320., Some(MouseButton::Left));
    assert_eq!(width(cx).0, 320.);
    draw(cx, &ws);
    drag_to(&ws, cx, layout::SIDEBAR_HIDE_AT + 10., Some(MouseButton::Left));
    assert_eq!(width(cx).0, layout::SIDEBAR_MIN, "it stops at its minimum");
    // Further left, it hides; dragging back out shows it again.
    drag_to(&ws, cx, 5., Some(MouseButton::Left));
    assert!(!ws.read_with(cx, |ws, _| ws.sidebar_shown()), "dragged shut");
    draw(cx, &ws);
    drag_to(&ws, cx, total, Some(MouseButton::Left));
    assert!(ws.read_with(cx, |ws, _| ws.sidebar_shown()));
    assert_eq!(width(cx).0, sidebar_limit, "and at what the window leaves room for");
    draw(cx, &ws);

    // Letting go ends the drag: moving on does nothing.
    ws.update(cx, |ws, cx| ws.end_resize(cx));
    drag_to(&ws, cx, 250., Some(MouseButton::Left));
    assert_eq!(width(cx).0, sidebar_limit);

    // The pane's divider: the pane runs from the pointer to the right edge.
    assert_eq!(width(cx).1, None, "the pane has its default share until it is dragged");
    ws.update(cx, |ws, cx| ws.begin_resize(Splitter::Pane, cx));
    drag_to(&ws, cx, total - 700., Some(MouseButton::Left));
    assert_eq!(width(cx).1, Some(700.));
    draw(cx, &ws);
    drag_to(&ws, cx, total - 50., Some(MouseButton::Left));
    assert_eq!(width(cx).1, Some(layout::PANE_MIN));
    drag_to(&ws, cx, 0., Some(MouseButton::Left));
    let widest = width(cx).1.unwrap();
    assert!(widest >= layout::PANE_MIN && widest <= total - sidebar_limit - layout::GRAPH_MIN + 0.01);
    draw(cx, &ws);

    // A move with no button down means the release happened outside the window: the drag ends.
    drag_to(&ws, cx, total - 600., None);
    assert!(ws.read_with(cx, |ws, _| ws.resizing.is_none()));
    drag_to(&ws, cx, total - 500., Some(MouseButton::Left));
    assert_eq!(width(cx).1, Some(widest), "and the pane stays where it was left");

    // Full view hides the graph and its divider, and still draws.
    ws.update(cx, |ws, cx| ws.toggle_expanded(cx));
    draw(cx, &ws);
}

/// Milliseconds per frame: the best of several batches of `frames` draws. Other tests run at the same
/// time, so one batch can land on a busy machine; the best batch is the one that shows what the code costs.
fn frame_ms(ws: &Entity<Workspace>, cx: &mut VisualTestContext, frames: u32) -> f64 {
    draw(cx, ws); // the first frame measures rows and fills caches
    let batches = if std::env::var("GITGUI_BENCH_FRAMES").is_ok() { 1 } else { 6 };
    (0..batches)
        .map(|_| {
            let started = std::time::Instant::now();
            for _ in 0..frames {
                draw(cx, ws);
            }
            started.elapsed().as_secs_f64() * 1000. / f64::from(frames)
        })
        .fold(f64::MAX, f64::min)
}

#[gpui::test]
async fn a_very_large_diff_costs_no_more_per_frame_than_a_small_one(cx: &mut TestAppContext) {
    let fx = fixture("big");
    let base: String = (0..8000).map(|i| format!("line number {i}\n")).collect();
    let changed: String = (0..8000).map(|i| if i % 4 == 0 { format!("LINE NUMBER {i}\n") } else { format!("line number {i}\n") }).collect();
    fx.write("big.txt", &base);
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "big base"]);
    fx.write("big.txt", &changed);
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "big change"]);

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "big change");
    let ix = ws.read_with(cx, |ws, _| ws.commit_view().unwrap().files.iter().position(|f| f.path == "big.txt").unwrap());

    ws.update(cx, |ws, cx| ws.open_file(ix, cx));
    cx.run_until_parked();
    let rows = ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().unwrap().rows.len());
    assert!(rows > 10_000, "the fixture should make a big diff, got {rows} rows");

    // GITGUI_BENCH_FRAMES=3000 makes the test run long enough to attach a sampler to it.
    let frames: u32 = std::env::var("GITGUI_BENCH_FRAMES").ok().and_then(|n| n.parse().ok()).unwrap_or(30);
    let unified = frame_ms(&ws, cx, frames);
    ws.update(cx, |ws, cx| ws.set_mode(Mode::Split, cx));
    let split = frame_ms(&ws, cx, frames);
    eprintln!("big diff ({rows} rows): unified {unified:.2} ms/frame, split {split:.2} ms/frame");

    // Scrolled far down, the cost is the same: only the rows on screen are built.
    ws.read_with(cx, |ws, _| {
        let list = &ws.repo.as_ref().unwrap().file.as_ref().unwrap().list;
        list.scroll_to(gpui::ListOffset { item_ix: rows / 2, offset_in_item: px(0.) });
    });
    let scrolled = frame_ms(&ws, cx, 30);
    eprintln!("scrolled to the middle: {scrolled:.2} ms/frame");

    // Where the time goes: the diff alone (full view hides the graph), and the graph alone (no pane).
    ws.update(cx, |ws, cx| ws.set_mode(Mode::Unified, cx));
    ws.update(cx, |ws, cx| ws.toggle_expanded(cx));
    let diff_only = frame_ms(&ws, cx, 30);
    ws.update(cx, |ws, cx| ws.close_pane(cx));
    let graph_only = frame_ms(&ws, cx, 30);
    eprintln!("diff alone (unified, graph hidden): {diff_only:.2} ms/frame; graph alone: {graph_only:.2} ms/frame");

    // A frame must fit well inside 16 ms (60 per second). Debug builds are exempt: they are not what runs.
    if !cfg!(debug_assertions) {
        for (what, ms) in [("unified", unified), ("split", split), ("scrolled", scrolled)] {
            assert!(ms < 8., "{what} took {ms:.2} ms per frame");
        }
    }
}

// ---- grouping, icons, squash notes, menus ---------------------------------------------------------

use gitgui_core::{Backend, CheckoutTarget, CommitKind, GitCli, Label, LabelKind, Operation};

use crate::menu::{Action, MenuTarget, NoticeAction};

/// A bare repository with only the data folder; the repo is built by the caller.
fn bare_fixture(name: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("gitgui-app-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("repo")).unwrap();
    let fx = Fixture(root, std::cell::Cell::new(0));
    fx.git(&["init", "-q", "-b", "main"]);
    fx
}

fn commit_file(fx: &Fixture, file: &str, text: &str, message: &str) {
    fx.write(file, text);
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", message]);
}

/// base ← m1 on main, with the branch feat/x (x1, x2) merged by a pull-request merge commit.
fn merged_pr(name: &str) -> Fixture {
    let fx = bare_fixture(name);
    commit_file(&fx, "a.txt", "base\n", "base");
    fx.git(&["checkout", "-q", "-b", "feat/x"]);
    commit_file(&fx, "x.txt", "1\n", "feat: x1");
    commit_file(&fx, "x.txt", "2\n", "feat: x2");
    fx.git(&["checkout", "-q", "main"]);
    commit_file(&fx, "b.txt", "m1\n", "chore: m1");
    fx.git(&["merge", "-q", "--no-ff", "-m", "Merge pull request #1 from owner/feat/x", "feat/x"]);
    fx
}

/// base, then feat/x (x1, x2), then main moves (m1) and squash-merges feat/x as `feat: x (#7)`.
fn squashed_pr(name: &str) -> Fixture {
    let fx = bare_fixture(name);
    commit_file(&fx, "a.txt", "base\n", "base");
    fx.git(&["checkout", "-q", "-b", "feat/x"]);
    commit_file(&fx, "x.txt", "1\n", "feat: x1");
    commit_file(&fx, "y.txt", "2\n", "feat: x2");
    fx.git(&["checkout", "-q", "main"]);
    commit_file(&fx, "b.txt", "m1\n", "chore: m1");
    fx.git(&["merge", "-q", "--squash", "feat/x"]);
    fx.git(&["commit", "-q", "-m", "feat: x (#7)"]);
    fx
}

fn shown(ws: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<(String, usize)> {
    ws.read_with(cx, |ws, _| match ws.repo.as_ref().map(|r| &r.phase) {
        Some(Phase::Ready(view)) => view.entries.iter().map(|e| (e.summary.to_string(), e.depth)).collect(),
        _ => Vec::new(),
    })
}

fn with_window(ws: &Entity<Workspace>, cx: &mut VisualTestContext, f: impl FnOnce(&mut Workspace, &mut gpui::Window, &mut gpui::Context<Workspace>)) {
    cx.update(|window, app| ws.update(app, |ws, cx| f(ws, window, cx)));
}

fn label(ws: &Entity<Workspace>, cx: &VisualTestContext, name: &str) -> Label {
    ws.read_with(cx, |ws, _| match ws.repo.as_ref().map(|r| &r.phase) {
        Some(Phase::Ready(view)) => view.entries.iter().flat_map(|e| e.labels.clone()).find(|l| l.name == name),
        _ => None,
    })
    .unwrap_or_else(|| panic!("no badge named {name}"))
}

fn current_branch(fx: &Fixture) -> Option<String> {
    GitCli::new(fx.repo()).current_branch().unwrap()
}

fn menu_labels(ws: &Entity<Workspace>, cx: &VisualTestContext, target: MenuTarget) -> Vec<(String, bool)> {
    ws.read_with(cx, |ws, _| {
        ws.menu_items(&target).into_iter().filter(|i| i.action.is_some()).map(|i| (i.label.to_string(), i.enabled)).collect()
    })
}

#[gpui::test]
async fn a_pull_requests_commits_are_listed_under_it_with_icons_and_the_setting_turns_that_off(cx: &mut TestAppContext) {
    let fx = merged_pr("grouped");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);

    // Grouping is on by default: the branch's commits sit one level in, right under the merge.
    let on = [("Merge pull request #1 from owner/feat/x", 0), ("feat: x2", 1), ("feat: x1", 1), ("chore: m1", 0), ("base", 0)]
        .map(|(s, d)| (s.to_owned(), d));
    assert_eq!(shown(&ws, cx), on);

    // The icons tell a pull request from a plain commit.
    let kinds: Vec<CommitKind> = ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => view.entries.iter().map(|e| e.kind).collect(),
        _ => Vec::new(),
    });
    assert_eq!(kinds, [CommitKind::PullRequest, CommitKind::Commit, CommitKind::Commit, CommitKind::Commit, CommitKind::Commit]);

    // Turning it off lists them in date order again, and the choice is kept.
    ws.update(cx, |ws, cx| ws.toggle_group_by_parent(cx));
    let off = ["Merge pull request #1 from owner/feat/x", "chore: m1", "feat: x2", "feat: x1", "base"].map(|s| (s.to_owned(), 0));
    assert_eq!(shown(&ws, cx), off);
    assert!(!Store::at(fx.data()).settings().unwrap().group_by_parent);
    draw(cx, &ws);

    ws.update(cx, |ws, cx| ws.toggle_group_by_parent(cx));
    assert_eq!(shown(&ws, cx), on);
    assert!(Store::at(fx.data()).settings().unwrap().group_by_parent);
}

#[gpui::test]
async fn a_saved_choice_to_not_group_is_there_on_the_next_launch(cx: &mut TestAppContext) {
    let fx = merged_pr("launch");
    Store::at(fx.data()).save_settings(&gitgui_store::Settings { group_by_parent: false, ..Default::default() }).unwrap();
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    assert!(shown(&ws, cx).iter().all(|(_, depth)| *depth == 0));
}

#[gpui::test]
async fn folding_a_group_hides_its_commits_and_a_link_into_it_unfolds_it(cx: &mut TestAppContext) {
    let fx = merged_pr("fold");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let merge = ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => view.commits[0].id.clone(),
        _ => String::new(),
    });

    ws.update(cx, |ws, cx| ws.toggle_group(&merge, cx));
    let folded: Vec<String> = shown(&ws, cx).into_iter().map(|(s, _)| s).collect();
    assert_eq!(folded, ["Merge pull request #1 from owner/feat/x", "chore: m1", "base"]);
    draw(cx, &ws);
    let note = ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => (view.entries[0].collapsed, view.entries[0].group_size),
        _ => (false, 0),
    });
    assert_eq!(note, (true, 2), "the merge row says its two commits are folded");

    // Asking for a commit that is folded away unfolds the group to show it.
    let x1 = GitCli::new(fx.repo()).log(&gitgui_core::LogOptions::default()).unwrap().into_iter().find(|c| c.summary == "feat: x1").unwrap().id;
    ws.update(cx, |ws, cx| ws.select_commit_id(&x1, cx));
    cx.run_until_parked();
    assert_eq!(shown(&ws, cx).len(), 5);
    assert_eq!(ws.read_with(cx, |ws, _| ws.commit_view().unwrap().detail.message.clone()), "feat: x1");

    ws.update(cx, |ws, cx| ws.toggle_group(&merge, cx));
    ws.update(cx, |ws, cx| ws.toggle_group(&merge, cx));
    assert_eq!(shown(&ws, cx).len(), 5);
}

#[gpui::test]
async fn a_squash_merged_branch_is_noted_and_goes_under_its_squash_commit(cx: &mut TestAppContext) {
    let fx = squashed_pr("squash-ui");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo()); // the scan finishes in the background too
    draw(cx, &ws);

    // The branch's commits are nested under the commit that carries them.
    let expected = [("feat: x (#7)", 0), ("feat: x2", 1), ("feat: x1", 1), ("chore: m1", 0), ("base", 0)].map(|(s, d)| (s.to_owned(), d));
    assert_eq!(shown(&ws, cx), expected);

    let (clues, scanning, notes) = ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => {
            let notes: Vec<(usize, String)> =
                view.entries.iter().enumerate().flat_map(|(i, e)| e.notes.iter().map(move |n| (i, n.text.to_string()))).collect();
            (view.clues.clone(), view.scanning, notes)
        }
        _ => (Vec::new(), true, Vec::new()),
    });
    assert!(!scanning);
    assert_eq!(clues.len(), 1);
    assert_eq!((clues[0].evidence, clues[0].pr), (gitgui_core::Evidence::SamePatch, Some(7)));
    // On the squash commit: where it came from. On the branch tip: where it went.
    assert!(notes.iter().any(|(i, t)| *i == 0 && t.contains("squash of branch feat/x") && t.contains("#7")), "{notes:?}");
    assert!(notes.iter().any(|(i, t)| *i == 1 && t.contains("squash-merged into main")), "{notes:?}");
    // And the commit icon on the squash commit is a pull request.
    assert_eq!(ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => view.entries[0].kind,
        _ => CommitKind::Commit,
    }), CommitKind::PullRequest);

    // Turning grouping off keeps the notes but lists the commits flat.
    ws.update(cx, |ws, cx| ws.toggle_group_by_parent(cx));
    assert!(shown(&ws, cx).iter().all(|(_, d)| *d == 0));
}

#[gpui::test]
async fn right_clicking_a_branch_or_a_commit_offers_the_right_items(cx: &mut TestAppContext) {
    let fx = merged_pr("menu-items");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());

    // A branch that is not the current one: everything is available.
    let other = menu_labels(&ws, cx, MenuTarget::Label(label(&ws, cx, "feat/x")));
    assert_eq!(
        other,
        [
            ("Checkout Branch", true),
            ("Rename Branch…", true),
            ("Delete Branch…", true),
            ("Merge into Current Branch…", true),
            ("Rebase Current Branch onto Branch…", true),
            ("Push Branch…", true),
            ("Copy Branch Name", true),
        ]
        .map(|(l, e)| (l.to_owned(), e))
    );
    // The current branch cannot be checked out, deleted, merged into itself, or rebased onto itself.
    let current = menu_labels(&ws, cx, MenuTarget::Label(label(&ws, cx, "main")));
    let disabled: Vec<&str> = current.iter().filter(|(_, enabled)| !enabled).map(|(l, _)| l.as_str()).collect();
    assert_eq!(disabled, ["Checkout Branch", "Delete Branch…", "Merge into Current Branch…", "Rebase Current Branch onto Branch…"]);

    // A commit.
    let id = GitCli::new(fx.repo()).log(&gitgui_core::LogOptions::default()).unwrap()[1].id.clone();
    let commit = menu_labels(&ws, cx, MenuTarget::Commit(id));
    assert_eq!(commit.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), [
        "Checkout Commit (detached HEAD)", "Create Branch Here…", "Create Tag Here…", "Cherry-pick onto Current Branch…",
        "Copy Commit SHA", "Copy Commit Message",
    ]);

    // Opening the menu shows it; Escape closes it before anything else does.
    let main_badge = label(&ws, cx, "main");
    ws.update(cx, |ws, cx| ws.open_menu(point(px(300.), px(200.)), MenuTarget::Label(main_badge), cx));
    assert!(ws.read_with(cx, |ws, _| ws.menu.is_some()));
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.back(cx));
    assert!(ws.read_with(cx, |ws, _| ws.menu.is_none()));
}

#[gpui::test]
async fn checkout_from_the_menu_switches_branch_and_a_remote_badge_makes_a_local_one(cx: &mut TestAppContext) {
    let fx = merged_pr("menu-checkout");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    assert_eq!(current_branch(&fx).as_deref(), Some("main"));

    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::Checkout(CheckoutTarget::Branch("feat/x".into())), window, cx));
    assert!(ws.read_with(cx, |ws, _| ws.busy.is_some()), "it is running, and says so");
    cx.run_until_parked();
    assert_eq!(current_branch(&fx).as_deref(), Some("feat/x"));
    assert!(ws.read_with(cx, |ws, _| ws.busy.is_none() && ws.notice.as_ref().is_some_and(|n| !n.warn && n.text.contains("Checked out"))));
    // The graph was read again: the current-branch marker moved.
    assert_eq!(ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => view.current_branch.as_ref().map(ToString::to_string),
        _ => None,
    }).as_deref(), Some("feat/x"));
    draw(cx, &ws);

    // Git's refusal is shown as git said it, and nothing changes.
    fx.write("a.txt", "local edit\n");
    fx.git(&["stash", "-q"]); // keep the tree clean for the next step
    fx.git(&["checkout", "-q", "main"]);
    fx.write("b.txt", "uncommitted on main\n");
    fx.git(&["checkout", "-q", "-b", "other-b"]);
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "other-b work"]);
    fx.git(&["checkout", "-q", "main"]);
    fx.write("b.txt", "different uncommitted edit\n");
    ws.update(cx, |ws, cx| ws.refresh(cx));
    cx.run_until_parked();
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::Checkout(CheckoutTarget::Branch("other-b".into())), window, cx));
    cx.run_until_parked();
    assert_eq!(current_branch(&fx).as_deref(), Some("main"), "git refused to overwrite the edit");
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.warn)));
}

#[gpui::test]
async fn merge_asks_first_and_a_conflict_offers_to_put_everything_back(cx: &mut TestAppContext) {
    let fx = bare_fixture("menu-merge");
    commit_file(&fx, "app.txt", "one\ntwo\nthree\nfour\nfive\n", "base");
    fx.git(&["checkout", "-q", "-b", "other"]);
    commit_file(&fx, "app.txt", "one\ntwo\nTHREE other\nfour\nfive\n", "feat: three from other");
    fx.git(&["checkout", "-q", "main"]);
    commit_file(&fx, "app.txt", "one\ntwo\nTHREE main\nfour\nfive\n", "feat: three from main");

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());

    // Choosing Merge only asks; nothing has happened yet.
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::Merge("other".into()), window, cx));
    let (title, danger) = ws.read_with(cx, |ws, _| {
        let d = ws.dialog.as_ref().expect("a question");
        (d.title.to_string(), d.danger)
    });
    assert_eq!(title, "Merge other into main?");
    assert!(!danger);
    assert_eq!(GitCli::new(fx.repo()).in_progress(), None);
    draw(cx, &ws);

    // Cancelling leaves it alone.
    ws.update(cx, |ws, cx| ws.cancel_dialog(cx));
    assert!(ws.read_with(cx, |ws, _| ws.dialog.is_none()));
    assert_eq!(GitCli::new(fx.repo()).in_progress(), None);

    // Confirming runs it, and the conflict is offered back as an abort.
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::Merge("other".into()), window, cx));
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    assert_eq!(GitCli::new(fx.repo()).in_progress(), Some(Operation::Merge));
    let (text, action) = ws.read_with(cx, |ws, _| {
        let n = ws.notice.as_ref().unwrap();
        (n.text.to_string(), n.action.clone().map(|(_, a)| a))
    });
    assert!(text.contains("stopped") && text.contains("conflicts"), "{text}");
    assert_eq!(action, Some(NoticeAction::Abort(Operation::Merge)));
    draw(cx, &ws);

    ws.update(cx, |ws, cx| ws.run_notice_action(NoticeAction::Abort(Operation::Merge), cx));
    cx.run_until_parked();
    assert_eq!(GitCli::new(fx.repo()).in_progress(), None);
    assert_eq!(std::fs::read_to_string(fx.repo().join("app.txt")).unwrap(), "one\ntwo\nTHREE main\nfour\nfive\n");
}

#[gpui::test]
async fn a_merge_that_works_is_reported_and_the_graph_shows_it(cx: &mut TestAppContext) {
    let fx = squashed_pr("menu-merge-ok");
    fx.git(&["checkout", "-q", "-b", "side", "main~1"]);
    commit_file(&fx, "side.txt", "s\n", "feat: side work");
    fx.git(&["checkout", "-q", "main"]);

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::Merge("side".into()), window, cx));
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();

    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| !n.warn && n.text.contains("Merged side into main"))));
    assert!(fx.repo().join("side.txt").exists());
    assert!(shown(&ws, cx).iter().any(|(s, _)| s.starts_with("Merge branch 'side'")));
}

#[gpui::test]
async fn deleting_a_squash_merged_branch_says_so_and_goes_through_after_confirming(cx: &mut TestAppContext) {
    let fx = squashed_pr("menu-delete");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo()); // the scan has found the squash merge by now

    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::DeleteBranch("feat/x".into()), window, cx));
    let (body, danger) = ws.read_with(cx, |ws, _| {
        let d = ws.dialog.as_ref().unwrap();
        (d.body.to_string(), d.danger)
    });
    assert!(body.contains("squash-merged into `main`") && body.contains("forced delete"), "{body}");
    assert!(danger);
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();

    let branches = Command::new("git").arg("-C").arg(fx.repo()).args(["branch", "--list", "feat/x"]).output().unwrap();
    assert!(branches.stdout.is_empty(), "the branch is gone");
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.text.contains("Deleted feat/x"))));
}

#[gpui::test]
async fn an_unmerged_branch_is_only_deleted_after_a_second_yes(cx: &mut TestAppContext) {
    let fx = bare_fixture("menu-delete-unmerged");
    commit_file(&fx, "a.txt", "base\n", "base");
    fx.git(&["checkout", "-q", "-b", "wip"]);
    commit_file(&fx, "w.txt", "work\n", "wip: unmerged work that exists nowhere else");
    fx.git(&["checkout", "-q", "main"]);

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::DeleteBranch("wip".into()), window, cx));
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();

    let still_there = Command::new("git").arg("-C").arg(fx.repo()).args(["branch", "--list", "wip"]).output().unwrap();
    assert!(!still_there.stdout.is_empty(), "git refused, so the work is safe");
    let (text, action) = ws.read_with(cx, |ws, _| {
        let n = ws.notice.as_ref().unwrap();
        (n.text.to_string(), n.action.clone())
    });
    assert!(text.contains("not fully merged"), "{text}");
    let (label, action) = action.expect("an offer to delete anyway");
    assert_eq!(label.to_string(), "Delete anyway");

    ws.update(cx, |ws, cx| ws.run_notice_action(action, cx));
    cx.run_until_parked();
    let gone = Command::new("git").arg("-C").arg(fx.repo()).args(["branch", "--list", "wip"]).output().unwrap();
    assert!(gone.stdout.is_empty());
}

#[gpui::test]
async fn rename_and_new_branch_and_new_tag_take_a_typed_name(cx: &mut TestAppContext) {
    let fx = merged_pr("menu-names");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let type_name = |ws: &Entity<Workspace>, cx: &mut VisualTestContext, text: &str| {
        let text = text.to_owned();
        cx.update(|_, app| {
            let input = ws.read(app).dialog_input.clone();
            input.update(app, |input, cx| input.set_text(&text, cx));
        });
    };

    // Rename starts with the old name filled in; an empty name is not accepted.
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::RenameBranch("feat/x".into()), window, cx));
    assert_eq!(ws.read_with(cx, |ws, cx| ws.dialog_input.read(cx).text().to_owned()), "feat/x");
    draw(cx, &ws);
    type_name(&ws, cx, "");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    assert!(ws.read_with(cx, |ws, _| ws.dialog.is_some()), "the dialog stays open until there is a name");
    type_name(&ws, cx, "feat/y");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    let list = Command::new("git").arg("-C").arg(fx.repo()).args(["branch", "--format=%(refname:short)"]).output().unwrap();
    let names = String::from_utf8_lossy(&list.stdout).to_string();
    assert!(names.contains("feat/y") && !names.contains("feat/x"), "{names}");

    // A bad name is refused by git, with git's reason, and nothing is created.
    let id = GitCli::new(fx.repo()).log(&gitgui_core::LogOptions::default()).unwrap()[1].id.clone();
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::CreateBranch(id.clone()), window, cx));
    type_name(&ws, cx, "bad name");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.warn)));

    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::CreateBranch(id.clone()), window, cx));
    type_name(&ws, cx, "from-here");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::CreateTag(id.clone()), window, cx));
    type_name(&ws, cx, "v0.1");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    let at = |name: &str| String::from_utf8_lossy(&Command::new("git").arg("-C").arg(fx.repo()).args(["rev-parse", &format!("{name}^{{commit}}")]).output().unwrap().stdout).trim().to_owned();
    assert_eq!(at("from-here"), id);
    assert_eq!(at("v0.1"), id);
    assert!(ws.read_with(cx, |ws, _| !label_names(ws).is_empty()));
}

fn label_names(ws: &Workspace) -> Vec<String> {
    match ws.repo.as_ref().map(|r| &r.phase) {
        Some(Phase::Ready(view)) => view.entries.iter().flat_map(|e| e.labels.iter().filter(|l| l.kind == LabelKind::Tag).map(|l| l.name.clone())).collect(),
        _ => Vec::new(),
    }
}

#[gpui::test]
async fn copy_puts_the_name_on_the_clipboard_and_the_settings_panel_toggles_grouping(cx: &mut TestAppContext) {
    let fx = merged_pr("menu-copy");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());

    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::Copy { text: "feat/x".into(), what: "branch name" }, window, cx));
    let copied = cx.update(|_, app| app.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("feat/x"));
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.text.contains("Copied"))));

    // The settings panel opens, draws, flips the setting, and closes with Escape.
    ws.update(cx, |ws, cx| ws.open_settings(cx));
    draw(cx, &ws);
    assert!(ws.read_with(cx, |ws, _| ws.settings.group_by_parent));
    ws.update(cx, |ws, cx| ws.toggle_group_by_parent(cx));
    assert!(!ws.read_with(cx, |ws, _| ws.settings.group_by_parent));
    ws.update(cx, |ws, cx| ws.back(cx));
    assert!(!ws.read_with(cx, |ws, _| ws.settings_open));
}

#[gpui::test]
async fn only_one_git_operation_runs_at_a_time(cx: &mut TestAppContext) {
    let fx = merged_pr("menu-busy");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());

    with_window(&ws, cx, |ws, window, cx| {
        ws.choose(Action::Checkout(CheckoutTarget::Branch("feat/x".into())), window, cx);
        // A second click before the first finishes is turned away, not queued.
        ws.choose(Action::Checkout(CheckoutTarget::Detached("HEAD".into())), window, cx);
    });
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.warn && n.text.contains("still running"))));
    cx.run_until_parked();
    assert_eq!(current_branch(&fx).as_deref(), Some("feat/x"), "only the first one ran");
}

/// A repository with `main_commits` commits on main, and a two-commit pull request merged into it every
/// `every` commits, made in one go with `git fast-import`.
fn big_history(name: &str, main_commits: usize, every: usize) -> Fixture {
    use std::io::Write;
    let fx = bare_fixture(name);
    let mut stream = String::new();
    let mut mark = 0usize;
    let message = |stream: &mut String, text: &str| {
        stream.push_str(&format!("data {}\n{text}\n", text.len()));
    };
    let mut time = 1_700_000_000u64;
    let mut main_tip: Option<usize> = None;
    for i in 0..main_commits {
        if i > 0 && i % every == 0 {
            // A pull request: two commits off the current tip, then a merge commit on main.
            let base = main_tip.unwrap();
            let mut tip = base;
            for k in 0..2 {
                mark += 1;
                time += 1;
                stream.push_str(&format!("commit refs/heads/feat\nmark :{mark}\nauthor A <a@b.c> {time} +0000\ncommitter A <a@b.c> {time} +0000\n"));
                message(&mut stream, &format!("feat: part {k} of pull request {i}"));
                stream.push_str(&format!("from :{tip}\n\n"));
                tip = mark;
            }
            mark += 1;
            time += 1;
            stream.push_str(&format!("commit refs/heads/main\nmark :{mark}\nauthor A <a@b.c> {time} +0000\ncommitter A <a@b.c> {time} +0000\n"));
            message(&mut stream, &format!("Merge pull request #{i} from owner/feat"));
            stream.push_str(&format!("from :{base}\nmerge :{tip}\n\n"));
            main_tip = Some(mark);
            continue;
        }
        mark += 1;
        time += 1;
        stream.push_str(&format!("commit refs/heads/main\nmark :{mark}\nauthor A <a@b.c> {time} +0000\ncommitter A <a@b.c> {time} +0000\n"));
        message(&mut stream, &format!("chore: commit number {i}"));
        if let Some(parent) = main_tip {
            stream.push_str(&format!("from :{parent}\n"));
        }
        stream.push('\n');
        main_tip = Some(mark);
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(fx.repo())
        .args(["fast-import", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stream.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    fx.git(&["checkout", "-q", "-f", "main"]);
    fx
}

#[gpui::test]
async fn the_graph_of_a_large_repository_groups_and_draws_quickly(cx: &mut TestAppContext) {
    let fx = big_history("big-graph", 4000, 20);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    let started = std::time::Instant::now();
    open_project(&ws, cx, &fx.repo());
    let loaded = started.elapsed();

    let (commits, nested, folds) = ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => (
            view.entries.len(),
            view.entries.iter().filter(|e| e.depth > 0).count(),
            view.entries.iter().filter(|e| e.group_size > 0).count(),
        ),
        _ => (0, 0, 0),
    });
    // 4000 commits on main (199 of them pull request merges) plus two commits in each pull request.
    assert_eq!(commits, 4000 + 2 * 199);
    assert_eq!(folds, 199, "every pull request merge has a group under it");
    assert_eq!(nested, 2 * 199, "and each group holds the pull request's two commits");
    eprintln!("big graph: {commits} rows, loaded and laid out in {:.0} ms", loaded.as_secs_f64() * 1000.);

    let grouped = frame_ms(&ws, cx, 30);
    ws.update(cx, |ws, cx| ws.toggle_group_by_parent(cx));
    let flat = frame_ms(&ws, cx, 30);
    eprintln!("graph frame: grouped {grouped:.2} ms, flat {flat:.2} ms");
    if !cfg!(debug_assertions) {
        assert!(grouped < 8. && flat < 8., "grouped {grouped:.2} ms, flat {flat:.2} ms");
    }
}


// ---- real mouse events ----------------------------------------------------------------------------

/// The middle of a drawn element, found by the name its row or badge was given.
fn center_of(cx: &mut VisualTestContext, name: String) -> Point<gpui::Pixels> {
    let name: &'static str = Box::leak(name.into_boxed_str());
    cx.debug_bounds(name).unwrap_or_else(|| panic!("nothing drawn as {name}")).center()
}

fn click(cx: &mut VisualTestContext, button: MouseButton, at: Point<gpui::Pixels>) {
    cx.simulate_event(MouseDownEvent { button, position: at, modifiers: Modifiers::default(), click_count: 1, first_mouse: false });
    cx.simulate_event(MouseUpEvent { button, position: at, modifiers: Modifiers::default(), click_count: 1 });
}

fn pane_open(ws: &Entity<Workspace>, cx: &VisualTestContext) -> bool {
    ws.read_with(cx, |ws, _| ws.repo.as_ref().is_some_and(|r| r.commit.is_some() || r.selected.is_some()))
}

#[gpui::test]
async fn right_clicking_opens_a_menu_and_never_the_file_preview(cx: &mut TestAppContext) {
    let fx = merged_pr("right-click");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    cx.run_until_parked();
    assert!(!pane_open(&ws, cx));

    // Right-click a commit row: its menu opens, and nothing is selected or previewed.
    let row = center_of(cx, "row-3".into()); // chore: m1, a plain commit
    click(cx, MouseButton::Right, row);
    assert!(ws.read_with(cx, |ws, _| matches!(ws.menu.as_ref().map(|m| &m.target), Some(MenuTarget::Commit(_)))));
    assert!(!pane_open(&ws, cx), "a right-click must not open the file pane");
    cx.run_until_parked();

    // The menu opens at the pointer, so a click on that spot is on the menu itself and leaves it open.
    click(cx, MouseButton::Left, row);
    assert!(ws.read_with(cx, |ws, _| ws.menu.is_some()));
    // A left click anywhere else closes it, and does not fall through to the row beneath.
    let above = center_of(cx, "row-0".into());
    click(cx, MouseButton::Left, above);
    assert!(ws.read_with(cx, |ws, _| ws.menu.is_none()));
    assert!(!pane_open(&ws, cx), "closing the menu must not select the row that was clicked");
    cx.run_until_parked();

    // Right-click a branch badge: the branch menu opens, still no preview.
    let badge = center_of(cx, "badge-feat/x".into());
    click(cx, MouseButton::Right, badge);
    assert!(ws.read_with(cx, |ws, _| matches!(ws.menu.as_ref().map(|m| &m.target), Some(MenuTarget::Label(l)) if l.name == "feat/x")));
    assert!(!pane_open(&ws, cx), "nor does right-clicking a badge");
    ws.update(cx, |ws, cx| ws.back(cx));
    cx.run_until_parked();

    // Right-clicking the same row twice, or another row while a menu is open, still selects nothing.
    click(cx, MouseButton::Right, row);
    click(cx, MouseButton::Right, row);
    assert!(!pane_open(&ws, cx));
    ws.update(cx, |ws, cx| ws.back(cx));
    cx.run_until_parked();

    // A plain left click is what opens the preview.
    click(cx, MouseButton::Left, row);
    cx.run_until_parked();
    assert!(pane_open(&ws, cx), "a left click still opens the file pane");
}

#[gpui::test]
async fn the_filter_bar_narrows_the_graph_and_the_search_finds_commits(cx: &mut TestAppContext) {
    use gitgui_core::Scope;

    // main: base -> change. other (not checked out): one commit on base. feat (HEAD): one commit on change.
    let fx = fixture("filters");
    fx.git(&["checkout", "-q", "-b", "other", "HEAD~1"]);
    fx.write("other.txt", "other\n");
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "other work"]);
    fx.git(&["checkout", "-q", "-b", "feat", "main"]);
    fx.write("src/feat.rs", "fn feat() {}\n");
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "feat work"]);

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    assert_eq!(summaries(&ws, cx), ["feat work", "other work", "change", "base"]);
    draw(cx, &ws);

    // The current branch only: the other branch's commit goes; ahead and behind still count it.
    ws.update(cx, |ws, cx| ws.set_scope(Scope::Current, cx));
    assert_eq!(summaries(&ws, cx), ["feat work", "change", "base"]);
    let base = ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => view.base.clone(),
        _ => None,
    });
    assert_eq!(base.map(|b| (b.name, b.ahead, b.behind)), Some(("main".into(), 1, 0)));
    draw(cx, &ws);

    // Reading the repository again keeps the filter.
    ws.update(cx, |ws, cx| ws.refresh(cx));
    cx.run_until_parked();
    assert_eq!(summaries(&ws, cx), ["feat work", "change", "base"]);
    ws.update(cx, |ws, cx| ws.set_scope(Scope::All, cx));

    // A plain search dims what it misses; Enter goes to what it finds.
    let misses = |ws: &Entity<Workspace>, cx: &VisualTestContext| -> Vec<bool> {
        ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
            Phase::Ready(view) => view.entries.iter().map(|e| e.search_miss).collect(),
            _ => Vec::new(),
        })
    };
    ws.update(cx, |ws, cx| ws.set_search("work".into(), cx));
    assert_eq!(misses(&ws, cx), [false, false, true, true]);
    ws.update(cx, |ws, cx| ws.submit_search(cx));
    cx.run_until_parked();
    ws.update(cx, |ws, cx| ws.submit_search(cx));
    cx.run_until_parked();
    assert_eq!(ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().selected), Some(1), "the second match");
    draw(cx, &ws);

    // `path:` asks git once Enter is pressed.
    ws.update(cx, |ws, cx| ws.set_search("path:src/feat.rs".into(), cx));
    assert_eq!(misses(&ws, cx), [false; 4], "nothing dims before git answers");
    ws.update(cx, |ws, cx| ws.submit_search(cx));
    cx.run_until_parked();
    assert_eq!(misses(&ws, cx), [false, true, true, true]);
    draw(cx, &ws);
}

#[gpui::test]
async fn a_diff_is_colored_from_the_whole_file_on_both_sides(cx: &mut TestAppContext) {
    let fx = fixture("colors");
    fx.write("src/app.ts", "const a = 1;\nexport function f() {\n  return \"old\";\n}\n");
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "add app"]);
    fx.write("src/app.ts", "const a = 1;\nexport function f() {\n  // changed\n  return 2;\n}\n");
    fx.git(&["commit", "-q", "-am", "change app"]);

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "change app");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();

    let names = ws.read_with(cx, |ws, _| {
        let file = ws.repo.as_ref().unwrap().file.as_ref().unwrap();
        file.diff.hunks.iter().flat_map(|h| &h.lines).map(|line| {
            let spans = file.colors.of(line);
            (line.text.clone(), spans.iter().map(|(_, n)| crate::syntax::NAMES[*n as usize]).collect::<Vec<_>>())
        }).collect::<Vec<_>>()
    });
    let of = |text: &str| names.iter().find(|(t, _)| t == text).map(|(_, n)| n.clone()).unwrap_or_default();
    assert!(of("  return \"old\";").contains(&"string"), "the removed line, from the old file: {names:?}");
    assert_eq!(of("  // changed"), ["comment"], "the added line, from the new file");
    assert!(of("export function f() {").contains(&"keyword"));
    draw(cx, &ws);

    // Every theme draws the diff.
    let themes = ws.read_with(cx, |ws, _| ws.themes.iter().map(|t| t.name.clone()).collect::<Vec<_>>());
    for name in themes.iter().take(3) {
        ws.update(cx, |ws, cx| ws.set_theme(name, cx));
        draw(cx, &ws);
    }
    ws.update(cx, |ws, cx| ws.set_theme(crate::theme::DEFAULT, cx));
}

#[gpui::test]
async fn folders_fold_and_the_sidebar_hides_and_both_are_kept(cx: &mut TestAppContext) {
    let fx = fixture("dir-fold");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));

    // With nothing open, the sidebar cannot hide: it is where a project is picked.
    ws.update(cx, |ws, cx| ws.toggle_sidebar(cx));
    assert!(ws.read_with(cx, |ws, _| ws.sidebar_shown()));
    assert!(!Store::at(fx.data()).settings().unwrap().sidebar_hidden, "and nothing is saved");

    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "change");
    // src/a.rs, src/b/c.rs and README.md: src holds the other two.
    assert_eq!(file_names(&ws, cx), ["src/b/c.rs", "src/a.rs", "README.md"]);
    ws.update(cx, |ws, cx| ws.toggle_dir("src", cx));
    assert_eq!(file_names(&ws, cx), ["README.md"]);
    draw(cx, &ws);
    // Another commit keeps the folder folded; unfolding brings the files back.
    select(&ws, cx, "base");
    ws.update(cx, |ws, cx| ws.toggle_dir("src", cx));
    assert_eq!(file_names(&ws, cx), ["src/a.rs", "README.md"]);

    ws.update(cx, |ws, cx| ws.toggle_sidebar(cx));
    assert!(!ws.read_with(cx, |ws, _| ws.sidebar_shown()));
    assert!(Store::at(fx.data()).settings().unwrap().sidebar_hidden, "the choice is kept");
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.toggle_sidebar(cx));
    assert!(ws.read_with(cx, |ws, _| ws.sidebar_shown()));
}

#[gpui::test]
async fn a_changed_image_shows_before_and_after_with_its_sizes(cx: &mut TestAppContext) {
    let fx = fixture("image");
    let png = |w: u32, h: u32| {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(w, h, image::Rgba([2, 136, 209, 255])).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    };
    std::fs::write(fx.repo().join("logo.png"), png(4, 2)).unwrap();
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "add logo"]);
    std::fs::write(fx.repo().join("logo.png"), png(8, 6)).unwrap();
    fx.git(&["commit", "-q", "-am", "bigger logo"]);

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let sizes = |ws: &Entity<Workspace>, cx: &VisualTestContext| {
        ws.read_with(cx, |ws, _| {
            let images = ws.repo.as_ref()?.file.as_ref()?.images.clone()?;
            let dims = |p: Option<crate::preview::Preview>| p.map(|p| (p.width, p.height));
            Some((dims(images.old), dims(images.new)))
        })
    };

    select(&ws, cx, "bigger logo");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    assert_eq!(sizes(&ws, cx), Some((Some((4, 2)), Some((8, 6)))));
    draw(cx, &ws);

    select(&ws, cx, "add logo");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    assert_eq!(sizes(&ws, cx), Some((None, Some((4, 2)))), "an added image has no before");
    draw(cx, &ws);
}

#[gpui::test]
async fn the_chevron_on_a_merge_in_the_graph_folds_it_and_the_compact_graph_is_narrower(cx: &mut TestAppContext) {
    let fx = merged_pr("chevron");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);
    assert_eq!(shown(&ws, cx).len(), 5);

    // Clicking the chevron folds the merge's commits, and does not open the commit.
    let chevron = center_of(cx, "fold-0".into());
    click(cx, MouseButton::Left, chevron);
    cx.run_until_parked();
    assert_eq!(shown(&ws, cx).len(), 3);
    assert!(!pane_open(&ws, cx), "folding is not selecting");
    draw(cx, &ws);
    click(cx, MouseButton::Left, chevron);
    cx.run_until_parked();
    assert_eq!(shown(&ws, cx).len(), 5);

    let width = |ws: &Entity<Workspace>, cx: &VisualTestContext| {
        ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
            Phase::Ready(view) => view.graph_width,
            _ => 0.,
        })
    };
    let roomy = width(&ws, cx);
    ws.update(cx, |ws, cx| ws.toggle_compact_graph(cx));
    assert!(width(&ws, cx) < roomy, "narrower lanes");
    assert!(Store::at(fx.data()).settings().unwrap().compact_graph, "the choice is kept");
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.toggle_compact_graph(cx));
    assert_eq!(width(&ws, cx), roomy);
}

#[gpui::test]
async fn a_pull_request_merge_links_to_its_page_on_the_remote_site(cx: &mut TestAppContext) {
    let fx = merged_pr("pr-link");
    fx.git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);

    let (pr, merge) = ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => (
            view.entries[0].pr.clone().map(|(n, url)| (n, url.to_string())),
            view.entries[0].commit.clone().unwrap(),
        ),
        _ => (None, String::new()),
    });
    assert_eq!(pr, Some((1, "https://github.com/owner/repo/pull/1".to_owned())));

    let labels = ws.read_with(cx, |ws, _| {
        ws.menu_items(&crate::menu::MenuTarget::Commit(merge.clone())).into_iter().map(|item| item.label.to_string()).collect::<Vec<_>>()
    });
    assert_eq!(labels[..2], ["Open Pull Request #1 in Browser".to_owned(), "Open Commit in Browser".to_owned()]);
    // A plain commit has no pull request to open, only itself.
    let plain = ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => view.entries.iter().find(|e| e.summary.as_ref() == "chore: m1").and_then(|e| e.commit.clone()).unwrap(),
        _ => String::new(),
    });
    let labels = ws.read_with(cx, |ws, _| {
        ws.menu_items(&crate::menu::MenuTarget::Commit(plain.clone())).into_iter().map(|item| item.label.to_string()).collect::<Vec<_>>()
    });
    assert_eq!(labels[0], "Open Commit in Browser");
    select(&ws, cx, "Merge pull request #1 from owner/feat/x");
    draw(cx, &ws);
}

#[gpui::test]
async fn the_stashes_checkbox_hides_and_shows_stashes(cx: &mut TestAppContext) {
    let fx = fixture("stash-filter");
    fx.write("src/a.rs", "work in progress\n");
    fx.git(&["stash", "push", "-q", "-m", "wip"]);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let has_stash = |ws: &Entity<Workspace>, cx: &VisualTestContext| summaries(ws, cx).iter().any(|s| s.contains("wip"));
    assert!(has_stash(&ws, cx));
    assert_eq!(ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => view.stashes,
        _ => 0,
    }), 1);

    ws.update(cx, |ws, cx| ws.toggle_stashes(cx));
    assert!(!has_stash(&ws, cx));
    assert_eq!(summaries(&ws, cx), ["change", "base"]);
    draw(cx, &ws);
    // With the current branch only, the stash made on its commit still shows when stashes are on.
    ws.update(cx, |ws, cx| ws.toggle_stashes(cx));
    ws.update(cx, |ws, cx| ws.set_scope(gitgui_core::Scope::Current, cx));
    assert!(has_stash(&ws, cx));
}

#[gpui::test]
async fn local_changes_list_under_the_project_preview_stage_and_commit(cx: &mut TestAppContext) {
    let fx = fixture("worktree");
    fx.write("src/a.rs", "one\nTWO\nthree\nfour\nfive\nsix\nseven\nEIGHT\nnine\nten\n");
    fx.write("notes.md", "# notes\n");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);

    let work = |ws: &Entity<Workspace>, cx: &VisualTestContext| -> Vec<(String, bool)> {
        ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().work.iter().map(|f| (f.change.path.clone(), f.staged)).collect())
    };
    assert_eq!(work(&ws, cx), [("notes.md".to_owned(), false), ("src/a.rs".to_owned(), false)]);

    // A click on a change opens its diff in the pane, from the working tree.
    ws.update(cx, |ws, cx| ws.open_work_file(1, cx));
    cx.run_until_parked();
    let added: Vec<String> = ws.read_with(cx, |ws, _| {
        let file = ws.repo.as_ref().unwrap().file.as_ref().unwrap();
        file.diff.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == gitgui_core::LineKind::Added).map(|l| l.text.clone()).collect()
    });
    assert_eq!(added, ["ten"]);
    assert!(ws.read_with(cx, |ws, _| ws.work_open()));
    draw(cx, &ws);

    // Staging one keeps it open, now from the index.
    ws.update(cx, |ws, cx| ws.stage_paths(vec!["src/a.rs".into()], true, cx));
    cx.run_until_parked();
    assert_eq!(work(&ws, cx), [("src/a.rs".to_owned(), true), ("notes.md".to_owned(), false)]);
    let open = ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().map(|f| f.index));
    assert_eq!(open, Some(0), "the open file followed its move to Staged");
    draw(cx, &ws);

    // No message, no commit.
    ws.update(cx, |ws, cx| ws.commit_work(cx));
    cx.run_until_parked();
    assert_eq!(summaries(&ws, cx).len(), 3, "uncommitted, change, base");

    ws.update(cx, |ws, cx| ws.commit_input.update(cx, |input, cx| input.set_text("feat: ten", cx)));
    ws.update(cx, |ws, cx| ws.commit_work(cx));
    cx.run_until_parked();
    assert_eq!(summaries(&ws, cx), ["Uncommitted Changes (1)", "feat: ten", "change", "base"], "only what was staged went in");
    assert_eq!(work(&ws, cx), [("notes.md".to_owned(), false)]);
    assert_eq!(ws.read_with(cx, |ws, cx| ws.commit_input.read(cx).text().to_owned()), "", "the message is cleared");
    draw(cx, &ws);
}

#[gpui::test]
async fn up_and_down_step_through_the_files_of_the_working_tree_and_of_a_commit(cx: &mut TestAppContext) {
    let fx = fixture("step");
    fx.write("README.md", "back\n");
    fx.write("src/a.rs", "changed\n");
    fx.git(&["add", "src/a.rs"]);
    fx.write("z.txt", "new\n");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let open = |ws: &Entity<Workspace>, cx: &VisualTestContext| {
        ws.read_with(cx, |ws, _| {
            let repo = ws.repo.as_ref().unwrap();
            let file = repo.file.as_ref()?;
            Some(if ws.work_open() { repo.work[file.index].change.path.clone() } else { file.index.to_string() })
        })
    };
    let step = |ws: &Entity<Workspace>, cx: &mut VisualTestContext, delta: isize| {
        ws.update(cx, |ws, cx| ws.step_file(delta, cx));
        cx.run_until_parked();
    };

    // The working tree, in the sidebar's order: staged src/a.rs, then README.md and z.txt.
    ws.update(cx, |ws, cx| ws.open_work(cx));
    step(&ws, cx, 1);
    assert_eq!(open(&ws, cx).as_deref(), Some("src/a.rs"), "Down opens the first");
    step(&ws, cx, 1);
    assert_eq!(open(&ws, cx).as_deref(), Some("README.md"));
    step(&ws, cx, 1);
    step(&ws, cx, 1);
    assert_eq!(open(&ws, cx).as_deref(), Some("z.txt"), "and stops at the last");
    step(&ws, cx, -1);
    assert_eq!(open(&ws, cx).as_deref(), Some("README.md"));
    draw(cx, &ws);

    // A commit's files, in the file list's order.
    select(&ws, cx, "change");
    let rows = ws.read_with(cx, |ws, _| {
        ws.repo.as_ref().unwrap().file_rows.iter().filter_map(|r| match r {
            gitgui_core::TreeRow::File { index, .. } => Some(index.to_string()),
            gitgui_core::TreeRow::Dir { .. } => None,
        }).collect::<Vec<_>>()
    });
    step(&ws, cx, 1);
    assert_eq!(open(&ws, cx).as_ref(), rows.first());
    step(&ws, cx, 1);
    assert_eq!(open(&ws, cx).as_ref(), rows.get(1));
    step(&ws, cx, -1);
    step(&ws, cx, -1);
    assert_eq!(open(&ws, cx).as_ref(), rows.first(), "and stops at the first");
}

#[gpui::test]
async fn the_arrow_keys_reach_the_files_after_typing_a_message_then_clicking_a_file(cx: &mut TestAppContext) {
    let fx = fixture("arrows");
    fx.write("README.md", "back\n");
    fx.write("z.txt", "new\n");
    cx.update(|cx| {
        crate::text_input::bind_keys(cx);
        cx.bind_keys([
            gpui::KeyBinding::new("up", crate::workspace::PreviousFile, None),
            gpui::KeyBinding::new("down", crate::workspace::NextFile, None),
        ]);
    });
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);
    let open = |ws: &Entity<Workspace>, cx: &VisualTestContext| {
        ws.read_with(cx, |ws, _| {
            let repo = ws.repo.as_ref().unwrap();
            repo.file.as_ref().map(|f| repo.work[f.index].change.path.clone())
        })
    };

    // Type in the message box: the arrows are the box's.
    cx.update(|window, cx| ws.read(cx).commit_input.read(cx).focus(window));
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(open(&ws, cx), None, "typing, the arrows do not open files");

    // Click a file, then Down: the next file opens.
    let first = center_of(cx, "change-0".into());
    click(cx, MouseButton::Left, first);
    cx.run_until_parked();
    assert_eq!(open(&ws, cx).as_deref(), Some("README.md"));
    draw(cx, &ws);
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(open(&ws, cx).as_deref(), Some("z.txt"));
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert_eq!(open(&ws, cx).as_deref(), Some("README.md"));
}

#[gpui::test]
async fn one_person_under_two_identities_gets_one_profile(cx: &mut TestAppContext) {
    let fx = fixture("people");
    // Ada (the fixture's own identity) commits work authored under her GitHub identity.
    fx.write("c.txt", "c\n");
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "from github", "--author=Ada L <123+ada@users.noreply.github.com>"]);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);

    let people = ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => view.entries.iter().map(|e| (e.author.to_string(), e.person.clone())).collect::<Vec<_>>(),
        _ => Vec::new(),
    });
    let (github, local) = (&people[0], &people[1]);
    assert_eq!((github.0.as_str(), local.0.as_str()), ("Ada L", "Ada"), "each commit keeps its own name");
    assert_eq!(github.1, local.1, "but both are one person");
    assert_eq!(local.1.name, "Ada", "named as she commits most");
    assert_eq!(local.1.avatar_email, "123+ada@users.noreply.github.com", "pictured by her GitHub account");
}

#[gpui::test]
async fn flat_and_split_are_remembered_for_the_next_start(cx: &mut TestAppContext) {
    use gitgui_store::{DiffMode, FileLayout};
    let fx = merged_pr("remember-layout");

    // First run: the defaults are tree and unified. Switch to flat and split.
    {
        let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
        open_project(&ws, cx, &fx.repo());
        let (layout, mode) = ws.read_with(cx, |ws, _| {
            let repo = ws.repo.as_ref().unwrap();
            (repo.layout, repo.mode)
        });
        assert_eq!((layout, mode), (Layout::Tree, Mode::Unified));

        ws.update(cx, |ws, cx| ws.set_layout(Layout::Flat, cx));
        ws.update(cx, |ws, cx| ws.set_mode(Mode::Split, cx));
        let saved = Store::at(fx.data()).settings().unwrap();
        assert_eq!((saved.file_layout, saved.diff_mode), (FileLayout::Flat, DiffMode::Split), "written as soon as it is chosen");

        // The Settings panel shows the same two choices.
        ws.update(cx, |ws, cx| ws.open_settings(cx));
        draw(cx, &ws);
    }

    // Next start: a brand new workspace on the same data folder opens in flat and split.
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let (layout, mode) = ws.read_with(cx, |ws, _| {
        let repo = ws.repo.as_ref().unwrap();
        (repo.layout, repo.mode)
    });
    assert_eq!((layout, mode), (Layout::Flat, Mode::Split), "restored without touching anything");

    // And it really shows: the file list is flat and the diff is side by side.
    select(&ws, cx, "chore: m1");
    let flat = ws.read_with(cx, |ws, _| {
        ws.repo.as_ref().unwrap().file_rows.iter().all(|row| matches!(row, gitgui_core::TreeRow::File { .. }))
    });
    assert!(flat, "no folder rows in a flat list");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    let has_pairs = ws.read_with(cx, |ws, _| {
        ws.repo.as_ref().unwrap().file.as_ref().unwrap().rows.iter().any(|r| matches!(r, DisplayRow::Pair { .. }))
    });
    assert!(has_pairs, "the first file opens split, with no click on Split");
    draw(cx, &ws);

    // Changing them in the Settings panel works the same way, and the project switch keeps them.
    ws.update(cx, |ws, cx| ws.set_layout(Layout::Tree, cx));
    ws.update(cx, |ws, cx| ws.set_mode(Mode::Unified, cx));
    let back = Store::at(fx.data()).settings().unwrap();
    assert_eq!((back.file_layout, back.diff_mode), (FileLayout::Tree, DiffMode::Unified));
}

/// With every theme and icon theme listed the panel is taller than a short window; it must scroll, not
/// push the Done button off the bottom.
#[gpui::test]
async fn the_settings_panel_fits_a_short_window(cx: &mut TestAppContext) {
    let fx = merged_pr("settings-fit");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    ws.update(cx, |ws, cx| ws.open_settings(cx));
    cx.simulate_resize(size(px(1000.), px(420.)));
    let view = AnyView::from(ws.clone());
    cx.draw(
        point(px(0.), px(0.)),
        size(AvailableSpace::Definite(px(1000.)), AvailableSpace::Definite(px(420.))),
        move |_, _| view,
    );
    let done = cx.debug_bounds("settings-done").expect("the Done button is drawn");
    assert!(done.bottom() <= px(420.), "Done sits at {:?}, below a 420 px window", done.bottom());
    assert!(done.top() >= px(0.));
}

// ---- background work stays bounded --------------------------------------------------------------

fn count(ws: &Entity<Workspace>, cx: &VisualTestContext, which: fn(&crate::workspace::Counts) -> &std::sync::atomic::AtomicUsize) -> usize {
    ws.read_with(cx, |ws, _| which(&ws.counts).load(std::sync::atomic::Ordering::Relaxed))
}

fn notes_shown(ws: &Entity<Workspace>, cx: &VisualTestContext) -> Vec<String> {
    ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => view.entries.iter().flat_map(|e| e.notes.iter().map(|n| n.text.to_string())).collect(),
        _ => Vec::new(),
    })
}

#[gpui::test]
async fn refreshing_when_no_branch_moved_runs_no_new_merge_scan(cx: &mut TestAppContext) {
    let fx = squashed_pr("rescan");
    fx.git(&["branch", "feat/open", "main~1"]);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let notes = notes_shown(&ws, cx);
    assert!(notes.iter().any(|n| n.contains("squash of branch feat/x")), "{notes:?}");
    assert_eq!(count(&ws, cx, |c| &c.scans), 1);

    // Refresh is also what every operation (commit, checkout, merge…) ends with.
    for _ in 0..50 {
        ws.update(cx, |ws, cx| ws.refresh(cx));
        cx.run_until_parked();
        assert_eq!(notes_shown(&ws, cx), notes, "the scan's findings show straight away after each read");
    }
    draw(cx, &ws);
    assert_eq!(count(&ws, cx, |c| &c.repo_reads), 51);
    assert_eq!(count(&ws, cx, |c| &c.scans), 1, "50 refreshes with no branch moved ran no merge scan");
    assert!(!ws.read_with(cx, |ws, _| ws.scan_running()), "nothing is left running");

    assert!(notes.iter().any(|n| n == "✓ merged into main"), "feat/open sits on main's history: {notes:?}");

    // A branch that moves is scanned again: it has work of its own now, so it is not merged any more.
    fx.git(&["checkout", "-q", "feat/open"]);
    commit_file(&fx, "open.txt", "1\n", "wip: open work");
    fx.git(&["checkout", "-q", "main"]);
    ws.update(cx, |ws, cx| ws.refresh(cx));
    cx.run_until_parked();
    assert_eq!(count(&ws, cx, |c| &c.scans), 2);
    let after = notes_shown(&ws, cx);
    assert!(after.iter().any(|n| n.contains("squash of branch feat/x")), "{after:?}");
    assert!(!after.iter().any(|n| n == "✓ merged into main"), "{after:?}");
}

#[gpui::test]
async fn refreshes_asked_for_while_a_read_runs_become_one_more_read(cx: &mut TestAppContext) {
    let fx = squashed_pr("reread");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let before = count(&ws, cx, |c| &c.repo_reads);
    // Holding Cmd-R: 30 refreshes before any read has come back.
    for _ in 0..30 {
        ws.update(cx, |ws, cx| ws.refresh(cx));
    }
    cx.run_until_parked();
    assert_eq!(count(&ws, cx, |c| &c.repo_reads) - before, 2, "the read already running, then one more");
    assert_eq!(count(&ws, cx, |c| &c.scans), 1);
    assert!(ws.read_with(cx, |ws, _| matches!(ws.repo.as_ref().unwrap().phase, Phase::Ready(_))));
    assert_eq!(summaries(&ws, cx)[0], "feat: x (#7)");

    // The extra read sees what changed after the first one started.
    ws.update(cx, |ws, cx| ws.refresh(cx));
    commit_file(&fx, "c.txt", "c\n", "chore: made while reading");
    ws.update(cx, |ws, cx| ws.refresh(cx));
    cx.run_until_parked();
    assert_eq!(summaries(&ws, cx)[0], "chore: made while reading");
}

#[gpui::test]
async fn stepping_through_files_quickly_reads_only_the_one_left_open(cx: &mut TestAppContext) {
    let fx = fixture("step-fast");
    for n in 0..12 {
        fx.write(&format!("src/f{n:02}.rs"), &format!("fn f{n}() {{ let s = \"{n}\"; }}\n"));
    }
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "twelve files"]);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());

    // Clicking down the graph faster than git answers: only the last commit is read.
    let before = count(&ws, cx, |c| &c.commit_reads);
    for summary in ["base", "change", "twelve files"] {
        let ix = index_of(&ws, cx, summary);
        ws.update(cx, |ws, cx| ws.select_entry(ix, cx));
    }
    cx.run_until_parked();
    assert_eq!(count(&ws, cx, |c| &c.commit_reads) - before, 1);
    assert_eq!(file_names(&ws, cx).len(), 12);

    // Holding Down through the files: only the one it stops on is read and colored.
    let before = count(&ws, cx, |c| &c.file_reads);
    for n in 0..12 {
        ws.update(cx, |ws, cx| ws.open_file(n, cx));
    }
    cx.run_until_parked();
    draw(cx, &ws);
    assert_eq!(count(&ws, cx, |c| &c.file_reads) - before, 1);
    let (index, ready, text) = ws.read_with(cx, |ws, _| {
        let file = ws.repo.as_ref().unwrap().file.as_ref().unwrap();
        let text = file.diff.hunks.iter().flat_map(|h| &h.lines).map(|l| l.text.clone()).collect::<Vec<_>>().join("\n");
        (file.index, matches!(file.phase, Phase::Ready(())), text)
    });
    assert_eq!((index, ready), (11, true));
    assert!(text.contains("fn f11()"), "{text}");
    assert!(ws.read_with(cx, |ws, _| !ws.repo.as_ref().unwrap().file.as_ref().unwrap().colors.new.0.is_empty()), "and it is colored");

    // Closing the file stops a read still running for it.
    let before = count(&ws, cx, |c| &c.file_reads);
    ws.update(cx, |ws, cx| {
        ws.open_file(0, cx);
        ws.close_file(cx);
    });
    cx.run_until_parked();
    assert_eq!(count(&ws, cx, |c| &c.file_reads), before);
}
