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
    ws.update(cx, |ws, cx| ws.set_mode(Mode::Unified, cx)); // one row per line: the most rows
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

use crate::menu::{Action, MenuTarget};

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
    // The default view shows every branch, so the squashed one is there.
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
    }), CommitKind::Squash);

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

    // Confirming runs it: the merge stops, its first file opens in the resolver, and the bar offers Abort, which asks.
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::Merge("other".into()), window, cx));
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    assert_eq!(GitCli::new(fx.repo()).in_progress(), Some(Operation::Merge));
    let text = ws.read_with(cx, |ws, _| ws.notice.as_ref().unwrap().text.to_string());
    assert!(text.contains("stopped") && text.contains("conflicts"), "{text}");
    assert_eq!(ws.read_with(cx, |ws, _| ws.resolver().map(|r| r.path.clone())).as_deref(), Some("app.txt"));
    draw(cx, &ws);

    with_window(&ws, cx, |ws, window, cx| ws.open_dialog(Action::AbortOperation(Operation::Merge), window, cx));
    assert_eq!(dialog_text(&ws, cx).0, "Abort the merge?");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    assert_eq!(GitCli::new(fx.repo()).in_progress(), None);
    assert_eq!(std::fs::read_to_string(fx.repo().join("app.txt")).unwrap(), "one\ntwo\nthree main\nfour\nfive\n".replace("three main", "THREE main"));
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
    // A real pointer arrives before it presses.
    cx.simulate_event(MouseMoveEvent { position: at, pressed_button: None, modifiers: Modifiers::default() });
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
    // The default view is every branch; Branch + base is the branch, the one it was cut from, and their remote copies.
    assert_eq!(summaries(&ws, cx), ["feat work", "other work", "change", "base"]);
    ws.update(cx, |ws, cx| ws.set_scope(Scope::Focus, cx));
    assert_eq!(summaries(&ws, cx), ["feat work", "change", "base"]);
    ws.update(cx, |ws, cx| ws.set_scope(Scope::All, cx));
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

    // First run: the defaults are tree and split. Switch to flat and unified.
    {
        let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
        open_project(&ws, cx, &fx.repo());
        let (layout, mode) = ws.read_with(cx, |ws, _| {
            let repo = ws.repo.as_ref().unwrap();
            (repo.layout, repo.mode)
        });
        assert_eq!((layout, mode), (Layout::Tree, Mode::Split));

        ws.update(cx, |ws, cx| ws.set_layout(Layout::Flat, cx));
        ws.update(cx, |ws, cx| ws.set_mode(Mode::Unified, cx));
        let saved = Store::at(fx.data()).settings().unwrap();
        assert_eq!((saved.file_layout, saved.diff_mode), (FileLayout::Flat, DiffMode::Unified), "written as soon as it is chosen");

        // The Settings panel shows the same two choices.
        ws.update(cx, |ws, cx| ws.open_settings(cx));
        draw(cx, &ws);
    }

    // Next start: a brand new workspace on the same data folder opens in flat and unified.
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let (layout, mode) = ws.read_with(cx, |ws, _| {
        let repo = ws.repo.as_ref().unwrap();
        (repo.layout, repo.mode)
    });
    assert_eq!((layout, mode), (Layout::Flat, Mode::Unified), "restored without touching anything");

    // And it really shows: the file list is flat and the diff is one column.
    select(&ws, cx, "chore: m1");
    let flat = ws.read_with(cx, |ws, _| {
        ws.repo.as_ref().unwrap().file_rows.iter().all(|row| matches!(row, gitgui_core::TreeRow::File { .. }))
    });
    assert!(flat, "no folder rows in a flat list");
    // (A new file has one side only, so it is shown as one column; this commit changes an existing file.)
    select(&ws, cx, "feat: x2");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    let has_pairs = ws.read_with(cx, |ws, _| {
        ws.repo.as_ref().unwrap().file.as_ref().unwrap().rows.iter().any(|r| matches!(r, DisplayRow::Pair { .. }))
    });
    assert!(!has_pairs, "the first file opens unified, with no click on Unified");
    draw(cx, &ws);

    // Changing them in the Settings panel works the same way, and the project switch keeps them.
    ws.update(cx, |ws, cx| ws.set_layout(Layout::Tree, cx));
    ws.update(cx, |ws, cx| ws.set_mode(Mode::Split, cx));
    let back = Store::at(fx.data()).settings().unwrap();
    assert_eq!((back.file_layout, back.diff_mode), (FileLayout::Tree, DiffMode::Split));
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

fn commit_text_as(fx: &Fixture, who: (&str, &str), file: &str, text: &str, message: &str) {
    fx.write(file, text);
    fx.git(&["add", "."]);
    let ok = Command::new("git")
        .arg("-C")
        .arg(fx.repo())
        .args(["-c", "commit.gpgsign=false", "commit", "-q", "-m", message])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", who.0)
        .env("GIT_AUTHOR_EMAIL", who.1)
        .env("GIT_COMMITTER_NAME", who.0)
        .env("GIT_COMMITTER_EMAIL", who.1)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git commit as {}", who.0);
}

/// Resting the pointer on a line of the diff says who last changed it, beside that line and nowhere else.
#[gpui::test]
async fn pointing_at_a_line_says_who_last_changed_it(cx: &mut TestAppContext) {
    let fx = bare_fixture("blame-hover");
    let before = "one\ntwo\nthree\nfour\nfive\nsix\nseven\n";
    commit_text_as(&fx, ("Ada", "ada@example.com"), "f.txt", before, "add the file");
    commit_text_as(&fx, ("Grace", "grace@example.com"), "f.txt", &before.replace("four", "FOUR"), "shout four");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    ws.update(cx, |ws, cx| ws.set_mode(Mode::Unified, cx));
    select(&ws, cx, "shout four");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    draw(cx, &ws);

    // The row showing a line with this text, in the unified diff.
    let row_of = |ws: &Entity<Workspace>, cx: &VisualTestContext, text: &str, kind: gitgui_core::LineKind| -> usize {
        ws.read_with(cx, |ws, _| {
            let file = ws.repo.as_ref().unwrap().file.as_ref().unwrap();
            file.rows
                .iter()
                .position(|row| match *row {
                    crate::rows::DisplayRow::Line { hunk, line } => {
                        let line = &file.diff.hunks[hunk].lines[line];
                        line.text == text && line.kind == kind
                    }
                    _ => false,
                })
                .unwrap_or_else(|| panic!("no row for {text:?}"))
        })
    };
    let note = |ws: &Entity<Workspace>, cx: &VisualTestContext, row: usize| ws.read_with(cx, |ws, _| ws.blame_note(row));
    use gitgui_core::LineKind::{Added, Context, Removed};

    // Nothing is pointed at, so nothing is said.
    let two = row_of(&ws, cx, "two", Context);
    assert!(note(&ws, cx, two).is_none());
    assert!(cx.debug_bounds("blame-note").is_none());

    // An unchanged line is Ada's, from the first commit.
    ws.update(cx, |ws, cx| ws.hover_diff_line(two, 0, true, cx));
    cx.run_until_parked();
    draw(cx, &ws);
    let said = note(&ws, cx, two).expect("a note on the pointed line").text.to_string();
    assert!(said.contains("Ada") && said.contains("add the file"), "{said}");
    assert!(cx.debug_bounds("blame-note").is_some(), "and it is drawn beside the line");
    assert!(note(&ws, cx, row_of(&ws, cx, "seven", Context)).is_none(), "only on the line pointed at");

    // The line this commit changed: it is this commit's own.
    let shout = row_of(&ws, cx, "FOUR", Added);
    ws.update(cx, |ws, cx| ws.hover_diff_line(shout, 0, true, cx));
    assert_eq!(note(&ws, cx, shout).unwrap().text.to_string(), "This change");

    // The line it removed was Ada's, as the commit's parent had it.
    let gone = row_of(&ws, cx, "four", Removed);
    ws.update(cx, |ws, cx| ws.hover_diff_line(gone, 0, true, cx));
    let said = note(&ws, cx, gone).unwrap();
    assert!(said.text.contains("Ada") && said.text.contains("add the file"), "{}", said.text);
    assert!(said.commit.is_some(), "and a click on it goes to that commit");

    // Leaving the line takes the note away.
    ws.update(cx, |ws, cx| ws.hover_diff_line(gone, 0, false, cx));
    assert!(note(&ws, cx, gone).is_none());
}

/// A repository whose team names branches `type/ticket-title` and merges them into `develop` with pull-request merges.
fn team_repo(name: &str) -> Fixture {
    let fx = bare_fixture(name);
    commit_file(&fx, "a.txt", "base\n", "base");
    fx.git(&["branch", "develop"]);
    fx.git(&["checkout", "-q", "develop"]);
    for (i, branch) in ["feature/74-driver-reporting", "feature/75-booking", "bugfix/90-crash", "bugfix/91-typo"].iter().enumerate() {
        fx.git(&["checkout", "-q", "-b", branch]);
        commit_file(&fx, &format!("f{i}.txt"), "x\n", &format!("work {i}"));
        fx.git(&["checkout", "-q", "develop"]);
        fx.git(&["merge", "-q", "--no-ff", "-m", &format!("Merge pull request #{} from acme/{branch}", i + 1), branch]);
    }
    fx
}

/// The workflow is read from the branches, kept per project, and the New branch window builds a name the team's way
/// and starts it from the team's branch.
#[gpui::test]
async fn the_new_branch_window_names_the_branch_the_way_the_team_does(cx: &mut TestAppContext) {
    use crate::settings_view::SettingsPage;
    let fx = team_repo("workflow");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    cx.run_until_parked();

    // What the branches show.
    let found = ws.read_with(cx, |ws, _| ws.detected_workflow()).expect("a repository is open");
    assert_eq!(found.shape, gitgui_core::Shape::TypeTicketSlug);
    assert!(found.types.iter().any(|(kind, _)| kind == "feature") && found.types.iter().any(|(kind, _)| kind == "bugfix"));
    assert_eq!(found.base.as_deref(), Some("develop"));
    assert!(ws.read_with(cx, |ws, _| ws.saved_workflow()).is_none(), "nothing is kept until it is accepted");

    // The Settings page says so, and "Use this" keeps it for the project.
    ws.update(cx, |ws, cx| {
        ws.open_settings(cx);
        ws.set_settings_page(SettingsPage::Workflow, cx);
    });
    draw(cx, &ws);
    let use_this = center_of(cx, "workflow-use".to_owned());
    click(cx, MouseButton::Left, use_this);
    let saved = ws.read_with(cx, |ws, _| ws.saved_workflow()).expect("kept");
    assert_eq!((saved.shape.as_str(), saved.base.as_deref()), ("type-ticket-slug", Some("develop")));
    assert_eq!(Store::at(fx.data()).projects().unwrap()[0].workflow, Some(saved), "and it is on disk");
    ws.update(cx, |ws, cx| ws.close_settings(cx));

    // A name that does not follow the habit is said so; one that does is not.
    assert!(ws.read_with(cx, |ws, _| ws.workflow_hint("driver-reporting")).unwrap().contains("type"));
    assert_eq!(ws.read_with(cx, |ws, _| ws.workflow_hint("feature/74-driver-reporting")), None);

    // The window: no type until one is clicked, the team's base, and the name as it is typed.
    ws.update_in(cx, |ws, window, cx| ws.open_new_branch(window, cx));
    ws.update(cx, |ws, cx| {
        ws.branch_ticket.update(cx, |input, cx| input.set_text("#101", cx));
        ws.branch_title.update(cx, |input, cx| input.set_text("Driver Reporting v2", cx));
    });
    draw(cx, &ws);
    let name = |ws: &Entity<Workspace>, cx: &VisualTestContext| ws.read_with(cx, |ws, cx| ws.typed_branch_name(cx));
    assert_eq!(name(&ws, cx), "101-driver-reporting-v2", "a name does not have to start with a type");
    assert!(cx.debug_bounds("new-branch-create").is_some(), "the window is drawn");
    assert_eq!(ws.read_with(cx, |ws, _| ws.new_branch.as_ref().map(|w| w.base.clone())), Some("develop".to_owned()));

    // A click on a type fills in its prefix; another takes it off.
    let feature = center_of(cx, "kind-feature".to_owned());
    click(cx, MouseButton::Left, feature);
    assert_eq!(name(&ws, cx), "feature/101-driver-reporting-v2");
    draw(cx, &ws);
    let feature = center_of(cx, "kind-feature".to_owned());
    click(cx, MouseButton::Left, feature);
    assert_eq!(name(&ws, cx), "101-driver-reporting-v2");
    draw(cx, &ws);
    let feature = center_of(cx, "kind-feature".to_owned());
    click(cx, MouseButton::Left, feature);
    assert_eq!(name(&ws, cx), "feature/101-driver-reporting-v2");

    // "Start from" opens a list to search: it narrows as it is typed, and a click on a branch starts from it.
    draw(cx, &ws);
    let base = center_of(cx, "branch-base".to_owned());
    click(cx, MouseButton::Left, base);
    assert!(ws.read_with(cx, |ws, _| ws.branch_picker.is_some()), "the list opens");
    let listed = |ws: &Entity<Workspace>, cx: &VisualTestContext| {
        ws.read_with(cx, |ws, cx| {
            let query = ws.branch_search.read(cx).text().to_owned();
            ws.branch_pick_rows(&query)
                .into_iter()
                .filter_map(|row| match row {
                    crate::workflow_ui::PickRow::Branch { name, .. } => Some(name),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
    };
    let everything = listed(&ws, cx);
    assert!(everything.iter().any(|b| b == "develop") && everything.iter().any(|b| b == "bugfix/90-crash"), "{everything:?}");
    assert_eq!(everything.iter().filter(|b| *b == "develop").count(), 1, "each branch once");
    ws.update(cx, |ws, cx| ws.branch_search.update(cx, |input, cx| input.set_text("bug", cx)));
    // Newest first, like the graph.
    let found = listed(&ws, cx);
    assert_eq!(found.len(), 2);
    assert!(found.contains(&"bugfix/90-crash".to_owned()) && found.contains(&"bugfix/91-typo".to_owned()), "{found:?}");
    ws.update(cx, |ws, cx| ws.step_branch_pick(1, cx));
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.pick_highlighted_branch(cx));
    assert!(ws.read_with(cx, |ws, _| ws.branch_picker.is_none()), "the list closes");
    assert_eq!(ws.read_with(cx, |ws, _| ws.new_branch.as_ref().map(|w| w.base.clone())), Some(found[1].clone()), "Down, then Enter, picks the second");
    // A click on a row picks that one.
    draw(cx, &ws);
    let base = center_of(cx, "branch-base".to_owned());
    click(cx, MouseButton::Left, base);
    ws.update(cx, |ws, cx| ws.branch_search.update(cx, |input, cx| input.set_text("devel", cx)));
    draw(cx, &ws);
    let row = center_of(cx, "pick-branch-0".to_owned());
    click(cx, MouseButton::Left, row);
    assert_eq!(ws.read_with(cx, |ws, _| ws.new_branch.as_ref().map(|w| w.base.clone())), Some("develop".to_owned()));
    assert!(ws.read_with(cx, |ws, _| ws.new_branch.is_some()), "the window is still there");

    // A name already taken is not made.
    ws.update(cx, |ws, cx| {
        ws.branch_ticket.update(cx, |input, cx| input.set_text("74", cx));
        ws.branch_title.update(cx, |input, cx| input.set_text("driver reporting", cx));
    });
    ws.update(cx, |ws, cx| ws.create_new_branch(cx));
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.new_branch.is_some()), "it stays open");

    // And the real one is made from develop and switched to.
    ws.update(cx, |ws, cx| {
        ws.branch_ticket.update(cx, |input, cx| input.set_text("#101", cx));
        ws.branch_title.update(cx, |input, cx| input.set_text("Driver Reporting v2", cx));
    });
    ws.update(cx, |ws, cx| ws.create_new_branch(cx));
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.new_branch.is_none()), "the window closes");
    let git = GitCli::new(fx.repo());
    assert_eq!(git.current_branch().unwrap().as_deref(), Some("feature/101-driver-reporting-v2"));
    let rev = |name: &str| {
        let out = Command::new("git").arg("-C").arg(fx.repo()).args(["rev-parse", name]).output().unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(rev("feature/101-driver-reporting-v2"), rev("develop"), "it starts where develop is");
}

/// Clicking a style card in the settings changes how the graph is colored, at once, and is kept for the next launch.
#[gpui::test]
async fn choosing_a_graph_style_in_the_settings_recolors_the_graph_and_is_kept(cx: &mut TestAppContext) {
    let _only = crate::graph_style::STYLE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let fx = merged_pr("graph-style");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    ws.update(cx, |ws, cx| {
        ws.open_settings(cx);
        ws.set_settings_page(crate::settings_view::SettingsPage::GraphStyle, cx);
    });
    draw(cx, &ws);
    crate::graph_style::set_active("theme");
    let before = crate::graph_style::lane(0);

    let card = center_of(cx, "graph-style-neon".to_owned());
    click(cx, MouseButton::Left, card);
    draw(cx, &ws);
    assert_eq!(crate::graph_style::active().id, "neon");
    assert_ne!(crate::graph_style::lane(0), before, "the lines have the new colors");
    assert_eq!(Store::at(fx.data()).settings().unwrap().graph_style, "neon", "and it is saved");

    // Back to the theme's own, so the other tests see the colors they expect.
    let card = center_of(cx, "graph-style-theme".to_owned());
    click(cx, MouseButton::Left, card);
    assert_eq!(crate::graph_style::lane(0), before);
    assert_eq!(Store::at(fx.data()).settings().unwrap().graph_style, "theme");
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

    // No merge commit brought it in: git cannot say it was merged, only that it is already there.
    assert!(notes.iter().any(|n| n == "✓ already in main"), "feat/open sits on main's history: {notes:?}");

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

#[gpui::test]
async fn pictures_looked_at_are_let_go_once_another_file_is_open(cx: &mut TestAppContext) {
    let fx = fixture("pictures-let-go");
    for n in 0..4u8 {
        let picture = image::RgbaImage::from_fn(64, 48, |x, y| image::Rgba([x as u8, y as u8, n, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        picture.write_to(&mut out, image::ImageFormat::Png).unwrap();
        std::fs::write(fx.repo().join(format!("p{n}.png")), out.into_inner()).unwrap();
    }
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "pictures"]);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "pictures");

    let mut seen = Vec::new();
    for n in 0..4 {
        ws.update(cx, |ws, cx| ws.open_file(n, cx));
        cx.run_until_parked();
        draw(cx, &ws);
        let picture = ws.read_with(cx, |ws, _| {
            let images = ws.repo.as_ref().unwrap().file.as_ref().unwrap().images.clone().expect("a picture");
            assert_eq!(images.new.as_ref().map(|p| (p.width, p.height)), Some((64, 48)));
            images.new.unwrap().source
        });
        seen.push(std::sync::Arc::downgrade(&picture));
    }
    draw(cx, &ws);
    // Only the one open is still held; the window was told to drop the others' textures.
    let alive: Vec<bool> = seen.iter().map(|p| p.upgrade().is_some()).collect();
    assert_eq!(alive, [false, false, false, true]);
    ws.update(cx, |ws, cx| ws.close_pane(cx));
    draw(cx, &ws);
    assert!(seen[3].upgrade().is_none(), "closing the pane lets go of the last one too");
}

/// Switching projects must free the old project's history at once, not when the new one has loaded:
/// otherwise both are in memory together and memory spikes on every switch.
#[gpui::test]
async fn switching_projects_frees_the_old_history_before_the_new_one_loads(cx: &mut TestAppContext) {
    let (first, second) = (merged_pr("switch-a"), merged_pr("switch-b"));
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(first.data())), cx));
    open_project(&ws, cx, &first.repo());
    open_project(&ws, cx, &second.repo());
    let old = ws.read_with(cx, |ws, _| ws.kept_history()).expect("the second project's history is kept");
    assert!(old.upgrade().is_some());

    let first_path = ws.read_with(cx, |ws, _| ws.projects[0].path.clone());
    ws.update(cx, |ws, cx| ws.select_project(first_path, cx));
    // Not yet read: the old history must already be gone.
    assert!(old.upgrade().is_none(), "the old project's commits are still held while the new one loads");
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.kept_history()).is_some_and(|new| new.upgrade().is_some()));
}

#[gpui::test]
async fn the_graph_size_setting_scales_the_rows_and_is_remembered(cx: &mut TestAppContext) {
    let fx = merged_pr("graph-scale");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);
    let height = |cx: &mut VisualTestContext| cx.debug_bounds("row-1").expect("a row is drawn").size.height;
    let normal = height(cx);
    ws.update(cx, |ws, cx| ws.set_graph_scale(150, cx));
    draw(cx, &ws);
    let large = height(cx);
    assert!((large / normal - 1.5).abs() < 0.01, "rows went from {normal:?} to {large:?}");
    assert_eq!(Store::at(fx.data()).settings().unwrap().graph_scale, 150, "kept for the next start");
    // Out of range values are brought inside.
    ws.update(cx, |ws, cx| ws.set_graph_scale(5000, cx));
    assert_eq!(ws.read_with(cx, |ws, _| ws.settings.graph_scale), 200);
}

/// Runs git in `dir` as a colleague would.
fn git_as_bo(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Bo")
        .env("GIT_AUTHOR_EMAIL", "bo@example.com")
        .env("GIT_COMMITTER_NAME", "Bo")
        .env("GIT_COMMITTER_EMAIL", "bo@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// "Fetch" brings in what colleagues pushed, so it can be started from; a new branch can be pushed as it is made, and
/// one that was not says so when pulled instead of git's words about tracking.
#[gpui::test]
async fn fetch_brings_in_remote_branches_and_a_new_branch_can_be_pushed_as_it_is_made(cx: &mut TestAppContext) {
    let fx = bare_fixture("fetch-publish");
    commit_file(&fx, "a.txt", "base\n", "base");
    let remote = fx.0.join("remote.git");
    fx.git(&["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    fx.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    fx.git(&["push", "-q", "-u", "origin", "main"]);
    // A colleague pushes a branch of their own.
    let other = fx.0.join("other");
    git_as_bo(&fx.0, &["clone", "-q", remote.to_str().unwrap(), other.to_str().unwrap()]);
    git_as_bo(&other, &["switch", "-q", "-c", "feature/theirs"]);
    std::fs::write(other.join("theirs.txt"), "t\n").unwrap();
    git_as_bo(&other, &["add", "."]);
    git_as_bo(&other, &["commit", "-q", "-m", "theirs"]);
    git_as_bo(&other, &["push", "-q", "origin", "feature/theirs"]);

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);
    let listed = |ws: &Entity<Workspace>, cx: &VisualTestContext| {
        ws.read_with(cx, |ws, _| {
            ws.branch_pick_rows("")
                .into_iter()
                .filter_map(|row| match row {
                    crate::workflow_ui::PickRow::Branch { name, .. } => Some(name),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
    };
    assert!(!listed(&ws, cx).iter().any(|b| b.contains("theirs")), "not known here yet");
    let at = center_of(cx, "fetch".to_owned());
    click(cx, MouseButton::Left, at);
    cx.run_until_parked();
    draw(cx, &ws);
    assert!(listed(&ws, cx).iter().any(|b| b == "origin/feature/theirs"), "{:?}", listed(&ws, cx));
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| !n.warn && n.text.contains("Fetched"))));
    assert!(!fx.repo().join("theirs.txt").exists(), "fetching changes no files");

    // A branch made and pushed in one go has a remote branch to pull from.
    ws.update_in(cx, |ws, window, cx| ws.open_new_branch(window, cx));
    ws.update(cx, |ws, cx| ws.branch_title.update(cx, |input, cx| input.set_text("pushed work", cx)));
    draw(cx, &ws);
    assert!(cx.debug_bounds("branch-publish").is_some(), "there is a remote to push to");
    let at = center_of(cx, "branch-publish".to_owned());
    click(cx, MouseButton::Left, at);
    ws.update(cx, |ws, cx| ws.create_new_branch(cx));
    cx.run_until_parked();
    assert_eq!(git_as_bo(&fx.repo(), &["rev-parse", "--abbrev-ref", "pushed-work@{upstream}"]).trim(), "origin/pushed-work");
    assert!(!git_as_bo(&remote, &["branch", "--list", "pushed-work"]).trim().is_empty(), "it is on the remote");

    // One that was not pushed says why a pull has nothing to do.
    ws.update_in(cx, |ws, window, cx| ws.open_new_branch(window, cx));
    ws.update(cx, |ws, cx| ws.branch_title.update(cx, |input, cx| input.set_text("local only", cx)));
    ws.update(cx, |ws, cx| ws.create_new_branch(cx));
    cx.run_until_parked();
    draw(cx, &ws);
    let at = center_of(cx, "pull-rebase".to_owned());
    click(cx, MouseButton::Left, at);
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    let said = ws.read_with(cx, |ws, _| ws.notice.as_ref().map(|n| (n.warn, n.text.to_string())));
    assert!(said.as_ref().is_some_and(|(warn, text)| *warn && text.contains("local-only is not on a remote yet")), "{said:?}");
}

#[gpui::test]
async fn the_pull_rebase_button_asks_then_pulls_and_keeps_history_straight(cx: &mut TestAppContext) {
    let fx = bare_fixture("pull-button");
    commit_file(&fx, "a.txt", "base\n", "base");
    let remote = fx.0.join("remote.git");
    fx.git(&["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    fx.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    fx.git(&["push", "-q", "-u", "origin", "main"]);
    // A colleague's commit lands on the remote; ours is not pushed yet.
    let other = fx.0.join("other");
    let run = |dir: &Path, args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Bo")
            .env("GIT_AUTHOR_EMAIL", "bo@example.com")
            .env("GIT_COMMITTER_NAME", "Bo")
            .env("GIT_COMMITTER_EMAIL", "bo@example.com")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    run(&fx.0, &["clone", "-q", remote.to_str().unwrap(), other.to_str().unwrap()]);
    std::fs::write(other.join("theirs.txt"), "t\n").unwrap();
    run(&other, &["add", "."]);
    run(&other, &["commit", "-q", "-m", "theirs"]);
    run(&other, &["push", "-q", "origin", "main"]);
    commit_file(&fx, "mine.txt", "m\n", "mine");

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);

    // Clicking the button only asks.
    let at = center_of(cx, "pull-rebase".to_owned());
    click(cx, MouseButton::Left, at);
    let (title, confirm) = ws.read_with(cx, |ws, _| {
        let d = ws.dialog.as_ref().expect("a question is asked first");
        (d.title.to_string(), d.confirm.to_string())
    });
    assert!(title.contains("Pull main with rebase"), "{title}");
    assert_eq!(confirm, "Pull");
    assert!(!fx.repo().join("theirs.txt").exists(), "nothing has been pulled yet");

    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    assert!(fx.repo().join("theirs.txt").exists(), "their commit is here");
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| !n.warn && n.text.contains("Pulled main"))));
    let merges = Command::new("git").arg("-C").arg(fx.repo()).args(["rev-list", "--merges", "--count", "HEAD"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&merges.stdout).trim(), "0", "no merge commit");
}

#[gpui::test]
async fn show_more_lines_widens_the_context_around_each_change_and_collapse_goes_back(cx: &mut TestAppContext) {
    let fx = bare_fixture("more-context");
    let numbered = |changed: Option<usize>| -> String {
        (1..=100).map(|n| if Some(n) == changed { "CHANGED\n".to_owned() } else { format!("line {n}\n") }).collect()
    };
    commit_file(&fx, "long.txt", &numbered(None), "base");
    commit_file(&fx, "long.txt", &numbered(Some(50)), "edit the middle");

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "edit the middle");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    draw(cx, &ws);
    let lines = |cx: &VisualTestContext| ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().unwrap().diff.line_count());
    let context = |cx: &VisualTestContext| ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().unwrap().context);
    assert_eq!((context(cx), lines(cx)), (3, 8), "3 lines around one changed line: 3 + removed + added + 3");

    draw(cx, &ws); // the pane's height is known from the first frame on
    // The toolbar button asks for more.
    let at = center_of(cx, "more-context".to_owned());
    click(cx, MouseButton::Left, at);
    cx.run_until_parked();
    assert_eq!(context(cx), 25);
    assert_eq!(lines(cx), 52, "25 + removed + added + 25");

    ws.update(cx, |ws, cx| ws.more_context(cx));
    cx.run_until_parked();
    assert_eq!(context(cx), 100);
    assert_eq!(lines(cx), 101, "the whole file: a hundred lines, one of them shown twice (removed and added)");
    ws.update(cx, |ws, cx| ws.more_context(cx));
    cx.run_until_parked();
    ws.update(cx, |ws, cx| ws.more_context(cx));
    cx.run_until_parked();
    assert_eq!(context(cx), crate::workspace::WHOLE_FILE);

    draw(cx, &ws);
    let at = center_of(cx, "less-context".to_owned());
    click(cx, MouseButton::Left, at);
    cx.run_until_parked();
    assert_eq!((context(cx), lines(cx)), (3, 8), "collapsed back");
}

#[gpui::test]
async fn long_lines_scroll_sideways_and_the_minimap_jumps_to_a_block(cx: &mut TestAppContext) {
    let fx = bare_fixture("minimap");
    let long = "x".repeat(300);
    let text = |a: &str, b: &str| -> String {
        (1..=400).map(|n| match n { 20 => format!("{a}\n"), 380 => format!("{b}\n"), _ => format!("line {n} {long}\n") }).collect()
    };
    commit_file(&fx, "wide.txt", &text("before one", "before two"), "base");
    commit_file(&fx, "wide.txt", &text("after one", "after two"), "change twice");

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    ws.update(cx, |ws, cx| ws.set_mode(Mode::Unified, cx)); // a removed and an added row each, for the ticks below
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "change twice");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    ws.update(cx, |ws, cx| ws.set_diff_context(Some(crate::workspace::WHOLE_FILE), cx));
    cx.run_until_parked();
    draw(cx, &ws);

    // A line longer than the pane can be scrolled to: the code has more to show than fits.
    let pane = cx.debug_bounds("minimap").expect("the minimap is drawn");
    let fits = ws.read_with(cx, |ws, _| {
        let file = ws.repo.as_ref().unwrap().file.as_ref().unwrap();
        (file.content_w.get(), f32::from(file.diff_bounds.get().size.width))
    });
    assert!(fits.0 > fits.1, "300 columns are wider than the pane: {fits:?}");

    // A sideways scroll over the code moves it.
    let x = |cx: &VisualTestContext| ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().unwrap().scroll_x.get());
    assert_eq!(x(cx), 0.);
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: point(pane.origin.x - px(300.), pane.origin.y + px(200.)),
        delta: gpui::ScrollDelta::Pixels(point(px(-250.), px(0.))),
        ..Default::default()
    });
    draw(cx, &ws);
    assert!(x(cx) > 100., "the code moved: {}", x(cx));

    // Two changes, far apart: two ticks (a removed line and an added one each, merged by split view or not).
    let ticks = ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().unwrap().minimap.marks.len());
    assert_eq!(ticks, 4, "removed + added at each of the two places");

    let top = |cx: &VisualTestContext| ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().unwrap().list.logical_scroll_top().item_ix);
    assert_eq!(top(cx), 0);
    // Clicking near the bottom of the strip goes to the second change.
    let below = point(pane.center().x, pane.origin.y + pane.size.height * 0.93);
    click(cx, MouseButton::Left, below);
    draw(cx, &ws);
    assert!(top(cx) > 300, "jumped to near the end, row {}", top(cx));
    // And near the top goes back.
    click(cx, MouseButton::Left, point(pane.center().x, pane.origin.y + pane.size.height * 0.02));
    draw(cx, &ws);
    assert!(top(cx) < 30, "back near the start, row {}", top(cx));
}

#[gpui::test]
async fn the_project_open_last_time_opens_again_on_the_next_start(cx: &mut TestAppContext) {
    let fx = merged_pr("last-project");
    let data = fx.data();
    // The app keeps the real path (on a Mac /var is really /private/var).
    let repo = fx.repo().canonicalize().unwrap();
    {
        let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(data.clone())), cx));
        assert!(ws.read_with(cx, |ws, _| ws.repo.is_none()), "nothing was open before");
        open_project(&ws, cx, &fx.repo());
        assert_eq!(Store::at(data.clone()).settings().unwrap().last_project, Some(repo.clone()));
    }
    // A new start: the project is open and its history is read without a click.
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(data.clone())), cx));
    cx.run_until_parked();
    let open = ws.read_with(cx, |ws, _| ws.repo.as_ref().map(|r| r.project.path.clone()));
    assert_eq!(open, Some(repo.clone()));
    assert!(!summaries(&ws, cx).is_empty(), "the history is there");

    // Removing it forgets it: the next start opens nothing.
    ws.update(cx, |ws, cx| ws.remove_project(&repo, cx));
    assert_eq!(Store::at(data.clone()).settings().unwrap().last_project, None);
    let (again, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(data.clone())), cx));
    assert!(again.read_with(cx, |ws, _| ws.repo.is_none()));
}

#[gpui::test]
async fn a_last_project_whose_folder_is_gone_is_not_opened(cx: &mut TestAppContext) {
    let fx = merged_pr("last-gone");
    let data = fx.data();
    {
        let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(data.clone())), cx));
        open_project(&ws, cx, &fx.repo());
    }
    std::fs::remove_dir_all(fx.repo()).unwrap();
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(data.clone())), cx));
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.repo.is_none()), "no error screen for a folder that moved");
}

#[gpui::test]
async fn the_date_gives_way_so_the_description_keeps_its_room(cx: &mut TestAppContext) {
    let fx = merged_pr("narrow-columns");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    // A wide window shows the whole row; the graph pane squeezed narrow by an open file pane draws without
    // panicking too.
    draw(cx, &ws);
    select(&ws, cx, "Merge pull request #1 from owner/feat/x");
    draw(cx, &ws);
    cx.simulate_resize(size(px(900.), px(600.)));
    let view = AnyView::from(ws.clone());
    cx.draw(point(px(0.), px(0.)), size(AvailableSpace::Definite(px(900.)), AvailableSpace::Definite(px(600.))), move |_, _| view);
    assert!(cx.debug_bounds("row-0").is_some());
}

#[gpui::test]
async fn pictures_in_the_graph_are_for_branch_tips_unless_the_setting_says_otherwise(cx: &mut TestAppContext) {
    use gitgui_store::GraphFaces;
    let fx = merged_pr("faces");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);

    let faces = |cx: &VisualTestContext, highlight: Option<usize>| -> (usize, usize) {
        ws.read_with(cx, |ws, _| {
            let mode = ws.settings.graph_faces;
            let Phase::Ready(view) = &ws.repo.as_ref().unwrap().phase else { panic!("not ready") };
            let commits: Vec<_> = view.entries.iter().filter(|e| e.commit.is_some()).collect();
            (commits.iter().filter(|e| crate::graph::shows_face(mode, e, highlight)).count(), commits.len())
        })
    };
    assert_eq!(ws.read_with(cx, |ws, _| ws.settings.graph_faces), GraphFaces::Tips, "the default");
    let (tips, all) = faces(cx, None);
    assert!(tips >= 1 && tips < all, "only some commits have a picture: {tips} of {all}");
    // Every shown picture is on a commit with a branch label, or the one HEAD is on.
    ws.read_with(cx, |ws, _| {
        let Phase::Ready(view) = &ws.repo.as_ref().unwrap().phase else { panic!() };
        for entry in view.entries.iter().filter(|e| e.commit.is_some()) {
            let tip = entry.labels.iter().any(|l| matches!(l.kind, gitgui_core::LabelKind::Branch | gitgui_core::LabelKind::RemoteBranch));
            assert_eq!(crate::graph::shows_face(GraphFaces::Tips, entry, None), tip || entry.dot == crate::graph::Dot::Current);
        }
    });

    ws.update(cx, |ws, cx| ws.set_graph_faces(GraphFaces::All, cx));
    assert_eq!(faces(cx, None), (all, all));
    assert_eq!(Store::at(fx.data()).settings().unwrap().graph_faces, GraphFaces::All, "kept for the next start");

    ws.update(cx, |ws, cx| ws.set_graph_faces(GraphFaces::Selected, cx));
    let (none, _) = faces(cx, None);
    assert!(none <= 1, "with nothing selected only the commit HEAD is on has one: {none}");
    let line = ws.read_with(cx, |ws, _| {
        let Phase::Ready(view) = &ws.repo.as_ref().unwrap().phase else { panic!() };
        view.entries.iter().find(|e| e.commit.is_some()).unwrap().row.lineage
    });
    let (on_line, _) = faces(cx, Some(line));
    assert!(on_line >= 1 && on_line < all, "the selected line's commits only: {on_line} of {all}");
    draw(cx, &ws);
}

#[gpui::test]
async fn cloning_by_address_makes_the_folder_adds_the_project_and_opens_it(cx: &mut TestAppContext) {
    let source = merged_pr("clone-source");
    let app = bare_fixture("clone-app");
    let target = app.0.join("clones");
    let url = format!("file://{}", source.repo().display());

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(app.data())), cx));
    draw(cx, &ws);
    // The sidebar button asks for an address and a folder.
    let at = center_of(cx, "clone-repo".to_owned());
    click(cx, MouseButton::Left, at);
    let (title, folder) = ws.read_with(cx, |ws, _| {
        let d = ws.dialog.as_ref().expect("a dialog asks for the address");
        (d.title.to_string(), d.folder.clone())
    });
    assert!(title.contains("Clone"), "{title}");
    assert!(folder.is_some(), "it says where the clone will go");

    // Nothing typed: it waits. A bad address is refused with a reason, and nothing runs.
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    assert!(ws.read_with(cx, |ws, _| ws.dialog.is_some()), "an empty address does not go through");
    ws.update(cx, |ws, cx| ws.dialog_input.update(cx, |input, cx| input.set_text("ext::sh -c touch% /tmp/pwned", cx)));
    ws.update(cx, |ws, cx| {
        ws.dialog.as_mut().unwrap().folder = Some(target.clone());
        ws.confirm_dialog(cx)
    });
    cx.run_until_parked();
    let warn = ws.read_with(cx, |ws, _| ws.notice.as_ref().map(|n| (n.warn, n.text.to_string())));
    assert!(warn.is_some_and(|(warn, text)| warn && text.contains("address")), "refused with a reason");
    assert!(ws.read_with(cx, |ws, _| ws.repo.is_none() && ws.projects.is_empty()));
    assert!(!std::path::Path::new("/tmp/pwned").exists());

    // A good address: cloned into <folder>/<name>, added to the sidebar and opened.
    ws.update(cx, |ws, cx| {
        let url = url.clone();
        let target = target.clone();
        ws.start_clone_for_test(url, target, cx)
    });
    cx.run_until_parked();
    let dest = target.join("repo");
    assert!(dest.join(".git").exists(), "the repository is on disk at {}", dest.display());
    let real = dest.canonicalize().unwrap();
    assert_eq!(ws.read_with(cx, |ws, _| ws.repo.as_ref().map(|r| r.project.path.clone())), Some(real.clone()));
    assert!(ws.read_with(cx, |ws, _| ws.projects.iter().any(|p| p.path == real)));
    assert!(!summaries(&ws, cx).is_empty(), "its history shows");
    assert_eq!(Store::at(app.data()).settings().unwrap().clone_dir, Some(target.clone()), "the folder is remembered");
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| !n.warn && n.text.contains("Cloned"))));

    // The same address again: the folder is there, so nothing is overwritten.
    ws.update(cx, |ws, cx| ws.start_clone_for_test(url.clone(), target.clone(), cx));
    cx.run_until_parked();
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.warn && n.text.contains("already exists"))));
}

#[gpui::test]
async fn the_settings_window_has_pages_and_its_controls_change_and_save_settings(cx: &mut TestAppContext) {
    use crate::settings_view::SettingsPage;
    use gitgui_store::GraphFaces;
    let fx = merged_pr("settings-pages");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    ws.update(cx, |ws, cx| ws.open_settings(cx));
    draw(cx, &ws);
    assert_eq!(ws.read_with(cx, |ws, _| ws.settings_page), SettingsPage::Graph, "opens on the graph page");

    // A switch: click it and the setting flips and is saved.
    let grouped = |cx: &VisualTestContext| ws.read_with(cx, |ws, _| ws.settings.group_by_parent);
    assert!(grouped(cx));
    let at = center_of(cx, "setting-group".to_owned());
    click(cx, MouseButton::Left, at);
    assert!(!grouped(cx));
    assert!(!Store::at(fx.data()).settings().unwrap().group_by_parent, "saved at once");
    draw(cx, &ws);
    let at = center_of(cx, "setting-group".to_owned());
    click(cx, MouseButton::Left, at);
    assert!(grouped(cx));

    // Each page draws, and its controls reach the same settings as the buttons in the panes.
    for (ix, page) in SettingsPage::ALL.into_iter().enumerate() {
        draw(cx, &ws);
        let at = center_of(cx, format!("settings-page-{ix}"));
        click(cx, MouseButton::Left, at);
        assert_eq!(ws.read_with(cx, |ws, _| ws.settings_page), page);
        draw(cx, &ws);
    }
    // The appearance page lists the themes; choosing one applies it.
    ws.update(cx, |ws, cx| ws.set_settings_page(SettingsPage::Appearance, cx));
    draw(cx, &ws);
    let other = ws.read_with(cx, |ws, _| ws.themes.iter().map(|t| t.name.clone()).find(|n| *n != crate::theme::t().name));
    if let Some(other) = other {
        ws.update(cx, |ws, cx| ws.set_theme(&other, cx));
        assert_eq!(Store::at(fx.data()).settings().unwrap().theme.as_deref(), Some(other.as_str()));
    }
    ws.update(cx, |ws, cx| ws.set_graph_faces(GraphFaces::All, cx));
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.back(cx));
    assert!(!ws.read_with(cx, |ws, _| ws.settings_open), "Esc closes it");
}

#[gpui::test]
async fn scrolling_down_and_sideways_do_not_fight_and_the_minimap_passes_the_wheel_on(cx: &mut TestAppContext) {
    let fx = bare_fixture("scroll-axes");
    let long = "y".repeat(300);
    let text = |a: &str| -> String { (1..=400).map(|n| if n == 200 { format!("{a}\n") } else { format!("row {n} {long}\n") }).collect() };
    commit_file(&fx, "w.txt", &text("before"), "base");
    commit_file(&fx, "w.txt", &text("after"), "change");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "change");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    ws.update(cx, |ws, cx| ws.set_diff_context(Some(crate::workspace::WHOLE_FILE), cx));
    cx.run_until_parked();
    draw(cx, &ws);

    let top = |cx: &VisualTestContext| ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().unwrap().list.logical_scroll_top());
    let x = |cx: &VisualTestContext| ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().file.as_ref().unwrap().scroll_x.get());
    let list = cx.debug_bounds("diff-list").unwrap();
    let over_code = point(list.origin.x + px(200.), list.origin.y + px(200.));
    let wheel = |cx: &mut VisualTestContext, at: Point<gpui::Pixels>, dx: f32, dy: f32| {
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(point(px(dx), px(dy))),
            ..Default::default()
        });
    };

    // Mostly down, a little sideways: it scrolls down and the code does not slide.
    wheel(cx, over_code, -4., -120.);
    draw(cx, &ws);
    assert!(top(cx).item_ix > 0, "scrolled down");
    assert_eq!(x(cx), 0., "a little sideways noise does not move the code");

    // Mostly sideways, a little down: the code slides and the rows stay where they were.
    let before = top(cx);
    wheel(cx, over_code, -200., -6.);
    draw(cx, &ws);
    assert!(x(cx) > 100., "slid sideways: {}", x(cx));
    let after = top(cx);
    assert_eq!((after.item_ix, after.offset_in_item), (before.item_ix, before.offset_in_item), "and did not drift down");

    // The wheel over the minimap strip scrolls the code too, instead of being swallowed by it.
    let strip = cx.debug_bounds("minimap").unwrap();
    let before = top(cx);
    wheel(cx, strip.center(), 0., -150.);
    draw(cx, &ws);
    assert!(top(cx).item_ix > before.item_ix, "scrolled from over the strip");
}

#[gpui::test]
async fn show_more_sits_in_the_gutter_and_full_view_gives_the_code_the_whole_window(cx: &mut TestAppContext) {
    let fx = bare_fixture("review-space");
    let text = |a: &str| -> String { (1..=60).map(|n| if n == 30 { format!("{a}\n") } else { format!("line {n}\n") }).collect() };
    commit_file(&fx, "r.txt", &text("before"), "base");
    commit_file(&fx, "r.txt", &text("after"), "change");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    ws.update(cx, |ws, cx| ws.set_review_layout(gitgui_store::ReviewLayout::Beside, cx));
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "change");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    draw(cx, &ws);

    // The button is by the line numbers, at the left edge of the code, not at the far end of the header row.
    let code = cx.debug_bounds("diff-list").unwrap();
    let more = center_of(cx, "hunk-up-0".to_owned());
    assert!(more.x - code.origin.x < px(60.), "beside the line numbers: {:?} vs {:?}", more.x, code.origin.x);

    // While reading, the code area is the biggest part of the window: no sidebar, no graph.
    let narrow_pane = code.size.width;
    let before_x = code.origin.x;
    ws.update(cx, |ws, cx| ws.toggle_expanded(cx));
    draw(cx, &ws);
    let wide = cx.debug_bounds("diff-list").unwrap();
    assert!(
        wide.origin.x < px(crate::workspace::FILES_WIDTH + 30.),
        "only the file list is left of the code: starts at {:?}",
        wide.origin.x
    );
    assert!(wide.size.width > narrow_pane * 1.3, "more room for the code in full view: {narrow_pane:?} → {:?}", wide.size.width);
    ws.update(cx, |ws, cx| ws.back(cx));
    draw(cx, &ws);
    assert_eq!(cx.debug_bounds("diff-list").unwrap().origin.x, before_x, "the sidebar and graph come back");
}

/// The arrows in the gutter show 20 more unchanged lines on the side they point to, and only there.
#[gpui::test]
async fn the_arrows_in_the_gutter_show_more_lines_above_or_below_a_hunk_only(cx: &mut TestAppContext) {
    let fx = bare_fixture("arrows-gutter");
    let text = |a: &str| -> String { (1..=60).map(|n| if n == 30 { format!("{a}\n") } else { format!("line {n}\n") }).collect() };
    commit_file(&fx, "r.txt", &text("before"), "base");
    commit_file(&fx, "r.txt", &text("after"), "change");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    ws.update(cx, |ws, cx| ws.set_review_layout(gitgui_store::ReviewLayout::Beside, cx));
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "change");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    draw(cx, &ws);

    // What is shown: the new side's first and last line number, and where lines are still hidden.
    let shown = |ws: &Entity<Workspace>, cx: &VisualTestContext| {
        ws.read_with(cx, |ws, _| {
            let file = ws.repo.as_ref().unwrap().file.as_ref().unwrap();
            let lines: Vec<_> = file.diff.hunks.iter().flat_map(|h| &h.lines).filter_map(|l| l.new_no).collect();
            (
                (lines.first().copied(), lines.last().copied()),
                file.above[0].map(|g| g.left),
                file.below.map(|g| g.left),
                file.rows.iter().any(|r| matches!(r, DisplayRow::Tail)),
            )
        })
    };
    // 3 lines around the change: 27..33 of 60 are shown, 26 are hidden above and 27 below.
    assert_eq!(shown(&ws, cx), ((Some(27), Some(33)), Some(26), Some(27), true));
    assert!(cx.debug_bounds("hunk-up-0").is_some() && cx.debug_bounds("tail-down-0").is_some());
    assert!(cx.debug_bounds("hunk-down-0").is_none(), "nothing is before the first hunk to show more under");

    // Up: 20 more lines over the hunk, none under it.
    let at = center_of(cx, "hunk-up-0".to_owned());
    click(cx, MouseButton::Left, at);
    draw(cx, &ws);
    assert_eq!(shown(&ws, cx), ((Some(7), Some(33)), Some(6), Some(27), true));

    // Down at the end: 20 more under it.
    let at = center_of(cx, "tail-down-0".to_owned());
    click(cx, MouseButton::Left, at);
    draw(cx, &ws);
    assert_eq!(shown(&ws, cx), ((Some(7), Some(53)), Some(6), Some(7), true));

    // What is left shows in full and the arrows go.
    let at = center_of(cx, "tail-down-0".to_owned());
    click(cx, MouseButton::Left, at);
    let at = center_of(cx, "hunk-up-0".to_owned());
    click(cx, MouseButton::Left, at);
    draw(cx, &ws);
    assert_eq!(shown(&ws, cx), ((Some(1), Some(60)), None, None, false));

    // The numbers are right on both sides, and a collapse puts everything back.
    let numbers = ws.read_with(cx, |ws, _| {
        let file = ws.repo.as_ref().unwrap().file.as_ref().unwrap();
        file.diff.hunks[0].lines.iter().all(|l| l.kind == gitgui_core::LineKind::Context && l.old_no == l.new_no || l.old_no.is_some() != l.new_no.is_some())
    });
    assert!(numbers, "the same line has the same number in both files here");
    ws.update(cx, |ws, cx| ws.set_diff_context(Some(25), cx));
    cx.run_until_parked();
    ws.update(cx, |ws, cx| ws.set_diff_context(None, cx));
    cx.run_until_parked();
    draw(cx, &ws);
    assert_eq!(shown(&ws, cx), ((Some(27), Some(33)), Some(26), Some(27), true));
}

#[gpui::test]
async fn hidden_panels_leave_a_strip_that_brings_them_back_and_the_file_list_resizes(cx: &mut TestAppContext) {
    use crate::workspace::{FILES_MAX, FILES_MIN, Panel};
    let fx = merged_pr("panels");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    // The pane beside the graph, where hiding the graph gives its room to the code.
    ws.update(cx, |ws, cx| ws.set_review_layout(gitgui_store::ReviewLayout::Beside, cx));
    open_project(&ws, cx, &fx.repo());
    select(&ws, cx, "feat: x2");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    draw(cx, &ws);
    let list_x = |cx: &mut VisualTestContext| cx.debug_bounds("diff-list").unwrap();
    let narrow = list_x(cx).size.width;

    // The file list's divider drags it wider and narrower, within limits.
    ws.update(cx, |ws, cx| ws.begin_resize(Splitter::Files, cx));
    let left = ws.read_with(cx, |ws, _| ws.files_left.get());
    drag_to(&ws, cx, left + 330., Some(MouseButton::Left));
    assert_eq!(ws.read_with(cx, |ws, _| ws.files_width), 330.);
    drag_to(&ws, cx, left + 5., Some(MouseButton::Left));
    assert_eq!(ws.read_with(cx, |ws, _| ws.files_width), FILES_MIN);
    drag_to(&ws, cx, left + 5000., Some(MouseButton::Left));
    assert_eq!(ws.read_with(cx, |ws, _| ws.files_width), FILES_MAX);
    ws.update(cx, |ws, cx| ws.end_resize(cx));
    ws.update(cx, |ws, cx| ws.begin_resize(Splitter::Files, cx));
    drag_to(&ws, cx, left + 230., Some(MouseButton::Left));
    ws.update(cx, |ws, cx| ws.end_resize(cx));

    // Hide the graph: the file pane takes the width, a strip stands where the graph was, and pointing at the
    // strip slides the graph in over the code until the pointer leaves.
    ws.update(cx, |ws, cx| ws.toggle_graph_hidden(cx));
    draw(cx, &ws);
    assert!(list_x(cx).size.width > narrow * 1.25, "the code has the graph's room too");
    assert!(cx.debug_bounds("graph-rail").is_some(), "a strip stands where the graph was");
    assert_eq!(ws.read_with(cx, |ws, _| ws.peek), None);
    ws.update(cx, |ws, cx| ws.peek_panel(Panel::Graph, cx));
    draw(cx, &ws);
    assert!(cx.debug_bounds("graph-peek").is_some(), "the graph is shown over the code");
    ws.update(cx, |ws, cx| ws.unpeek(Panel::Graph, cx));
    assert_eq!(ws.read_with(cx, |ws, _| ws.peek), None);
    // Unpeeking something that is not showing does nothing.
    ws.update(cx, |ws, cx| ws.peek_panel(Panel::Files, cx));
    ws.update(cx, |ws, cx| ws.unpeek(Panel::Graph, cx));
    assert_eq!(ws.read_with(cx, |ws, _| ws.peek), Some(Panel::Files));
    ws.update(cx, |ws, cx| ws.toggle_graph_hidden(cx));
    assert_eq!(ws.read_with(cx, |ws, _| ws.peek), None, "showing a panel puts the peek away");

    // The same for the file list and the sidebar.
    ws.update(cx, |ws, cx| ws.toggle_files_visible(cx));
    ws.update(cx, |ws, cx| ws.toggle_sidebar(cx));
    draw(cx, &ws);
    assert!(cx.debug_bounds("files-rail").is_some() && cx.debug_bounds("sidebar-rail").is_some());
    ws.update(cx, |ws, cx| ws.peek_panel(Panel::Files, cx));
    draw(cx, &ws);
    assert!(cx.debug_bounds("files-peek").is_some());
    ws.update(cx, |ws, cx| ws.peek_panel(Panel::Sidebar, cx));
    draw(cx, &ws);
    assert!(cx.debug_bounds("sidebar-peek").is_some());
}

#[gpui::test]
async fn the_pane_below_the_graph_has_a_divider_that_resizes_it_within_limits(cx: &mut TestAppContext) {
    use crate::workspace::{GRAPH_HEIGHT_MIN, PANE_HEIGHT_MIN};
    let fx = merged_pr("pane-height");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    assert_eq!(ws.read_with(cx, |ws, _| ws.settings.review_layout), gitgui_store::ReviewLayout::Below, "the default");
    select(&ws, cx, "feat: x2");
    ws.update(cx, |ws, cx| ws.open_file(0, cx));
    cx.run_until_parked();
    draw(cx, &ws);
    assert!(cx.debug_bounds("pane-height-divider").is_some(), "a horizontal divider between the graph and the pane");

    let total = cx.update(|window, _| f32::from(window.viewport_size().height));
    let move_to = |cx: &mut VisualTestContext, y: f32| {
        let event = MouseMoveEvent { position: point(px(900.), px(y)), pressed_button: Some(MouseButton::Left), modifiers: Modifiers::default() };
        cx.update(|window, app| ws.update(app, |ws, cx| ws.drag_divider(&event, window, cx)));
    };
    ws.update(cx, |ws, cx| ws.begin_resize(Splitter::PaneHeight, cx));
    move_to(cx, total - 300.);
    assert_eq!(ws.read_with(cx, |ws, _| ws.pane_height), Some(300.), "the pane runs from the pointer to the bottom");
    move_to(cx, total - 5.);
    assert_eq!(ws.read_with(cx, |ws, _| ws.pane_height), Some(PANE_HEIGHT_MIN), "not smaller than it can be read");
    move_to(cx, 5.);
    assert_eq!(ws.read_with(cx, |ws, _| ws.pane_height), Some(total - GRAPH_HEIGHT_MIN), "and the graph keeps some rows");
    ws.update(cx, |ws, cx| ws.end_resize(cx));
    draw(cx, &ws);

    // Beside the graph there is no such divider.
    ws.update(cx, |ws, cx| ws.set_review_layout(gitgui_store::ReviewLayout::Beside, cx));
    assert_eq!(Store::at(fx.data()).settings().unwrap().review_layout, gitgui_store::ReviewLayout::Beside, "kept");
}

/// A commit by `who` on `day` (noon UTC, so it is the same day wherever the test runs).
fn commit_as(fx: &Fixture, who: (&str, &str), day: &str, file: &str, message: &str) {
    fx.write(file, message);
    fx.git(&["add", "."]);
    let date = format!("{day}T12:00:00 +0000");
    let ok = Command::new("git")
        .arg("-C")
        .arg(fx.repo())
        .args(["-c", "commit.gpgsign=false", "commit", "-q", "-m", message])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", who.0)
        .env("GIT_AUTHOR_EMAIL", who.1)
        .env("GIT_COMMITTER_NAME", who.0)
        .env("GIT_COMMITTER_EMAIL", who.1)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .status()
        .unwrap()
        .success();
    assert!(ok, "commit {message}");
}

#[gpui::test]
async fn the_graph_can_be_limited_to_an_author_and_to_days_from_the_search_box_and_the_menus(cx: &mut TestAppContext) {
    use crate::menu::Action;
    let fx = bare_fixture("by-author-date");
    let (ada, bob) = (("Ada Lovelace", "ada@example.com"), ("Bob Barker", "bob@example.com"));
    commit_as(&fx, ada, "2026-10-01", "a.txt", "one by ada");
    commit_as(&fx, bob, "2026-10-02", "b.txt", "two by bob");
    commit_as(&fx, ada, "2026-10-05", "c.txt", "three by ada");
    commit_as(&fx, bob, "2026-10-06", "d.txt", "four by bob");

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let shown = |cx: &VisualTestContext| -> Vec<String> {
        ws.read_with(cx, |ws, _| {
            let Phase::Ready(view) = &ws.repo.as_ref().unwrap().phase else { panic!("not ready") };
            view.entries.iter().filter(|e| e.commit.is_some()).map(|e| e.summary.to_string()).collect()
        })
    };
    let search = |ws: &Entity<Workspace>, cx: &mut VisualTestContext, text: &str| {
        ws.update(cx, |ws, cx| ws.set_search(text.to_owned(), cx));
        draw(cx, ws);
    };
    assert_eq!(shown(cx).len(), 4);

    // An author, by part of the name: the others are not in the graph at all.
    search(&ws, cx, "author:bob");
    assert_eq!(shown(cx), ["four by bob", "two by bob"]);
    assert!(ws.read_with(cx, |ws, _| match &ws.repo.as_ref().unwrap().phase {
        Phase::Ready(view) => view.timing.contains("2 of 4 commits"),
        _ => false,
    }), "the count says what is shown");

    // Days: a range with an open end, one day, a month.
    search(&ws, cx, "date:2026-10-05..");
    assert_eq!(shown(cx), ["four by bob", "three by ada"]);
    search(&ws, cx, "date:2026-10-02");
    assert_eq!(shown(cx), ["two by bob"]);
    search(&ws, cx, "date:2026-10");
    assert_eq!(shown(cx).len(), 4);
    search(&ws, cx, "date:2026-09");
    assert!(shown(cx).is_empty(), "no commit in September: an empty graph, not a crash");

    // Both together, and words besides them still mark rows.
    search(&ws, cx, "author:ada date:2026-10-02..");
    assert_eq!(shown(cx), ["three by ada"]);
    search(&ws, cx, "author:ada two");
    assert_eq!(shown(cx), ["three by ada", "one by ada"]);

    // Half a date, while it is typed, limits nothing.
    search(&ws, cx, "date:2026-1");
    assert_eq!(shown(cx).len(), 4);

    // The menus write the same words in the box, keeping what else is typed there.
    search(&ws, cx, "");
    ws.update(cx, |ws, cx| ws.set_search_term("author", Some("bob@example.com".into()), cx));
    assert_eq!(ws.read_with(cx, |ws, app| ws.search_input.read(app).text().to_owned()), "author:bob@example.com");
    assert_eq!(shown(cx), ["four by bob", "two by bob"]);
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::FilterDate(Some("7d".into())), window, cx));
    let text = ws.read_with(cx, |ws, app| ws.search_input.read(app).text().to_owned());
    assert_eq!(text, "author:bob@example.com date:7d");
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::FilterAuthor(None), window, cx));
    let text = ws.read_with(cx, |ws, app| ws.search_input.read(app).text().to_owned());
    assert_eq!(text, "date:7d", "Anyone takes the author out and leaves the days");

    // The chips are in the filter bar and the menus list the people and the presets.
    draw(cx, &ws);
    assert!(cx.debug_bounds("filter-author").is_some() && cx.debug_bounds("filter-date").is_some());
    let people: Vec<String> = ws.read_with(cx, |ws, _| ws.menu_items(&crate::menu::MenuTarget::Authors).iter().map(|i| i.label.to_string()).collect());
    assert!(people.iter().any(|l| l.contains("Ada Lovelace")) && people.iter().any(|l| l.contains("Bob Barker")), "{people:?}");
    let dates: Vec<String> = ws.read_with(cx, |ws, _| ws.menu_items(&crate::menu::MenuTarget::Dates).iter().map(|i| i.label.to_string()).collect());
    assert!(dates.iter().any(|l| l.starts_with('✓') && l.contains("Last 7 days")), "the chosen preset is ticked: {dates:?}");
}

// ---- the remote: ahead and behind, Push, fetching on its own -------------------------------------------

/// Gives the fixture a bare `origin` with `main` pushed to it and tracked; returns the remote's folder.
fn with_origin(fx: &Fixture) -> PathBuf {
    let remote = fx.0.join("remote.git");
    fx.git(&["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    fx.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    fx.git(&["push", "-q", "-u", "origin", "main"]);
    remote
}

/// A colleague's clone of `remote`, who commits `file` on `branch` and pushes it.
fn colleague_pushes(fx: &Fixture, remote: &Path, branch: &str, file: &str) {
    let other = fx.0.join("other");
    if !other.exists() {
        git_as_bo(&fx.0, &["clone", "-q", remote.to_str().unwrap(), other.to_str().unwrap()]);
    }
    git_as_bo(&other, &["fetch", "-q", "origin"]);
    git_as_bo(&other, &["switch", "-q", "-C", branch, &format!("origin/{branch}")]);
    std::fs::write(other.join(file), "theirs\n").unwrap();
    git_as_bo(&other, &["add", "."]);
    git_as_bo(&other, &["commit", "-q", "-m", file]);
    git_as_bo(&other, &["push", "-q", "origin", branch]);
}

fn upstream_now(ws: &Entity<Workspace>, cx: &VisualTestContext) -> Option<gitgui_core::Upstream> {
    ws.read_with(cx, |ws, _| match ws.repo.as_ref().map(|r| &r.phase) {
        Some(Phase::Ready(view)) => view.upstream().cloned(),
        _ => None,
    })
}

fn dialog_text(ws: &Entity<Workspace>, cx: &VisualTestContext) -> (String, String, String) {
    ws.read_with(cx, |ws, _| {
        let d = ws.dialog.as_ref().expect("a question is asked first");
        (d.title.to_string(), d.body.to_string(), d.confirm.to_string())
    })
}

#[gpui::test]
async fn the_header_says_where_the_branch_stands_against_its_remote_and_push_asks_then_publishes(cx: &mut TestAppContext) {
    use crate::workspace::{Emphasis, sync_emphasis};
    use gitgui_core::Upstream;
    let fx = bare_fixture("push-button");
    commit_file(&fx, "a.txt", "base\n", "base");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);
    // No remote: nothing to fetch, pull or push, and nothing to be ahead of.
    assert!(cx.debug_bounds("push").is_none() && cx.debug_bounds("fetch").is_none());
    assert!(cx.debug_bounds("branch-upstream").is_none());

    let remote = with_origin(&fx);
    ws.update(cx, |ws, cx| ws.refresh(cx));
    cx.run_until_parked();
    draw(cx, &ws);
    assert!(cx.debug_bounds("push").is_some() && cx.debug_bounds("branch-upstream").is_some());
    let in_step = Upstream::Tracking { name: "origin/main".into(), remote: "origin".into(), ahead: 0, behind: 0 };
    assert_eq!(upstream_now(&ws, cx), Some(in_step.clone()));
    assert_eq!(sync_emphasis(Some(&in_step)), (Emphasis::Plain, Emphasis::Plain), "in step: both quiet");

    // A commit of ours: one ahead, and Push is the thing to do.
    commit_file(&fx, "mine.txt", "m\n", "mine");
    ws.update(cx, |ws, cx| ws.refresh(cx));
    cx.run_until_parked();
    let ahead = upstream_now(&ws, cx).unwrap();
    assert_eq!((ahead.ahead(), ahead.behind()), (1, 0));
    assert_eq!(sync_emphasis(Some(&ahead)), (Emphasis::Plain, Emphasis::Lit));

    // Clicking Push only asks, and says where it goes.
    draw(cx, &ws);
    let at = center_of(cx, "push".to_owned());
    click(cx, MouseButton::Left, at);
    let (title, body, confirm) = dialog_text(&ws, cx);
    assert_eq!((title.as_str(), confirm.as_str()), ("Push main to origin?", "Push"));
    assert!(body.contains("Sends 1 commit") && body.contains("Nothing is forced"), "{body}");
    assert!(git_as_bo(&remote, &["log", "--format=%s", "main"]).starts_with("base"), "nothing is pushed yet");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    assert!(git_as_bo(&remote, &["log", "--format=%s", "main"]).starts_with("mine"), "now it is");
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| !n.warn && n.text.contains("Pushed main"))));
    assert_eq!(upstream_now(&ws, cx), Some(in_step));

    // A colleague pushes: after a fetch we are one behind, Pull is the thing to do, and a push would be refused.
    colleague_pushes(&fx, &remote, "main", "theirs.txt");
    ws.update(cx, |ws, cx| ws.fetch(cx));
    cx.run_until_parked();
    let behind = upstream_now(&ws, cx).unwrap();
    assert_eq!((behind.ahead(), behind.behind()), (0, 1));
    assert_eq!(sync_emphasis(Some(&behind)), (Emphasis::Lit, Emphasis::Plain));
    with_window(&ws, cx, |ws, window, cx| ws.push_current(window, cx));
    assert!(dialog_text(&ws, cx).1.contains("will be rejected: Pull first"));
    ws.update(cx, |ws, cx| ws.cancel_dialog(cx));
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::PullRebase, window, cx));
    assert!(dialog_text(&ws, cx).1.starts_with("`origin/main` has 1 commit that `main` does not."));
    ws.update(cx, |ws, cx| ws.cancel_dialog(cx));

    // A branch made here is not on a remote yet: Push publishes it, and then it tracks its remote branch.
    fx.git(&["switch", "-q", "-c", "feature/new"]);
    ws.update(cx, |ws, cx| ws.refresh(cx));
    cx.run_until_parked();
    assert_eq!(upstream_now(&ws, cx), Some(Upstream::None));
    assert_eq!(sync_emphasis(Some(&Upstream::None)), (Emphasis::Idle, Emphasis::Lit));
    with_window(&ws, cx, |ws, window, cx| ws.push_current(window, cx));
    let (title, body, confirm) = dialog_text(&ws, cx);
    assert_eq!((title.as_str(), confirm.as_str()), ("Publish feature/new to origin?", "Publish"));
    assert!(body.contains("`origin/feature/new`"), "{body}");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    assert!(!git_as_bo(&remote, &["branch", "--list", "feature/new"]).trim().is_empty(), "it is on the remote");
    assert!(matches!(upstream_now(&ws, cx), Some(Upstream::Tracking { ref name, ahead: 0, behind: 0, .. }) if name == "origin/feature/new"));
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.text.contains("Published feature/new to origin"))));

    // Its remote branch deleted and pruned: gone, and Push puts it back.
    git_as_bo(&fx.0.join("other"), &["push", "-q", "origin", "--delete", "feature/new"]);
    ws.update(cx, |ws, cx| ws.fetch(cx));
    cx.run_until_parked();
    assert!(matches!(upstream_now(&ws, cx), Some(Upstream::Gone { .. })));
    with_window(&ws, cx, |ws, window, cx| ws.push_current(window, cx));
    assert_eq!(dialog_text(&ws, cx).0, "Push feature/new to origin again?");
    ws.update(cx, |ws, cx| ws.cancel_dialog(cx));
    // And Pull says why there is nothing to pull, without asking first.
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::PullRebase, window, cx));
    assert!(ws.read_with(cx, |ws, _| ws.dialog.is_none()));
    let said = ws.read_with(cx, |ws, _| ws.notice.as_ref().map(|n| n.text.to_string())).unwrap_or_default();
    assert!(said.contains("origin/feature/new was deleted"), "{said}");

    // A detached HEAD has no branch to push.
    fx.git(&["switch", "-q", "--detach", "main"]);
    ws.update(cx, |ws, cx| ws.refresh(cx));
    cx.run_until_parked();
    assert_eq!(upstream_now(&ws, cx), None);
    assert_eq!(sync_emphasis(None), (Emphasis::Idle, Emphasis::Idle));
    with_window(&ws, cx, |ws, window, cx| ws.push_current(window, cx));
    assert!(ws.read_with(cx, |ws, _| ws.dialog.is_none() && ws.notice.as_ref().is_some_and(|n| n.text.contains("detached HEAD"))));
    draw(cx, &ws);
}

/// A local branch the remote has moved on from is marked in the list of branches to start from and in the New branch
/// window, which offers its remote branch instead.
#[gpui::test]
async fn a_branch_behind_its_remote_is_marked_where_new_work_starts_from(cx: &mut TestAppContext) {
    let fx = bare_fixture("start-behind");
    commit_file(&fx, "a.txt", "base\n", "base");
    let remote = with_origin(&fx);
    fx.git(&["switch", "-q", "-c", "develop"]);
    fx.git(&["push", "-q", "-u", "origin", "develop"]);
    fx.git(&["switch", "-q", "main"]);
    colleague_pushes(&fx, &remote, "develop", "one.txt");
    colleague_pushes(&fx, &remote, "develop", "two.txt");

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let develop_behind = |ws: &Entity<Workspace>, cx: &VisualTestContext| {
        ws.read_with(cx, |ws, _| {
            ws.branch_pick_rows("develop").into_iter().find_map(|row| match row {
                crate::workflow_ui::PickRow::Branch { name, behind, .. } if name == "develop" => Some(behind),
                _ => None,
            })
        })
        .expect("develop is listed")
    };
    assert_eq!(develop_behind(&ws, cx), None, "not known to be behind before a fetch");
    ws.update(cx, |ws, cx| ws.fetch(cx));
    cx.run_until_parked();
    let behind = develop_behind(&ws, cx).expect("two behind after the fetch");
    assert_eq!((behind.count, behind.upstream.as_str(), behind.remote.as_str()), (2, "origin/develop", "origin"));
    // The remote branch itself is not marked.
    let remote_row = ws.read_with(cx, |ws, _| {
        ws.branch_pick_rows("origin/develop").into_iter().any(|row| matches!(row, crate::workflow_ui::PickRow::Branch { name, behind: None, .. } if name == "origin/develop"))
    });
    assert!(remote_row);

    // Starting from it in the New branch window says so, and one click starts from the remote branch instead.
    ws.update_in(cx, |ws, window, cx| ws.open_new_branch(window, cx));
    ws.update(cx, |ws, cx| ws.set_branch_base("develop", cx));
    draw(cx, &ws);
    let at = center_of(cx, "branch-base-upstream".to_owned());
    click(cx, MouseButton::Left, at);
    assert_eq!(ws.read_with(cx, |ws, _| ws.new_branch.as_ref().map(|w| w.base.clone())), Some("origin/develop".to_owned()));
    // The list draws its note too.
    with_window(&ws, cx, |ws, window, cx| ws.open_branch_picker(window, cx));
    draw(cx, &ws);
}

/// Fetching on its own: off by default; when on, it fetches the open project in the background without a banner,
/// keeps what is selected, reads the repository again only when something came in, waits for an operation that is
/// running, and says so only when it fails twice in a row.
#[gpui::test]
async fn fetching_on_its_own_brings_in_new_branches_quietly_and_only_rereads_when_something_changed(cx: &mut TestAppContext) {
    use crate::workspace::AUTO_FETCH_TICK;
    use std::time::Duration;
    let fx = bare_fixture("auto-fetch");
    commit_file(&fx, "a.txt", "base\n", "base");
    commit_file(&fx, "b.txt", "next\n", "next");
    let remote = with_origin(&fx);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    assert!(!ws.read_with(cx, |ws, _| ws.auto_fetch_scheduled()), "off until chosen");
    select(&ws, cx, "base");
    let selected = |cx: &VisualTestContext| ws.read_with(cx, |ws, _| ws.repo.as_ref().unwrap().selected_id());
    let base_id = selected(cx);

    // Chosen in Settings, by a click.
    ws.update(cx, |ws, cx| {
        ws.open_settings(cx);
        ws.set_settings_page(crate::settings_view::SettingsPage::Projects, cx);
    });
    draw(cx, &ws);
    let at = center_of(cx, "setting-auto-fetch-5".to_owned());
    click(cx, MouseButton::Left, at);
    ws.update(cx, |ws, cx| ws.close_settings(cx));
    assert!(ws.read_with(cx, |ws, _| ws.auto_fetch_scheduled()));
    assert_eq!(Store::at(fx.data()).settings().unwrap().auto_fetch_minutes, 5, "kept for the next start");

    let fetches = |cx: &VisualTestContext| count(&ws, cx, |c| &c.fetches);
    let reads = |cx: &VisualTestContext| count(&ws, cx, |c| &c.repo_reads);
    let wait = |cx: &mut VisualTestContext, by: Duration| {
        cx.executor().advance_clock(by);
        cx.run_until_parked();
    };

    // A colleague pushes a branch; the first look after it is turned on fetches it, without a banner.
    git_as_bo(&fx.0, &["clone", "-q", remote.to_str().unwrap(), fx.0.join("other").to_str().unwrap()]);
    colleague_pushes(&fx, &remote, "main", "theirs.txt");
    let before = reads(cx);
    wait(cx, AUTO_FETCH_TICK);
    assert_eq!(fetches(cx), 1);
    assert_eq!(reads(cx), before + 1, "something came in, so the repository was read again");
    assert_eq!(upstream_now(&ws, cx).map(|u| u.behind()), Some(1), "and it shows");
    assert!(ws.read_with(cx, |ws, _| ws.notice.is_none() && ws.busy.is_none()), "quietly");
    assert_eq!(selected(cx), base_id, "what was selected stays selected");

    // Not again until five minutes have passed; then nothing new, so nothing is read again.
    wait(cx, AUTO_FETCH_TICK);
    assert_eq!(fetches(cx), 1);
    let before = reads(cx);
    wait(cx, Duration::from_secs(5 * 60));
    assert_eq!(fetches(cx), 2);
    assert_eq!(reads(cx), before, "nothing changed, nothing read");

    // Never while an operation runs.
    ws.update(cx, |ws, _| ws.busy = Some("Pulling…".into()));
    wait(cx, Duration::from_secs(10 * 60));
    assert_eq!(fetches(cx), 2);
    ws.update(cx, |ws, _| ws.busy = None);

    // The remote goes away: the first failure is not said, the second is, once.
    fx.git(&["remote", "set-url", "origin", fx.0.join("nowhere.git").to_str().unwrap()]);
    wait(cx, AUTO_FETCH_TICK);
    assert_eq!(fetches(cx), 3);
    assert!(ws.read_with(cx, |ws, _| ws.notice.is_none()), "one failure is not worth a banner");
    wait(cx, Duration::from_secs(5 * 60));
    assert_eq!(fetches(cx), 4);
    let said = ws.read_with(cx, |ws, _| ws.notice.as_ref().map(|n| (n.warn, n.text.to_string())));
    assert!(said.as_ref().is_some_and(|(warn, text)| *warn && text.contains("failed twice")), "{said:?}");
    ws.update(cx, |ws, cx| ws.dismiss_notice(cx));
    wait(cx, Duration::from_secs(5 * 60));
    assert_eq!(fetches(cx), 5);
    assert!(ws.read_with(cx, |ws, _| ws.notice.is_none()), "said once, not every time");

    // Off stops it.
    ws.update(cx, |ws, cx| ws.set_auto_fetch(0, cx));
    assert!(!ws.read_with(cx, |ws, _| ws.auto_fetch_scheduled()));
    wait(cx, Duration::from_secs(30 * 60));
    assert_eq!(fetches(cx), 5);
}

#[test]
fn the_header_gives_up_its_words_before_the_counts_as_it_narrows() {
    use crate::workspace::{HeaderRoom, header_room};
    let name = "feature/88-offline-mode";
    // "↑2" and "origin"; "4 ahead · 7 behind origin/develop".
    let (upstream, base) = (Some((2, 7)), Some(33));
    let room = |area: f32| header_room(area, name, upstream, base);
    assert_eq!(room(1100.), HeaderRoom { label: true, base: true, remote: true }, "room for everything");
    assert_eq!(room(950.), HeaderRoom { label: false, base: true, remote: true }, "the words go first");
    assert_eq!(room(720.), HeaderRoom { label: false, base: false, remote: true }, "then what it was cut from");
    assert_eq!(room(560.), HeaderRoom { label: false, base: false, remote: false }, "then the remote's name; the counts stay");
    // A short name leaves room for more.
    assert!(header_room(780., "main", upstream, base).base && !room(780.).base);
    // Nothing to show is never shown.
    assert!(!header_room(2000., name, None, None).base);
}

#[test]
fn the_push_question_names_where_the_push_goes() {
    use crate::menu::push_question;
    use gitgui_core::Upstream;
    let origin = ["origin".to_owned()];
    // A branch that tracks a remote branch of another name is still pushed to its own name, and the question says so.
    let other = Upstream::Tracking { name: "origin/develop".into(), remote: "origin".into(), ahead: 1, behind: 0 };
    let (title, body, confirm) = push_question("feat", &other, &origin);
    assert_eq!((title.as_str(), confirm), ("Push feat to origin/feat?", "Push"));
    assert!(body.contains("Its upstream stays `origin/develop`"), "{body}");
    // In step: nothing to send, said before anything is sent.
    let same = Upstream::Tracking { name: "origin/feat".into(), remote: "origin".into(), ahead: 0, behind: 0 };
    assert!(push_question("feat", &same, &origin).1.contains("nothing new to send"));
    // Not on a remote: published to origin, else the first remote; with none, it says so.
    assert_eq!(push_question("feat", &Upstream::None, &["fork".to_owned()]).0, "Publish feat to fork?");
    assert!(push_question("feat", &Upstream::None, &[]).1.contains("no remote"));
}


// ---- resolving conflicts ---------------------------------------------------------------------------

/// `main` and `other` both change `config.txt` in three places (the middle one only spaced differently), and `other`
/// only re-indents `style.txt` where `main` changes it.
fn conflicting_branches(name: &str) -> Fixture {
    let fx = bare_fixture(name);
    fx.write("config.txt", "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\n");
    fx.write("style.txt", "a {\n  color: black;\n}\n");
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "base"]);
    fx.git(&["checkout", "-q", "-b", "other"]);
    fx.write("config.txt", "one\ntwo other\nthree\nfour\nFIVE   \nsix\nseven\neight other\nnine\n");
    fx.write("style.txt", "a {\n    color: black;\n}\n");
    fx.git(&["commit", "-q", "-am", "feat: other's way"]);
    fx.git(&["checkout", "-q", "main"]);
    fx.write("config.txt", "one\ntwo main\nthree\nfour\nFIVE\nsix\nseven\neight main\nnine\n");
    fx.write("style.txt", "a {\n  color: navy;\n}\n");
    fx.git(&["commit", "-q", "-am", "feat: main's way"]);
    fx
}

fn resolver_state(ws: &Entity<Workspace>, cx: &VisualTestContext) -> Option<(String, Vec<Option<gitgui_core::Resolution>>, bool)> {
    ws.read_with(cx, |ws, _| {
        let r = ws.resolver()?;
        Some((r.path.clone(), r.choices.clone(), r.text().is_some_and(|t| t.pristine)))
    })
}

fn click_on(cx: &mut VisualTestContext, ws: &Entity<Workspace>, name: &str) {
    draw(cx, ws);
    let at = center_of(cx, name.to_owned());
    click(cx, MouseButton::Left, at);
    cx.run_until_parked();
}

#[gpui::test]
async fn a_merge_conflict_is_resolved_in_the_app_with_clicks_and_keys_and_continued(cx: &mut TestAppContext) {
    use gitgui_core::Resolution;
    let fx = conflicting_branches("resolve-merge");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());

    // The question says, from a test merge, which files would conflict.
    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::Merge("other".into()), window, cx));
    let check = ws.read_with(cx, |ws, _| ws.dialog.as_ref().and_then(|d| d.check.clone()));
    assert_eq!(check, Some(crate::menu::Check::Running), "the question opens before the test merge is done");
    cx.run_until_parked();
    let check = ws.read_with(cx, |ws, _| ws.dialog.as_ref().and_then(|d| d.check.clone())).unwrap();
    assert_eq!(check, crate::menu::Check::Found(gitgui_core::Preflight::Conflicts(vec!["config.txt".into(), "style.txt".into()])));
    let said = crate::menu::check_line(&Action::Merge("other".into()), &check).0;
    assert!(said.starts_with("Would conflict in 2 files: config.txt, style.txt"), "{said}");
    draw(cx, &ws);
    assert!(cx.debug_bounds("dialog-check").is_some());
    assert_eq!(GitCli::new(fx.repo()).in_progress(), None, "the test merge touched nothing");

    // Merging stops on the conflicts: the first file opens in the resolver, with the graph out of the way and the bar
    // over the main area.
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    let (path, choices, pristine) = resolver_state(&ws, cx).expect("the resolver opened");
    assert_eq!((path.as_str(), choices.len(), pristine), ("config.txt", 3, true));
    assert!(ws.read_with(cx, |ws, _| ws.graph_hidden));
    let headline = ws.read_with(cx, |ws, _| ws.operation().map(|op| (op.headline(), op.current.title.clone(), op.incoming.title.clone())));
    assert_eq!(headline, Some(("Merging other into main".into(), "On main".into(), "Coming in from other".into())));
    draw(cx, &ws);
    assert!(cx.debug_bounds("operation-bar").is_some() && cx.debug_bounds("conflict-story").is_some());

    // A click on a side; the safe one with its button; the keys for the last.
    click_on(cx, &ws, "choose-0-incoming");
    assert_eq!(resolver_state(&ws, cx).unwrap().1, [Some(Resolution::Incoming), None, None]);
    click_on(cx, &ws, "conflict-safe");
    assert_eq!(resolver_state(&ws, cx).unwrap().1, [Some(Resolution::Incoming), Some(Resolution::Current), None]);
    draw(cx, &ws);
    assert!(cx.debug_bounds("conflict-why-1").is_some(), "the safe one says why");
    cx.simulate_keystrokes("n n 1");
    assert_eq!(resolver_state(&ws, cx).unwrap().1[2], Some(Resolution::Current));
    cx.simulate_keystrokes("backspace");
    assert_eq!(resolver_state(&ws, cx).unwrap().1[2], None, "Backspace takes the choice back");
    cx.simulate_keystrokes("1");
    assert!(std::fs::read_to_string(fx.repo().join("config.txt")).unwrap().contains("<<<<<<<"), "nothing is written until asked");

    // Use this result: written, marked resolved, and the next file opens.
    click_on(cx, &ws, "conflict-use");
    assert_eq!(std::fs::read_to_string(fx.repo().join("config.txt")).unwrap(), "one\ntwo other\nthree\nfour\nFIVE\nsix\nseven\neight main\nnine\n");
    assert_eq!(GitCli::new(fx.repo()).unmerged().unwrap().len(), 1);
    assert_eq!(resolver_state(&ws, cx).map(|r| r.0).as_deref(), Some("style.txt"));

    // style.txt: other only re-indented; the safe resolution takes main's change.
    click_on(cx, &ws, "conflict-safe");
    click_on(cx, &ws, "conflict-use");
    assert_eq!(std::fs::read_to_string(fx.repo().join("style.txt")).unwrap(), "a {\n  color: navy;\n}\n");
    assert!(resolver_state(&ws, cx).is_none(), "nothing is left to resolve");
    draw(cx, &ws);
    assert!(cx.debug_bounds("work-overview").is_some());

    // Continue finishes the merge; the bar goes and the graph comes back.
    click_on(cx, &ws, "operation-continue");
    assert_eq!(GitCli::new(fx.repo()).in_progress(), None);
    let log = git_as_bo(&fx.repo(), &["log", "-1", "--format=%s %p"]);
    assert!(log.starts_with("Merge branch 'other'") && log.split_whitespace().count() >= 5, "a merge commit: {log}");
    assert!(ws.read_with(cx, |ws, _| ws.operation().is_none() && !ws.graph_hidden));
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.text.contains("Finished the merge"))));
    draw(cx, &ws);
}

#[gpui::test]
async fn a_file_edited_by_hand_is_never_written_over_and_start_over_puts_back_the_conflict(cx: &mut TestAppContext) {
    let fx = conflicting_branches("resolve-edited");
    let _ = GitCli::new(fx.repo()).merge("other").unwrap();
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);
    click_on(cx, &ws, "operation-resolve");
    click_on(cx, &ws, "choose-0-current");

    // Edited in an editor meanwhile: Use this result refuses and says why; the edit stays.
    let edited = std::fs::read_to_string(fx.repo().join("config.txt")).unwrap().replace("two main", "two by hand");
    std::fs::write(fx.repo().join("config.txt"), &edited).unwrap();
    click_on(cx, &ws, "conflict-use");
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.warn && n.text.contains("changed on disk"))));
    assert_eq!(std::fs::read_to_string(fx.repo().join("config.txt")).unwrap(), edited);

    // Re-read shows the file as it is, no longer git's own conflict.
    click_on(cx, &ws, "conflict-reread");
    let (_, _, pristine) = resolver_state(&ws, cx).unwrap();
    assert!(!pristine);
    draw(cx, &ws);
    assert!(cx.debug_bounds("conflict-note").is_some());

    // Start over asks, since it replaces the edit, then puts back the conflict from the index.
    click_on(cx, &ws, "conflict-start-over");
    assert_eq!(dialog_text(&ws, cx).0, "Start over with config.txt?");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    let (_, choices, pristine) = resolver_state(&ws, cx).unwrap();
    assert!(pristine && choices.iter().all(Option::is_none) && choices.len() == 3);
    let text = std::fs::read_to_string(fx.repo().join("config.txt")).unwrap();
    assert!(text.contains("<<<<<<< On main\n") && text.contains(">>>>>>> Coming in from other\n") && !text.contains("by hand"), "{text}");

    // Finished by hand in an editor: Re-read finds no markers left and offers to mark it resolved as it is.
    click_on(cx, &ws, "conflict-edit");
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.text.contains("Opened config.txt in your editor"))));
    std::fs::write(fx.repo().join("config.txt"), "resolved by hand\n").unwrap();
    click_on(cx, &ws, "conflict-reread");
    assert_eq!(resolver_state(&ws, cx).map(|r| r.1.len()), Some(0), "no conflicts left to choose in");
    draw(cx, &ws);
    assert!(cx.debug_bounds("conflict-note").is_some());
    click_on(cx, &ws, "conflict-use");
    assert_eq!(GitCli::new(fx.repo()).unmerged().unwrap().len(), 1, "config.txt is resolved; style.txt is left");
    assert_eq!(std::fs::read_to_string(fx.repo().join("config.txt")).unwrap(), "resolved by hand\n");
}

#[gpui::test]
async fn a_rebase_that_stops_twice_is_resolved_and_finished_from_the_bar(cx: &mut TestAppContext) {
    let fx = bare_fixture("resolve-rebase");
    commit_file(&fx, "f.txt", "one\ntwo\nthree\n", "base");
    fx.git(&["checkout", "-q", "-b", "feature"]);
    commit_file(&fx, "f.txt", "one F\ntwo\nthree\n", "feat: first");
    commit_file(&fx, "f.txt", "one F\ntwo\nthree F\n", "feat: second");
    fx.git(&["checkout", "-q", "main"]);
    commit_file(&fx, "f.txt", "one M\ntwo\nthree M\n", "main: both");
    fx.git(&["checkout", "-q", "feature"]);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());

    with_window(&ws, cx, |ws, window, cx| ws.choose(Action::Rebase("main".into()), window, cx));
    cx.run_until_parked();
    let check = ws.read_with(cx, |ws, _| ws.dialog.as_ref().and_then(|d| d.check.clone())).unwrap();
    assert!(crate::menu::check_line(&Action::Rebase("main".into()), &check).0.starts_with("May conflict in 1 file: f.txt"), "only a hint for a rebase");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();

    for (step, mine) in [(1, "Your commit: feat: first"), (2, "Your commit: feat: second")] {
        let state = ws.read_with(cx, |ws, _| ws.operation().map(|op| (op.step, op.current.title.clone(), op.incoming.title.clone())));
        assert_eq!(state, Some((Some((step, 2)), "Already on main".into(), mine.into())), "at commit {step}");
        assert_eq!(resolver_state(&ws, cx).map(|r| r.0).as_deref(), Some("f.txt"), "the stop's file is open");
        click_on(cx, &ws, "choose-0-incoming");
        click_on(cx, &ws, "conflict-use");
        click_on(cx, &ws, "operation-continue");
    }
    assert_eq!(GitCli::new(fx.repo()).in_progress(), None);
    assert_eq!(git_as_bo(&fx.repo(), &["log", "--format=%s"]), "feat: second\nfeat: first\nmain: both\nbase\n");
    assert_eq!(std::fs::read_to_string(fx.repo().join("f.txt")).unwrap(), "one F\ntwo\nthree F\n");
}

#[gpui::test]
async fn an_operation_left_half_done_shows_its_bar_when_the_app_starts_again(cx: &mut TestAppContext) {
    let fx = conflicting_branches("resolve-restart");
    {
        let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
        open_project(&ws, cx, &fx.repo());
    }
    // The merge stops while the app is closed (in a terminal, say).
    assert!(matches!(GitCli::new(fx.repo()).merge("other").unwrap(), gitgui_core::Outcome::Conflicts { .. }));

    // Started again, the project reopens and everything is read from git: the bar, its words, its counts.
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    cx.run_until_parked();
    draw(cx, &ws);
    assert!(cx.debug_bounds("operation-bar").is_some());
    assert_eq!(ws.read_with(cx, |ws, _| ws.operation().map(|op| op.headline())), Some("Merging other into main".into()));
    assert_eq!(ws.read_with(cx, |ws, _| ws.conflicts_left()), 2);

    // Continue waits for the files; the bar opens the first one.
    click_on(cx, &ws, "operation-continue");
    assert_eq!(GitCli::new(fx.repo()).in_progress(), Some(Operation::Merge));
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.text.contains("Resolve the 2 files"))));
    click_on(cx, &ws, "operation-resolve");
    assert_eq!(resolver_state(&ws, cx).map(|r| r.0).as_deref(), Some("config.txt"));

    // Abort asks first, then puts everything back.
    click_on(cx, &ws, "operation-abort");
    assert_eq!(dialog_text(&ws, cx).0, "Abort the merge?");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    assert_eq!(GitCli::new(fx.repo()).in_progress(), None);
    assert_eq!(std::fs::read_to_string(fx.repo().join("config.txt")).unwrap(), "one\ntwo main\nthree\nfour\nFIVE\nsix\nseven\neight main\nnine\n");
    assert!(ws.read_with(cx, |ws, _| ws.operation().is_none()));
}

#[gpui::test]
async fn added_twice_binary_and_deleted_files_get_their_own_choices(cx: &mut TestAppContext) {
    let fx = bare_fixture("resolve-kinds");
    commit_file(&fx, "notes.txt", "notes\n", "base");
    std::fs::write(fx.repo().join("logo.png"), b"\x00\x01base").unwrap();
    commit_file(&fx, "keep.txt", "k\n", "logo");
    fx.git(&["checkout", "-q", "-b", "other"]);
    fx.write("deploy.txt", "Tuesdays\n");
    fx.git(&["rm", "-q", "notes.txt"]);
    std::fs::write(fx.repo().join("logo.png"), b"\x00\x02other").unwrap();
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "other"]);
    fx.git(&["checkout", "-q", "main"]);
    fx.write("deploy.txt", "Thursdays\n");
    fx.write("notes.txt", "notes, longer\n");
    std::fs::write(fx.repo().join("logo.png"), b"\x00\x03main").unwrap();
    fx.git(&["add", "."]);
    fx.git(&["commit", "-q", "-m", "main"]);
    assert!(matches!(GitCli::new(fx.repo()).merge("other").unwrap(), gitgui_core::Outcome::Conflicts { files: 3, .. }));

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);
    click_on(cx, &ws, "operation-resolve");

    // Added on both sides: text with no base, so no "Neither"; keep both, theirs first.
    assert_eq!(resolver_state(&ws, cx).map(|r| r.0).as_deref(), Some("deploy.txt"));
    draw(cx, &ws);
    assert!(cx.debug_bounds("choose-0-base").is_none());
    click_on(cx, &ws, "choose-0-both");
    click_on(cx, &ws, "conflict-swap-0");
    click_on(cx, &ws, "conflict-use");
    assert_eq!(std::fs::read_to_string(fx.repo().join("deploy.txt")).unwrap(), "Tuesdays\nThursdays\n");

    // A binary file: one whole version, named by meaning.
    assert_eq!(resolver_state(&ws, cx).map(|r| r.0).as_deref(), Some("logo.png"));
    draw(cx, &ws);
    assert!(cx.debug_bounds("conflict-whole").is_some());
    click_on(cx, &ws, "conflict-take-incoming");
    assert_eq!(std::fs::read(fx.repo().join("logo.png")).unwrap(), b"\x00\x02other");

    // Deleted on one side, changed on the other: deleting asks first.
    assert_eq!(resolver_state(&ws, cx).map(|r| r.0).as_deref(), Some("notes.txt"));
    click_on(cx, &ws, "conflict-delete");
    assert_eq!(dialog_text(&ws, cx).0, "Delete notes.txt?");
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    assert!(!fx.repo().join("notes.txt").exists());
    assert!(GitCli::new(fx.repo()).unmerged().unwrap().is_empty());

    click_on(cx, &ws, "operation-continue");
    assert_eq!(GitCli::new(fx.repo()).in_progress(), None);
}

#[gpui::test]
async fn a_long_conflicted_file_folds_its_unchanged_lines_and_draws_quickly(cx: &mut TestAppContext) {
    let fx = bare_fixture("resolve-long");
    let lines = |tag: &str| -> String {
        (0..20_000).map(|i| if i % 5_000 == 2_500 { format!("line {i} {tag}\n") } else { format!("line {i}\n") }).collect()
    };
    commit_file(&fx, "long.txt", &lines("base"), "base");
    fx.git(&["checkout", "-q", "-b", "other"]);
    commit_file(&fx, "long.txt", &lines("other"), "other");
    fx.git(&["checkout", "-q", "main"]);
    commit_file(&fx, "long.txt", &lines("main"), "main");
    let _ = GitCli::new(fx.repo()).merge("other").unwrap();

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    ws.update(cx, |ws, cx| ws.open_first_conflict(cx));
    cx.run_until_parked();
    let rows = ws.read_with(cx, |ws, _| ws.resolver().unwrap().rows.len());
    assert!(rows < 120, "20,000 unchanged lines fold away: {rows} rows");
    let folded = frame_ms(&ws, cx, 20);

    // Unfolded, every line is a row, and a frame still costs only the rows in view.
    let segments: Vec<usize> = ws.read_with(cx, |ws, _| {
        ws.resolver().unwrap().rows.iter().filter_map(|r| match r { crate::resolver::Row::Fold { segment, .. } => Some(*segment), _ => None }).collect()
    });
    for segment in segments {
        ws.update(cx, |ws, cx| ws.unfold(segment, cx));
    }
    let rows = ws.read_with(cx, |ws, _| ws.resolver().unwrap().rows.len());
    assert!(rows > 20_000, "{rows} rows");
    ws.read_with(cx, |ws, _| ws.resolver().unwrap().list.scroll_to(gpui::ListOffset { item_ix: rows / 2, offset_in_item: px(0.) }));
    let unfolded = frame_ms(&ws, cx, 20);
    eprintln!("long conflict: folded {folded:.2} ms/frame, unfolded and scrolled {unfolded:.2} ms/frame");
    if !cfg!(debug_assertions) {
        assert!(folded < 8. && unfolded < 8., "{folded:.2} / {unfolded:.2} ms per frame");
    }
}

/// base ← m1 on main; feat starts at base, then brings main in with a plain merge (a sync merge), then adds f2.
fn synced_branch(name: &str) -> Fixture {
    let fx = bare_fixture(name);
    commit_file(&fx, "a.txt", "base\n", "base");
    fx.git(&["checkout", "-q", "-b", "feat"]);
    commit_file(&fx, "f.txt", "1\n", "feat: f1");
    fx.git(&["checkout", "-q", "main"]);
    commit_file(&fx, "b.txt", "m1\n", "chore: m1");
    fx.git(&["checkout", "-q", "feat"]);
    fx.git(&["merge", "-q", "--no-ff", "-m", "Merge branch 'main' into feat", "main"]);
    commit_file(&fx, "f.txt", "2\n", "feat: f2");
    fx
}

#[gpui::test]
async fn a_merge_that_only_brings_main_in_is_left_out_until_asked_for(cx: &mut TestAppContext) {
    let fx = synced_branch("sync-merge");
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let merge = "Merge branch 'main' into feat";
    let count = |ws: &Entity<Workspace>, cx: &VisualTestContext| {
        ws.read_with(cx, |ws, _| match ws.repo.as_ref().map(|r| &r.phase) {
            Some(Phase::Ready(view)) => view.sync_count,
            _ => 0,
        })
    };
    // The default view: the sync merge is not a row, and f2 follows f1 as if it were not there.
    let rows = summaries(&ws, cx);
    assert!(!rows.iter().any(|s| s == merge), "{rows:?}");
    assert!(["feat: f2", "feat: f1", "chore: m1", "base"].iter().all(|want| rows.iter().any(|s| s == want)), "{rows:?}");
    assert_eq!(count(&ws, cx), 1);
    draw(cx, &ws);

    // Asked for, it is a row with its own mark.
    ws.update(cx, |ws, cx| ws.toggle_sync_merges(cx));
    let rows = summaries(&ws, cx);
    assert!(rows.iter().any(|s| s == merge), "{rows:?}");
    let kind = ws.read_with(cx, |ws, _| match ws.repo.as_ref().map(|r| &r.phase) {
        Some(Phase::Ready(view)) => view.entries.iter().find(|e| e.summary == merge).map(|e| e.kind),
        _ => None,
    });
    assert_eq!(kind, Some(CommitKind::Sync));
    draw(cx, &ws);
    // The key to the marks opens and closes.
    ws.update(cx, |ws, cx| ws.toggle_legend(cx));
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.toggle_legend(cx));
}

/// `count` branches cut from base, each with one commit of its own, all still open (nothing merged): one lane each.
fn many_lanes(name: &str, count: usize) -> Fixture {
    let fx = bare_fixture(name);
    commit_file(&fx, "a.txt", "base\n", "base");
    for n in 0..count {
        fx.git(&["checkout", "-q", "-b", &format!("feat/{n}"), "main"]);
        commit_file(&fx, &format!("f{n}.txt"), "1\n", &format!("feat: work {n}"));
    }
    fx.git(&["checkout", "-q", "main"]);
    fx
}

#[gpui::test]
async fn lanes_past_the_sixth_fold_into_one_until_asked_to_open(cx: &mut TestAppContext) {
    let fx = many_lanes("many-lanes", 10);
    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    ws.update(cx, |ws, cx| ws.set_scope(gitgui_core::Scope::All, cx));
    let measure = |ws: &Entity<Workspace>, cx: &VisualTestContext| {
        ws.read_with(cx, |ws, _| match ws.repo.as_ref().map(|r| &r.phase) {
            Some(Phase::Ready(view)) => (view.folded_lanes, view.graph_width),
            _ => (0, 0.),
        })
    };
    let (folded, narrow) = measure(&ws, cx);
    assert!(folded >= 2, "ten open branches are more lanes than fit: {folded} folded");
    draw(cx, &ws);

    ws.update(cx, |ws, cx| ws.toggle_all_lanes(cx));
    let (folded_open, wide) = measure(&ws, cx);
    assert_eq!(folded_open, folded, "the button says how many either way");
    assert!(wide > narrow, "every lane is drawn: {wide} against {narrow}");
    assert!(Store::at(fx.data()).settings().unwrap().all_lanes, "the choice is kept");
    draw(cx, &ws);
}

#[gpui::test]
async fn commits_only_here_or_only_on_the_remote_are_marked_and_a_label_click_isolates_its_branch(cx: &mut TestAppContext) {
    use crate::graph::Mark;
    let fx = bare_fixture("divergence-marks");
    commit_file(&fx, "a.txt", "base\n", "base");
    let remote = fx.0.join("remote.git");
    fx.git(&["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    fx.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    fx.git(&["push", "-q", "-u", "origin", "main"]);
    // A colleague's commit lands on the remote and is fetched; ours is not pushed.
    let other = fx.0.join("other");
    let run = |dir: &Path, args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Bo")
            .env("GIT_AUTHOR_EMAIL", "bo@example.com")
            .env("GIT_COMMITTER_NAME", "Bo")
            .env("GIT_COMMITTER_EMAIL", "bo@example.com")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    run(&fx.0, &["clone", "-q", remote.to_str().unwrap(), other.to_str().unwrap()]);
    std::fs::write(other.join("theirs.txt"), "t\n").unwrap();
    run(&other, &["add", "."]);
    run(&other, &["commit", "-q", "-m", "theirs"]);
    run(&other, &["push", "-q", "origin", "main"]);
    fx.git(&["fetch", "-q"]);
    commit_file(&fx, "mine.txt", "m\n", "mine");
    // Another branch, to pick out later.
    fx.git(&["checkout", "-q", "-b", "side", "main~1"]);
    commit_file(&fx, "side.txt", "s\n", "side work");
    fx.git(&["checkout", "-q", "main"]);

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    let marks = |ws: &Entity<Workspace>, cx: &VisualTestContext| -> Vec<(String, Option<Mark>)> {
        ws.read_with(cx, |ws, _| match ws.repo.as_ref().map(|r| &r.phase) {
            Some(Phase::Ready(view)) => view.entries.iter().map(|e| (e.summary.to_string(), e.mark)).collect(),
            _ => Vec::new(),
        })
    };
    let found = marks(&ws, cx);
    assert!(found.contains(&("mine".into(), Some(Mark::Unpushed))), "{found:?}");
    assert!(found.contains(&("theirs".into(), Some(Mark::Unpulled))), "{found:?}");
    assert!(found.contains(&("base".into(), None)), "{found:?}");
    assert!(found.iter().any(|(summary, _)| summary == "side work"), "the default view shows every branch: {found:?}");
    // Branch + base leaves the other branch out.
    ws.update(cx, |ws, cx| ws.set_scope(gitgui_core::Scope::Focus, cx));
    assert!(!marks(&ws, cx).iter().any(|(summary, _)| summary == "side work"));
    ws.update(cx, |ws, cx| ws.set_scope(gitgui_core::Scope::All, cx));
    draw(cx, &ws);

    // Pointing at a row brings its line forward, and leaving puts it back.
    ws.update(cx, |ws, cx| ws.script_graph_hover(Some(0), cx));
    assert!(ws.read_with(cx, |ws, _| ws.graph_hover.is_some()));
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.hover_graph_line(None, None, cx));
    assert!(ws.read_with(cx, |ws, _| ws.graph_hover.is_none()));

    // A click on a branch label picks that branch out; Escape puts it back.
    ws.update(cx, |ws, cx| ws.isolate_branch("side", false, cx));
    // The branch, and the one it was cut from with its remote copy: nothing else is left.
    let mut alone: Vec<String> = summaries(&ws, cx);
    alone.sort();
    assert_eq!(alone, ["base", "mine", "side work", "theirs"].map(String::from));
    draw(cx, &ws);
    ws.update(cx, |ws, cx| ws.back(cx));
    assert!(summaries(&ws, cx).iter().any(|s| s == "mine"), "back to the view the filter bar says");
}

#[gpui::test]
async fn a_rebased_branch_shows_its_copies_and_can_be_moved_back(cx: &mut TestAppContext) {
    let fx = bare_fixture("rebased-branch");
    commit_file(&fx, "a.txt", "base\n", "base");
    let remote = fx.0.join("remote.git");
    fx.git(&["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    fx.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    fx.git(&["push", "-q", "-u", "origin", "main"]);
    fx.git(&["checkout", "-q", "-b", "feat"]);
    commit_file(&fx, "f1.txt", "1\n", "feat: f1");
    commit_file(&fx, "f2.txt", "2\n", "feat: f2");
    fx.git(&["push", "-q", "-u", "origin", "feat"]);
    fx.git(&["checkout", "-q", "main"]);
    commit_file(&fx, "m1.txt", "m\n", "chore: m1");
    fx.git(&["push", "-q", "origin", "main"]);
    fx.git(&["checkout", "-q", "feat"]);
    let before = {
        let out = Command::new("git").arg("-C").arg(fx.repo()).args(["rev-parse", "feat"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    fx.git(&["rebase", "-q", "main"]);

    let (ws, cx) = cx.add_window_view(|_, cx| Workspace::with_store(Ok(Store::at(fx.data())), cx));
    open_project(&ws, cx, &fx.repo());
    cx.run_until_parked();
    draw(cx, &ws);

    // Each rebased commit and the one it was copied from point at one another.
    let twins = ws.read_with(cx, |ws, _| match ws.repo.as_ref().map(|r| &r.phase) {
        Some(Phase::Ready(view)) => view.entries.iter().filter(|e| e.twin.is_some()).count(),
        _ => 0,
    });
    assert_eq!(twins, 4, "f1, f2 and their rebased copies");

    // The bar says so, and asks before it moves the branch back.
    let at = center_of(cx, "undo-rebase".to_owned());
    click(cx, MouseButton::Left, at);
    let title = ws.read_with(cx, |ws, _| ws.dialog.as_ref().map(|d| d.title.to_string()));
    assert_eq!(title.as_deref(), Some("Undo the rebase of feat?"));
    ws.update(cx, |ws, cx| ws.confirm_dialog(cx));
    cx.run_until_parked();
    let after = {
        let out = Command::new("git").arg("-C").arg(fx.repo()).args(["rev-parse", "feat"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    assert_eq!(after, before, "feat is where it was before the rebase");
    draw(cx, &ws);
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| !n.warn && n.text.contains("Moved feat back"))));
}

/// A repository whose remote is on GitHub, with a branch `feat/x` that has a commit of its own.
fn github_fixture(name: &str) -> Fixture {
    let fx = bare_fixture(name);
    commit_file(&fx, "a.txt", "base\n", "base");
    fx.git(&["remote", "add", "origin", "git@github.com:acme/app.git"]);
    fx.git(&["checkout", "-q", "-b", "feat/x"]);
    commit_file(&fx, "x.txt", "1\n", "feat: x");
    fx.git(&["checkout", "-q", "main"]);
    fx
}

/// The answers GitHub gives for acme/app: three open pull requests (one of them the person's review to give), a merged
/// one, and two issues.
fn github_answers(fx: &Fixture) -> std::path::PathBuf {
    let dir = fx.0.join("gh");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("user.json"), r#"{"login":"rom"}"#).unwrap();
    std::fs::write(
        dir.join("pulls.json"),
        r#"[
          {"number":3,"state":"open","draft":false,"title":"feat: x","user":{"login":"kim"},"html_url":"https://github.com/acme/app/pull/3","head":{"ref":"feat/x","repo":{"full_name":"acme/app"}},"base":{"ref":"main"},"updated_at":"2026-10-08T01:00:00Z","body":"Summary\n\n## What\n\nAdds **x** with a [guide](https://docs.example.com/x).\n\n- [x] code\n- [ ] docs\n\n```rust\nfn x() {}\n```\n\n![shot](https://github.com/user-attachments/assets/abc)","labels":[{"name":"api","color":"1d76db"}],"assignees":[{"login":"kim"}],"requested_reviewers":[{"login":"rom"}],"merged_at":null},
          {"number":2,"state":"open","draft":true,"title":"wip","user":{"login":"rom"},"html_url":"u","head":{"ref":"wip","repo":{"full_name":"acme/app"}},"base":{"ref":"main"},"updated_at":"2026-10-07T01:00:00Z","merged_at":null},
          {"number":1,"state":"open","draft":false,"title":"from a fork","user":{"login":"out"},"html_url":"u","head":{"ref":"patch","repo":{"full_name":"out/app"}},"base":{"ref":"main"},"updated_at":"2026-10-06T01:00:00Z","merged_at":null}
        ]"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("pulls_closed.json"),
        r#"[{"number":9,"state":"closed","title":"done","user":{"login":"kim"},"html_url":"u","head":{"ref":"old","repo":{"full_name":"acme/app"}},"base":{"ref":"main"},"updated_at":"2026-10-01T01:00:00Z","merged_at":"2026-10-01T01:00:00Z"}]"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("issues.json"),
        r#"[
          {"number":5,"state":"open","title":"a bug","user":{"login":"kim"},"html_url":"u","labels":[{"name":"bug"}],"assignees":[{"login":"rom"}],"comments":2,"updated_at":"2026-10-08T00:00:00Z"},
          {"number":3,"state":"open","title":"the pull request, which is not an issue","user":{"login":"kim"},"html_url":"u","pull_request":{"url":"x"},"updated_at":"2026-10-08T00:00:00Z"}
        ]"#,
    )
    .unwrap();
    dir
}

#[gpui::test]
async fn a_github_project_lists_its_pull_requests_and_issues_beside_the_graph(cx: &mut TestAppContext) {
    use crate::github_ui::{GithubState, ListFilter, Lists, MainTab};
    let fx = github_fixture("github-lists");
    let answers = github_answers(&fx);
    let (ws, cx) = cx.add_window_view(|_, cx| {
        let mut ws = Workspace::with_store(Ok(Store::at(fx.data())), cx);
        ws.github = GithubState::with_transport(std::sync::Arc::new(gitgui_forge::Fixture(answers)), true);
        ws
    });
    open_project(&ws, cx, &fx.repo());
    cx.run_until_parked();
    draw(cx, &ws);

    // Read in the background: three open pull requests, a merged one, and one issue (the pull request in the issues is left out).
    let counts = ws.read_with(cx, |ws, _| match ws.github_lists() {
        Some(Lists::Ready { pulls, issues, .. }) => Some((pulls.len(), issues.len(), ws.github.login.clone())),
        _ => None,
    });
    assert_eq!(counts, Some((4, 1, Some("rom".to_owned()))));

    // The tabs are beside the graph; the Pull requests tab lists them, the one waiting for a review first.
    let at = center_of(cx, "tab-pulls".to_owned());
    click(cx, MouseButton::Left, at);
    draw(cx, &ws);
    assert_eq!(ws.read_with(cx, |ws, _| ws.github.tab), MainTab::Pulls);
    let first = center_of(cx, "pull-3".to_owned());
    click(cx, MouseButton::Left, first);
    draw(cx, &ws);
    assert_eq!(ws.read_with(cx, |ws, _| ws.github.selected), Some(3));
    assert!(cx.debug_bounds("github-checkout").is_some(), "a branch of this repository can be checked out");
    // A fork's branch cannot be checked out from this remote: it says so and does nothing.
    with_window(&ws, cx, |ws, window, cx| ws.github_checkout(1, window, cx));
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.warn && n.text.contains("fork"))));
    assert!(ws.read_with(cx, |ws, _| ws.dialog.is_none() && ws.busy.is_none()));

    // The divider between the list and the panel is dragged: the panel takes the width that meets the pointer, and the
    // list keeps room however far it goes.
    let divider = center_of(cx, "github-divider".to_owned());
    click(cx, MouseButton::Left, divider);
    cx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: divider, modifiers: Modifiers::default(), click_count: 1, first_mouse: false });
    let (left, right) = ws.read_with(cx, |ws, _| ws.github.edges.get());
    assert!(right > left, "the lists have measured themselves: {left}..{right}");
    drag_to(&ws, cx, right - 500., Some(MouseButton::Left));
    assert_eq!(ws.read_with(cx, |ws, _| ws.github.detail_width), 500.);
    drag_to(&ws, cx, left + 10., Some(MouseButton::Left));
    let widest = ws.read_with(cx, |ws, _| ws.github.detail_width);
    assert!(widest <= right - left - crate::github_ui::LIST_MIN + 0.5 && widest >= crate::github_ui::DETAIL_MIN, "the list keeps room: {widest}");
    drag_to(&ws, cx, right - 10., Some(MouseButton::Left));
    assert_eq!(ws.read_with(cx, |ws, _| ws.github.detail_width), crate::github_ui::DETAIL_MIN, "and the panel never gets thinner than it can read");
    drag_to(&ws, cx, 0., None); // the button came up somewhere: the drag is over
    assert!(ws.read_with(cx, |ws, _| ws.resizing.is_none()));
    ws.update(cx, |ws, cx| ws.github_set_detail_width(400., cx));
    draw(cx, &ws);

    // The filters: mine is the draft, review is the one asked of rom, closed is the merged one.
    let shown = |ws: &Entity<Workspace>, cx: &mut VisualTestContext, filter| -> Vec<u32> {
        ws.update(cx, |ws, cx| ws.github_set_filter(filter, cx));
        ws.read_with(cx, |ws, _| match ws.github_lists() {
            Some(Lists::Ready { pulls, .. }) => {
                crate::github_ui::visible_pulls(pulls, ws.github.filter, ws.github.login.as_deref()).iter().map(|p| p.number).collect()
            }
            _ => Vec::new(),
        })
    };
    assert_eq!(shown(&ws, cx, ListFilter::Open), [3, 2, 1]);
    assert_eq!(shown(&ws, cx, ListFilter::Mine), [2]);
    assert_eq!(shown(&ws, cx, ListFilter::Review), [3]);
    assert_eq!(shown(&ws, cx, ListFilter::Closed), [9]);
    draw(cx, &ws);

    // "Show in graph" goes back to the graph with that branch picked out.
    ws.update(cx, |ws, cx| ws.github_show_in_graph(3, cx));
    assert_eq!(ws.read_with(cx, |ws, _| ws.github.tab), MainTab::Graph);
    let rows = summaries(&ws, cx);
    assert!(rows.iter().any(|s| s == "feat: x") && rows.iter().any(|s| s == "base"), "{rows:?}");
    draw(cx, &ws);

    // A branch that is not in this history yet is said so, not guessed at.
    ws.update(cx, |ws, cx| ws.github_show_in_graph(2, cx));
    assert!(ws.read_with(cx, |ws, _| ws.notice.as_ref().is_some_and(|n| n.warn && n.text.contains("isn't in this history"))));

    // The issues tab.
    ws.update(cx, |ws, cx| ws.github_set_tab(MainTab::Issues, cx));
    draw(cx, &ws);
    assert!(cx.debug_bounds("issue-5").is_some());
}

/// Answers each request with the next one in line.
struct Script(std::sync::Mutex<Vec<gitgui_forge::Response>>);

impl gitgui_forge::Transport for Script {
    fn send(&self, _: &gitgui_forge::Request) -> Result<gitgui_forge::Response, gitgui_forge::Error> {
        let mut answers = self.0.lock().unwrap();
        if answers.is_empty() { Err(gitgui_forge::Error::Network("no more answers".into())) } else { Ok(answers.remove(0)) }
    }
}

#[gpui::test]
async fn signing_in_shows_the_code_waits_for_the_approval_and_then_lists(cx: &mut TestAppContext) {
    use crate::github_ui::{GithubState, Lists};
    let fx = github_fixture("github-sign-in");
    let ok = |body: &str| gitgui_forge::Response { status: 200, body: body.to_owned(), ..Default::default() };
    let answers = vec![
        ok(r#"{"device_code":"dc","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","expires_in":900,"interval":1}"#),
        ok(r#"{"error":"authorization_pending"}"#),
        ok(r#"{"access_token":"ghu_x","expires_in":28800,"refresh_token":"ghr_y","refresh_token_expires_in":15897600}"#),
        ok(r#"{"login":"rom"}"#),
        ok(r#"[{"number":3,"state":"open","title":"feat: x","user":{"login":"kim"},"head":{"ref":"feat/x"},"base":{"ref":"main"}}]"#),
        ok("[]"),
        ok("[]"),
        ok("[]"),
    ];
    let (ws, cx) = cx.add_window_view(|_, cx| {
        let mut ws = Workspace::with_store(Ok(Store::at(fx.data())), cx);
        ws.github = GithubState::with_transport(std::sync::Arc::new(Script(std::sync::Mutex::new(answers))), false);
        ws
    });
    open_project(&ws, cx, &fx.repo());
    draw(cx, &ws);
    // Signed out: the sidebar offers the sign-in, and nothing has been read.
    assert!(cx.debug_bounds("github-side-sign-in").is_some());
    assert!(ws.read_with(cx, |ws, _| ws.github.token.is_none() && ws.github_lists().is_none()));

    ws.update(cx, |ws, cx| ws.github_sign_in(cx));
    // The code comes first, and the window that shows it is up while the wait goes on.
    for _ in 0..3 {
        cx.run_until_parked();
        if ws.read_with(cx, |ws, _| ws.github.flow.is_some() || ws.github.token.is_some()) {
            break;
        }
    }
    cx.run_until_parked();
    // Approved: signed in, the window is gone, and the lists were read.
    let state = ws.read_with(cx, |ws, _| {
        (
            ws.github.token.is_some(),
            ws.github.flow.is_none(),
            ws.dialog.is_none(),
            ws.github.login.clone(),
            matches!(ws.github_lists(), Some(Lists::Ready { pulls, .. }) if pulls.len() == 1),
        )
    });
    assert_eq!(state, (true, true, true, Some("rom".to_owned()), true), "signed in");

    // Signing out forgets all of it.
    ws.update(cx, |ws, cx| ws.github_sign_out(cx));
    assert!(ws.read_with(cx, |ws, _| ws.github.token.is_none() && ws.github.login.is_none() && ws.github_lists().is_none()));
}

#[gpui::test]
async fn a_repository_the_app_is_not_installed_on_says_so(cx: &mut TestAppContext) {
    use crate::github_ui::{GithubState, Lists};
    let fx = github_fixture("github-no-access");
    let refused = gitgui_forge::Response { status: 404, body: r#"{"message":"Not Found"}"#.into(), ..Default::default() };
    let user = gitgui_forge::Response { status: 200, body: r#"{"login":"rom"}"#.into(), ..Default::default() };
    let script = Script(std::sync::Mutex::new(vec![user, refused]));
    let (ws, cx) = cx.add_window_view(|_, cx| {
        let mut ws = Workspace::with_store(Ok(Store::at(fx.data())), cx);
        ws.github = GithubState::with_transport(std::sync::Arc::new(script), true);
        ws
    });
    open_project(&ws, cx, &fx.repo());
    cx.run_until_parked();
    draw(cx, &ws);
    assert!(ws.read_with(cx, |ws, _| matches!(ws.github_lists(), Some(Lists::Failed(gitgui_forge::Error::NoAccess)))));
    assert!(cx.debug_bounds("github-side-install").is_some(), "the sidebar offers to install the app");
}
