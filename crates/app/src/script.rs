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
//! review below|beside    where the file pane goes      theme <name>   e.g. theme One Light      style <id>   graph style, e.g. style neon
//! sidebar | graph | files   hide or show that panel      peek sidebar|graph|files   as if the pointer were at its edge;
//! nopeek   as if it had left
//! settings [graph|files|appearance|projects]
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
        "search" => workspace.set_search(rest.to_owned(), cx),
        "menu" => {
            // menu authors|dates X Y: the menu a click there on that chip would open.
            let (kind, at) = rest.split_once(' ').unwrap_or((rest, "0 0"));
            if let [x, y] = numbers(at)[..] {
                let target = if kind == "dates" { crate::menu::MenuTarget::Dates } else { crate::menu::MenuTarget::Authors };
                workspace.open_menu(gpui::point(px(x), px(y)), target, cx);
            }
        }
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
                "appearance" => SettingsPage::Appearance,
                "projects" => SettingsPage::Projects,
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
