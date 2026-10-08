//! gitgui: browse a repository's history, its commits' files and diffs, and comment on lines.
//!
//! usage: gitgui-app [PATH]

// A release build on Windows is a window program: no console opens behind it.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod avatars;
mod changes;
mod detail;
mod diff_view;
mod graph;
mod github_ui;
mod github_view;
mod graph_style;
mod icons;
mod layout;
mod menu;
mod minimap;
mod pane;
mod preview;
mod resolver;
mod rows;
mod script;
mod settings_view;
mod snapshot;
mod syntax;
#[cfg(test)]
mod tests;
mod text_input;
mod theme;
mod ui;
mod workflow_ui;
mod workspace;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, SystemMenuType, TitlebarOptions, WindowBounds,
    WindowOptions, actions, prelude::*, px, size,
};

use workspace::Workspace;

actions!(
    gitgui,
    [
        OpenFolder,
        CloneRepository,
        OpenSettings,
        Refresh,
        FindCommits,
        ToggleSidebar,
        Back,
        ToggleFullView,
        CloseWindow,
        Minimize,
        Zoom,
        ToggleFullScreen,
        Hide,
        HideOthers,
        ShowAll,
        Quit,
    ]
);

fn main() {
    Application::new().run(|cx: &mut App| {
        text_input::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("secondary-o", OpenFolder, None),
            KeyBinding::new("secondary-shift-o", CloneRepository, None),
            KeyBinding::new("secondary-,", OpenSettings, None),
            KeyBinding::new("secondary-r", Refresh, None),
            KeyBinding::new("secondary-f", FindCommits, None),
            KeyBinding::new("secondary-b", ToggleSidebar, None),
            KeyBinding::new("up", workspace::PreviousFile, None),
            KeyBinding::new("down", workspace::NextFile, None),
            KeyBinding::new("escape", Back, None),
            KeyBinding::new("secondary-e", ToggleFullView, None),
            KeyBinding::new("secondary-w", CloseWindow, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("secondary-q", Quit, None),
        ]);

        // A development aid, like the script below: open on the nth screen. A picture of the window is as sharp as its
        // screen, so the README's are taken on a Retina one.
        let display = std::env::var("GITGUI_DISPLAY").ok().and_then(|n| n.parse::<usize>().ok()).and_then(|n| cx.displays().get(n).map(|d| d.id()));
        let bounds = Bounds::centered(display, size(px(1280.), px(800.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    display_id: display,
                    // The system title bar: close, minimize and zoom buttons, drag to move, and a
                    // menu bar that comes down when the pointer reaches the top in full screen.
                    titlebar: Some(TitlebarOptions { title: Some("gitgui".into()), ..Default::default() }),
                    window_min_size: Some(size(px(900.), px(560.))),
                    ..Default::default()
                },
                |_, cx| cx.new(Workspace::new),
            )
            .expect("could not open the window");
        let workspace = window.update(cx, |_, _, cx| cx.entity()).expect("the window was just opened");
        // A development aid: drive the window through a script and save pictures of it.
        if let Ok(script) = std::env::var("GITGUI_SCRIPT") {
            script::run(window, script, cx);
        }

        let target = workspace.clone();
        cx.on_action(move |_: &OpenFolder, cx| target.update(cx, |this, cx| this.open_folder(cx)));
        let target = workspace.clone();
        cx.on_action(move |_: &CloneRepository, cx| {
            if let Some(window) = cx.active_window() {
                window.update(cx, |_, window, cx| target.update(cx, |this, cx| this.start_clone(window, cx))).ok();
            }
        });
        let target = workspace.clone();
        cx.on_action(move |_: &OpenSettings, cx| target.update(cx, |this, cx| this.open_settings(cx)));
        let target = workspace.clone();
        cx.on_action(move |_: &Refresh, cx| target.update(cx, |this, cx| this.refresh(cx)));
        let target = workspace.clone();
        cx.on_action(move |_: &ToggleSidebar, cx| target.update(cx, |this, cx| this.toggle_sidebar(cx)));
        let target = workspace.clone();
        cx.on_action(move |_: &FindCommits, cx| {
            let input = target.read(cx).search_input.clone();
            if let Some(window) = cx.active_window() {
                window.update(cx, |_, window, cx| input.read(cx).focus(window)).ok();
            }
        });
        let target = workspace.clone();
        cx.on_action(move |_: &Back, cx| target.update(cx, |this, cx| this.back(cx)));
        let target = workspace.clone();
        cx.on_action(move |_: &ToggleFullView, cx| target.update(cx, |this, cx| this.toggle_expanded(cx)));

        cx.on_action(|_: &CloseWindow, cx| {
            if let Some(window) = cx.active_window() {
                window.update(cx, |_, window, _| window.remove_window()).ok();
            }
        });
        cx.on_action(|_: &Minimize, cx| {
            if let Some(window) = cx.active_window() {
                window.update(cx, |_, window, _| window.minimize_window()).ok();
            }
        });
        cx.on_action(|_: &Zoom, cx| {
            if let Some(window) = cx.active_window() {
                window.update(cx, |_, window, _| window.zoom_window()).ok();
            }
        });
        cx.on_action(|_: &ToggleFullScreen, cx| {
            if let Some(window) = cx.active_window() {
                window.update(cx, |_, window, _| window.toggle_fullscreen()).ok();
            }
        });
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.on_action(|_: &Quit, cx| cx.quit());

        cx.set_menus(vec![
            Menu {
                name: "gitgui".into(),
                items: vec![
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Hide gitgui", Hide),
                    MenuItem::action("Hide Others", HideOthers),
                    MenuItem::action("Show All", ShowAll),
                    MenuItem::separator(),
                    MenuItem::action("Quit gitgui", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("Open Folder…", OpenFolder),
                    MenuItem::action("Clone Repository…", CloneRepository),
                    MenuItem::action("Refresh", Refresh),
                    MenuItem::separator(),
                    MenuItem::action("Close Window", CloseWindow),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Toggle Sidebar", ToggleSidebar),
                    MenuItem::action("Find Commits…", FindCommits),
                    MenuItem::action("Toggle Full View of the File Pane", ToggleFullView),
                    MenuItem::action("Enter Full Screen", ToggleFullScreen),
                ],
            },
            Menu {
                name: "Window".into(),
                items: vec![MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)],
            },
        ]);

        // Closing the last window ends the app.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        workspace::open_initial(&workspace, cx);
        cx.activate(true);
    });
}
