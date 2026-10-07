//! gitgui: browse a repository's history, its commits' files and diffs, and comment on lines.
//!
//! usage: gitgui-app [PATH]

mod detail;
mod diff_view;
mod graph;
mod layout;
mod menu;
mod pane;
mod rows;
#[cfg(test)]
mod tests;
mod text_input;
mod ui;
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
        OpenSettings,
        Refresh,
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
            KeyBinding::new("cmd-o", OpenFolder, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-r", Refresh, None),
            KeyBinding::new("escape", Back, None),
            KeyBinding::new("cmd-e", ToggleFullView, None),
            KeyBinding::new("cmd-w", CloseWindow, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);

        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
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

        let target = workspace.clone();
        cx.on_action(move |_: &OpenFolder, cx| target.update(cx, |this, cx| this.open_folder(cx)));
        let target = workspace.clone();
        cx.on_action(move |_: &OpenSettings, cx| target.update(cx, |this, cx| this.open_settings(cx)));
        let target = workspace.clone();
        cx.on_action(move |_: &Refresh, cx| target.update(cx, |this, cx| this.refresh(cx)));
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
                    MenuItem::action("Refresh", Refresh),
                    MenuItem::separator(),
                    MenuItem::action("Close Window", CloseWindow),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
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
