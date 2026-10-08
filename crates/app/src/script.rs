//! A development aid: with `GITGUI_SCRIPT=steps.txt` the app drives itself through the steps in the file
//! and saves what its window looks like (`GITGUI_SHOTS=folder` says where; default the temp folder).
//! It is how the interface is looked at without anyone at the screen. Nothing here runs without the variable.
//!
//! One step per line; blank lines and `#` lines are ignored:
//!
//! ```text
//! size 1280x800           resize the window
//! open /path/to/repo      add the folder and open it
//! wait 800                pause, in milliseconds
//! select <text>           the first commit whose summary has the text (or a row number)
//! file 0                  open the commit's nth changed file
//! mode split|unified      layout tree|flat      expand      back      more
//! search <text>   fill the search box (author:… date:… words)      menu authors|dates X Y   open that chip's menu
//! scope focus|current|local|all   which branches the graph shows      lanes   fold or unfold the lanes past the sixth      sync   show or hide sync merges      isolate <branch>   pick that branch out of the graph (clear   put it back)      pointer <row>   as if the pointer were on that graph row      legend   the key to the marks
//! tab graph|pulls|issues   the middle of the window      ghfilter open|mine|review|closed      ghselect <number>   open that pull request or issue
//! review below|beside    where the file pane goes      theme <name>   e.g. theme One Light      style <id>   graph style, e.g. style neon
//! sidebar | graph | files   hide or show that panel      peek sidebar|graph|files   as if the pointer were at its edge;
//! nopeek   as if it had left
//! hover <text>   rest the pointer on the first diff line whose text has the text (nopeek-style: `hover` alone leaves)
//! settings [graph|style|files|appearance|projects]
//! fetch | pull | push     the header's buttons (pull and push open their question)      autofetch 5   fetch on its own every 5 min
//! merge <branch> | rebase <onto> | cherrypick <text>   open that question (with its test merge)      confirm   answer yes
//! conflict [n|text]   open the nth file with conflicts (or the first whose path has the text) in the resolver
//! choose <n> current|incoming|both|both2|base|none   a choice for conflict n      key 1 n 2 …   the resolver's keys
//! safe   resolve the safe ones      useresult      continue   the bar's Continue      base   show or hide the base
//! shot name               save <folder>/name.png
//! quit
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use gitgui_core::Layout;
use gpui::{App, WindowHandle, px, size};

use crate::rows::Mode;
use crate::settings_view::SettingsPage;
use crate::snapshot;
use crate::workspace::{Phase, Workspace};

/// Starts the steps in `file` after the window has had a moment to draw.
pub fn run(window: WindowHandle<Workspace>, file: String, cx: &mut App) {
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) => return eprintln!("gitgui: cannot read the script {file}: {err}"),
    };
    let dir = std::env::var_os("GITGUI_SHOTS").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    eprintln!("gitgui: running the script {file}");
    // A window that is covered is not drawn, and a picture of it would show what it last drew.
    cx.activate(true);
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        let pause = |ms: u64| executor.timer(Duration::from_millis(ms));
        pause(900).await;
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let (word, rest) = line.split_once(' ').map_or((line, ""), |(word, rest)| (word, rest.trim()));
            eprintln!("gitgui: step {line}");
            match word {
                "wait" => pause(rest.parse().unwrap_or(500)).await,
                "shot" => {
                    // Bring the window forward and ask for a frame: a covered one is not drawn, and the picture would
                    // show what it last drew.
                    window
                        .update(cx, |_, window, cx| {
                            cx.activate(true);
                            window.activate_window();
                            window.refresh();
                        })
                        .ok();
                    pause(1000).await;
                    let path = dir.join(format!("{rest}.png"));
                    match window.update(cx, |_, window, _| snapshot::save(window, &path)) {
                        Ok(Ok(())) => eprintln!("gitgui: saved {}", path.display()),
                        Ok(Err(err)) => eprintln!("gitgui: no picture for {rest}: {err}"),
                        Err(err) => eprintln!("gitgui: no picture for {rest}: {err}"),
                    }
                }
                "quit" => {
                    // `cx.quit()` leaves a window behind when it is asked from here; leave for good.
                    std::process::exit(0);
                }
                _ => {
                    window.update(cx, |workspace, window, cx| step(workspace, window, word, rest, cx)).ok();
                    pause(500).await;
                }
            }
        }
    })
    .detach();
}

fn numbers(text: &str) -> Vec<f32> {
    text.split(|c: char| c == 'x' || c.is_whitespace()).filter_map(|n| n.parse().ok()).collect()
}

fn step(workspace: &mut Workspace, window: &mut gpui::Window, word: &str, rest: &str, cx: &mut gpui::Context<Workspace>) {
    match word {
        "size" => {
            if let [w, h] = numbers(rest)[..] {
                window.resize(size(px(w), px(h)));
            }
        }
        "open" => workspace.add_folder(Path::new(rest), cx),
        "select" => workspace.script_select(rest, cx),
        "file" => workspace.open_file(rest.parse().unwrap_or(0), cx),
        "mode" => workspace.set_mode(if rest == "split" { Mode::Split } else { Mode::Unified }, cx),
        "layout" => workspace.set_layout(if rest == "flat" { Layout::Flat } else { Layout::Tree }, cx),
        "expand" => workspace.toggle_expanded(cx),
        "back" => workspace.back(cx),
        "more" => workspace.more_context(cx),
        "review" => workspace.set_review_layout(
            if rest == "beside" { gitgui_store::ReviewLayout::Beside } else { gitgui_store::ReviewLayout::Below },
            cx,
        ),
        "theme" => workspace.set_theme(rest, cx),
        "style" => workspace.set_graph_style(rest, cx),
        "scope" => workspace.set_scope(
            match rest {
                "focus" => gitgui_core::Scope::Focus,
                "current" => gitgui_core::Scope::Current,
                "local" => gitgui_core::Scope::Local,
                _ => gitgui_core::Scope::All,
            },
            cx,
        ),
        "lanes" => workspace.toggle_all_lanes(cx),
        "tab" => workspace.github_set_tab(
            match rest {
                "pulls" => crate::github_ui::MainTab::Pulls,
                "issues" => crate::github_ui::MainTab::Issues,
                _ => crate::github_ui::MainTab::Graph,
            },
            cx,
        ),
        "ghfilter" => workspace.github_set_filter(
            match rest {
                "mine" => crate::github_ui::ListFilter::Mine,
                "review" => crate::github_ui::ListFilter::Review,
                "closed" => crate::github_ui::ListFilter::Closed,
                _ => crate::github_ui::ListFilter::Open,
            },
            cx,
        ),
        "ghselect" => workspace.github_select(rest.parse().ok(), cx),
        "sync" => workspace.toggle_sync_merges(cx),
        "isolate" => workspace.isolate_branch(rest, false, cx),
        "clear" => workspace.clear_isolate(cx),
        "pointer" => workspace.script_graph_hover(rest.parse().ok(), cx),
        "legend" => workspace.toggle_legend(cx),
        "hover" => workspace.script_hover(rest, cx),
        "newbranch" => {
            workspace.open_new_branch(window, cx);
            // `newbranch kind ticket title…` also fills it in.
            let mut words = rest.splitn(3, ' ');
            if let (Some(kind), Some(ticket), Some(title)) = (words.next(), words.next(), words.next()) {
                workspace.set_branch_kind(if kind == "-" { "" } else { kind }, cx);
                workspace.branch_ticket.update(cx, |input, cx| input.replace_text(if ticket == "-" { "" } else { ticket }, cx));
                workspace.branch_title.update(cx, |input, cx| input.replace_text(title, cx));
            }
        }
        // pickbase [query]: the list of branches to start from, with a search typed in it.
        "pickbase" => {
            workspace.open_branch_picker(window, cx);
            workspace.branch_search.update(cx, |input, cx| input.replace_text(rest, cx));
        }
        // reveal up N | reveal down N | reveal tail: the arrows in the diff's gutter, of hunk N.
        "reveal" => {
            let mut words = rest.split_whitespace();
            let (which, hunk) = (words.next().unwrap_or(""), words.next().and_then(|n| n.parse().ok()));
            match which {
                "up" => workspace.reveal_lines(hunk, true, cx),
                "down" => workspace.reveal_lines(hunk, false, cx),
                _ => workspace.reveal_lines(None, false, cx),
            }
        }
        "search" => workspace.set_search(rest.to_owned(), cx),
        "fetch" => workspace.fetch(cx),
        "pull" => workspace.choose(crate::menu::Action::PullRebase, window, cx),
        "push" => workspace.push_current(window, cx),
        "autofetch" => workspace.set_auto_fetch(rest.parse().unwrap_or(0), cx),
        "menu" => {
            // menu authors|dates X Y: the menu a click there on that chip would open.
            let (kind, at) = rest.split_once(' ').unwrap_or((rest, "0 0"));
            if let [x, y] = numbers(at)[..] {
                let target = if kind == "dates" { crate::menu::MenuTarget::Dates } else { crate::menu::MenuTarget::Authors };
                workspace.open_menu(gpui::point(px(x), px(y)), target, cx);
            }
        }
        "merge" => workspace.choose(crate::menu::Action::Merge(rest.to_owned()), window, cx),
        "rebase" => workspace.choose(crate::menu::Action::Rebase(rest.to_owned()), window, cx),
        "cherrypick" => {
            let found = match workspace.repo.as_ref().map(|repo| &repo.phase) {
                Some(Phase::Ready(view)) => view.entries.iter().find(|e| e.commit.is_some() && e.summary.contains(rest)).and_then(|e| e.commit.clone()),
                _ => None,
            };
            match found {
                Some(id) => workspace.choose(crate::menu::Action::CherryPick(id), window, cx),
                None => eprintln!("gitgui: no commit matches {rest:?}"),
            }
        }
        "confirm" => workspace.confirm_dialog(cx),
        // conflict alone: the bar's "Resolve next file".
        "conflict" if rest.is_empty() => workspace.open_first_conflict(cx),
        "conflict" => {
            let found = workspace.repo.as_ref().and_then(|repo| {
                let conflicted = repo.work.iter().enumerate().filter(|(_, f)| f.conflicted);
                match rest.parse::<usize>() {
                    Ok(n) => conflicted.map(|(i, _)| i).nth(n),
                    Err(_) => conflicted.filter(|(_, f)| f.change.path.contains(rest)).map(|(i, _)| i).next(),
                }
            });
            match found {
                Some(index) => workspace.open_work_file(index, cx),
                None => eprintln!("gitgui: no file with conflicts matches {rest:?}"),
            }
        }
        "choose" => {
            let (block, which) = rest.split_once(' ').unwrap_or((rest, ""));
            let choice = match which {
                "current" => Some(gitgui_core::Resolution::Current),
                "incoming" => Some(gitgui_core::Resolution::Incoming),
                "both" => Some(gitgui_core::Resolution::CurrentThenIncoming),
                "both2" => Some(gitgui_core::Resolution::IncomingThenCurrent),
                "base" => Some(gitgui_core::Resolution::Base),
                _ => None,
            };
            workspace.choose_block(block.parse().unwrap_or(0), choice, false, cx);
        }
        "key" => {
            for key in rest.split_whitespace() {
                if let Ok(keystroke) = gpui::Keystroke::parse(key) {
                    workspace.resolver_key(&gpui::KeyDownEvent { keystroke, is_held: false }, window, cx);
                }
            }
        }
        "safe" => workspace.resolve_safe(cx),
        "useresult" => workspace.use_result(cx),
        "continue" => workspace.continue_operation(cx),
        "base" => workspace.toggle_conflict_base(cx),
        "sidebar" => workspace.toggle_sidebar(cx),
        "graph" => workspace.toggle_graph_hidden(cx),
        "files" => workspace.toggle_files_visible(cx),
        "nopeek" => workspace.peek = None,
        "peek" => workspace.peek_panel(
            match rest {
                "graph" => crate::workspace::Panel::Graph,
                "files" => crate::workspace::Panel::Files,
                _ => crate::workspace::Panel::Sidebar,
            },
            cx,
        ),
        "settings" => {
            workspace.open_settings(cx);
            let page = match rest {
                "files" => SettingsPage::Files,
                "style" => SettingsPage::GraphStyle,
                "workflow" => SettingsPage::Workflow,
                "appearance" => SettingsPage::Appearance,
                "projects" => SettingsPage::Projects,
                "github" => SettingsPage::GitHub,
                _ => SettingsPage::Graph,
            };
            workspace.set_settings_page(page, cx);
        }
        other => eprintln!("gitgui: unknown step {other:?}"),
    }
}

impl Workspace {
    /// Selects the first commit whose summary contains `text`, or the row with that number.
    pub(crate) fn script_select(&mut self, text: &str, cx: &mut gpui::Context<Self>) {
        let found = match self.repo.as_ref().map(|repo| &repo.phase) {
            Some(Phase::Ready(view)) => text
                .parse::<usize>()
                .ok()
                .or_else(|| view.entries.iter().position(|entry| entry.commit.is_some() && entry.summary.contains(text))),
            _ => None,
        };
        match found {
            Some(ix) => self.select_entry(ix, cx),
            None => eprintln!("gitgui: no commit matches {text:?}"),
        }
    }
}
