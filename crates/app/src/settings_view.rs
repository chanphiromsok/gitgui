//! The settings window: a list of pages on the left, the chosen page on the right.
//!
//! Every setting is one row (what it is and what it does on the left, its control on the right), and rows
//! of a kind sit together in a card. Switches are for on/off, segmented controls for a few choices, cards
//! for themes. Everything is saved the moment it changes, so there is nothing to apply, only Done.

use gpui::{
    AnyElement, Context, FontWeight, Pixels, SharedString, Stateful, Window, div, prelude::*, px, rgb,
};
use gitgui_core::Layout;
use gitgui_store::{DiffMode, FileLayout, GraphFaces, ReviewLayout};

use crate::icons;
use crate::menu::modal;
use crate::rows::Mode;
use crate::theme::{Origin, t};
use crate::ui::{MONO, button, segment, segmented};
use crate::workspace::Workspace;

const NAV_W: f32 = 184.;
const MAX_W: f32 = 820.;
const MAX_H: f32 = 600.;

/// The pages of the settings window, in the order they are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
    Graph,
    Files,
    Appearance,
    Projects,
}

impl SettingsPage {
    pub const ALL: [SettingsPage; 4] = [Self::Graph, Self::Files, Self::Appearance, Self::Projects];

    fn title(self) -> &'static str {
        match self {
            Self::Graph => "Graph",
            Self::Files => "Files & diffs",
            Self::Appearance => "Appearance",
            Self::Projects => "Projects",
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Self::Graph => "How the commit history is drawn and what is shown beside it.",
            Self::Files => "How a commit's changed files and their diffs are laid out.",
            Self::Appearance => "Colors and file icons.",
            Self::Projects => "Where new clones go, and where gitgui keeps its own files.",
        }
    }
}

/// An on/off control.
fn switch(id: &'static str, on: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex_none()
        .w(px(38.))
        .h(px(22.))
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .flex()
        .items_center()
        .bg(rgb(if on { t().accent } else { t().element }))
        .when(on, |track| track.justify_end())
        .child(div().size(px(18.)).rounded_full().bg(rgb(0xffffff)))
}

/// One setting: its name and what it does, and its control.
fn row(title: &'static str, detail: &'static str, control: impl IntoElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_4()
        .px_4()
        .py_3()
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(rgb(t().text_strong)).child(title))
                .child(div().text_xs().text_color(rgb(t().muted)).child(detail)),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// Rows of one kind, in a card with a line between them.
fn card(title: Option<&'static str>, rows: Vec<AnyElement>) -> AnyElement {
    let count = rows.len();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(title.map(|title| {
            div().px_1().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(t().muted)).child(title.to_uppercase())
        }))
        .child(
            div()
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(rgb(t().border))
                .bg(rgb(t().card))
                .children(rows.into_iter().enumerate().map(|(ix, row)| {
                    div().when(ix + 1 < count, |r| r.border_b_1().border_color(rgb(t().border))).child(row)
                })),
        )
        .into_any_element()
}

impl Workspace {
    pub fn set_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if self.settings_page != page {
            self.settings_page = page;
            cx.notify();
        }
    }

    pub(crate) fn render_settings(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.settings_open {
            return None;
        }
        let view: Pixels = window.viewport_size().width;
        let height: Pixels = window.viewport_size().height;
        let width = (view - px(48.)).min(px(MAX_W)).max(px(480.));
        let height = (height * 0.9).min(px(MAX_H));
        let page = self.settings_page;
        // Two cards to a row, filling it: the page's width less the list of pages and the page's own padding.
        let card_w = ((f32::from(width) - NAV_W - 48. - 8.) / 2.).floor();

        let nav = div()
            .flex_none()
            .w(px(NAV_W))
            .h_full()
            .flex()
            .flex_col()
            .gap_0p5()
            .p_3()
            .bg(rgb(t().panel))
            .border_r_1()
            .border_color(rgb(t().border))
            .child(div().px_2().pb_3().text_base().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child("Settings"))
            .children(SettingsPage::ALL.into_iter().map(|this_page| {
                let chosen = this_page == page;
                div()
                    .id(("settings-page", this_page as usize))
                    .debug_selector(move || format!("settings-page-{}", this_page as usize))
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .when(chosen, |item| item.bg(rgb(t().selected)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(t().text_strong)))
                    .when(!chosen, |item| item.text_color(rgb(t().text)).hover(|style| style.bg(rgb(t().hover))))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_settings_page(this_page, cx)))
                    .child(this_page.title())
            }));

        let content = div()
            .id("settings-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_6()
            .py_5()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child(page.title()))
                    .child(div().text_color(rgb(t().muted)).child(page.blurb())),
            )
            .children(self.settings_page_body(page, card_w, cx));

        let footer = div()
            .flex_none()
            .h(px(48.))
            .px_6()
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(rgb(t().border))
            .child(div().text_xs().text_color(rgb(t().muted)).child("Changes are saved as you make them."))
            .child(
                button("settings-done", "Done")
                    .debug_selector(|| "settings-done".to_owned())
                    .bg(rgb(t().accent))
                    .text_color(rgb(t().on_accent))
                    .font_weight(FontWeight::BOLD)
                    .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx))),
            );

        Some(modal(
            div()
                .w(width)
                .h(height)
                .flex()
                .overflow_hidden()
                .rounded_lg()
                .child(nav)
                .child(div().min_w_0().flex_1().h_full().flex().flex_col().bg(rgb(t().bg)).child(content).child(footer)),
        ))
    }

    fn settings_page_body(&self, page: SettingsPage, card_w: f32, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = &self.settings;
        match page {
            SettingsPage::Graph => {
                let faces = |id: &'static str, label: &'static str, value: GraphFaces| {
                    segment(id, label, s.graph_faces == value).on_click(cx.listener(move |this, _, _, cx| this.set_graph_faces(value, cx)))
                };
                let scale = [75u32, 100, 125, 150, 200].map(|percent| {
                    segment(("setting-scale", percent as usize), format!("{percent}%"), s.graph_scale == percent)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_graph_scale(percent, cx)))
                });
                vec![
                    card(
                        Some("History"),
                        vec![
                            row(
                                "Group commits under their pull request",
                                "List a pull request's commits indented under it, with a guide line, and fold them with the chevron. \
                                 Squash-merged branches go under their squash commit. Off lists every commit by date.",
                                switch("setting-group", s.group_by_parent)
                                    .on_click(cx.listener(|this, _, _, cx| this.toggle_group_by_parent(cx))),
                            ),
                            row(
                                "Pictures in the graph",
                                "Which commits are drawn with their author's picture. The rest are plain dots.",
                                segmented(vec![
                                    faces("faces-tips", "Branch tips", GraphFaces::Tips),
                                    faces("faces-all", "Every commit", GraphFaces::All),
                                    faces("faces-selected", "Selected branch", GraphFaces::Selected),
                                ]),
                            ),
                            row(
                                "Fetch authors' pictures",
                                "From GitHub (and Gravatar, which is sent a SHA-256 of each email). Kept for a week. Off shows initials.",
                                switch("setting-avatars", s.fetch_avatars)
                                    .on_click(cx.listener(|this, _, _, cx| this.toggle_fetch_avatars(cx))),
                            ),
                        ],
                    ),
                    card(
                        Some("Drawing"),
                        vec![
                            row(
                                "Compact graph",
                                "Narrow lanes and thin lines, so a busy history leaves more room for messages.",
                                switch("setting-compact", s.compact_graph)
                                    .on_click(cx.listener(|this, _, _, cx| this.toggle_compact_graph(cx))),
                            ),
                            row(
                                "Graph size",
                                "How large the commit circles, lanes and rows are. Larger reads better on a big screen.",
                                segmented(scale.into_iter().collect()),
                            ),
                        ],
                    ),
                ]
            }
            SettingsPage::Files => vec![card(
                None,
                vec![
                    row(
                        "Review layout",
                        "Where a commit's files and diff go. Below the graph they get the whole width, which suits a split diff.",
                        segmented(vec![
                            segment("setting-review-below", "Below the graph", s.review_layout == ReviewLayout::Below)
                                .on_click(cx.listener(|this, _, _, cx| this.set_review_layout(ReviewLayout::Below, cx))),
                            segment("setting-review-beside", "Beside the graph", s.review_layout == ReviewLayout::Beside)
                                .on_click(cx.listener(|this, _, _, cx| this.set_review_layout(ReviewLayout::Beside, cx))),
                        ]),
                    ),
                    row(
                        "File list",
                        "How a commit's changed files are listed. Also the Tree / Flat buttons above the list.",
                        segmented(vec![
                            segment("setting-layout-tree", "Tree", s.file_layout == FileLayout::Tree)
                                .on_click(cx.listener(|this, _, _, cx| this.set_layout(Layout::Tree, cx))),
                            segment("setting-layout-flat", "Flat", s.file_layout == FileLayout::Flat)
                                .on_click(cx.listener(|this, _, _, cx| this.set_layout(Layout::Flat, cx))),
                        ]),
                    ),
                    row(
                        "Diff view",
                        "How a file's changes are laid out. Also the Unified / Split buttons above the diff.",
                        segmented(vec![
                            segment("setting-diff-unified", "Unified", s.diff_mode == DiffMode::Unified)
                                .on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::Unified, cx))),
                            segment("setting-diff-split", "Split", s.diff_mode == DiffMode::Split)
                                .on_click(cx.listener(|this, _, _, cx| this.set_mode(Mode::Split, cx))),
                        ]),
                    ),
                ],
            )],
            SettingsPage::Appearance => vec![self.theme_cards(card_w, cx), self.icon_cards(card_w, cx)],
            SettingsPage::Projects => {
                let folder = self.clone_folder();
                let data = self.store_dir().map(|dir| dir.display().to_string()).unwrap_or_else(|| "none".to_owned());
                let path = |text: String| {
                    div()
                        .max_w(px(260.))
                        .overflow_hidden()
                        .line_clamp(1)
                        .text_ellipsis()
                        .text_xs()
                        .font_family(MONO)
                        .text_color(rgb(t().muted))
                        .child(SharedString::from(text))
                };
                vec![card(
                    None,
                    vec![
                        row(
                            "Clone into",
                            "The folder new clones go into. Each clone gets a folder of its own inside it.",
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(path(folder.display().to_string()))
                                .child(
                                    button("setting-clone-folder", "Change…")
                                        .on_click(cx.listener(|this, _, _, cx| this.choose_default_clone_folder(cx))),
                                ),
                        ),
                        row(
                            "Data folder",
                            "Projects, settings, comments and pictures are kept here. Set GITGUI_DATA_DIR to move it.",
                            path(data),
                        ),
                    ],
                )]
            }
        }
    }

    /// Every theme found, as a card with a small preview of its colors; click one to use it.
    fn theme_cards(&self, card_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let current = t().name.clone();
        let where_from = |origin: &Origin| match origin {
            Origin::BuiltIn => "built in",
            Origin::User(_) => "your themes",
            Origin::Zed(_) => "from Zed",
        };
        let cards = self.themes.iter().enumerate().map(|(ix, theme)| {
            let chosen = theme.name == current;
            let name = theme.name.clone();
            div()
                .id(("theme", ix))
                .w(px(card_w))
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .rounded_lg()
                .border_2()
                .border_color(rgb(if chosen { t().accent } else { t().border }))
                .bg(rgb(t().card))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(t().hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.set_theme(&name, cx)))
                .child(
                    // A miniature of the theme: its editor color with its accent, syntax, added and removed colors.
                    div()
                        .h(px(34.))
                        .rounded_md()
                        .bg(rgb(theme.editor_bg))
                        .border_1()
                        .border_color(rgb(theme.border))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .children([theme.accent, theme.syntax_hint(), theme.added, theme.removed].map(|color| {
                            div().h(px(8.)).w(px(26.)).rounded_full().bg(rgb(color))
                        })),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .line_clamp(1)
                                .text_ellipsis()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(t().text_strong))
                                .child(theme.name.clone()),
                        )
                        .when(chosen, |line| line.child(div().flex_none().text_color(rgb(t().accent)).font_weight(FontWeight::BOLD).child("✓"))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(t().muted))
                        .child(format!("{} · {}", if theme.dark { "dark" } else { "light" }, where_from(&theme.origin))),
                )
        });
        let folder = self.store_dir().map(|dir| dir.join("themes").display().to_string()).unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().px_1().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(t().muted)).child("COLOR THEME"))
            .child(div().flex().flex_wrap().gap_2().children(cards))
            .child(div().px_1().text_xs().text_color(rgb(t().muted)).child(format!(
                "Zed themes work as they are: themes installed in Zed show up here, or put a Zed theme file in {folder}."
            )))
            .into_any_element()
    }

    /// The built-in icons, then each icon theme installed in Zed.
    fn icon_cards(&self, card_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let current = self.settings.icon_theme.clone();
        let names: Vec<Option<String>> =
            std::iter::once(None).chain(self.icon_themes.iter().map(|t| Some(t.name.clone()))).collect();
        let cards = names.into_iter().enumerate().map(|(ix, name)| {
            let chosen = name == current;
            let label = name.clone().unwrap_or_else(|| icons::BUILT_IN.to_owned());
            div()
                .id(("icon-theme", ix))
                .w(px(card_w))
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_2()
                .border_color(rgb(if chosen { t().accent } else { t().border }))
                .bg(rgb(t().card))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(t().hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.set_icon_theme(name.as_deref(), cx)))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .line_clamp(1)
                        .text_ellipsis()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(t().text_strong))
                        .child(label),
                )
                .when(ix > 0, |card| card.child(div().flex_none().text_xs().text_color(rgb(t().muted)).child("from Zed")))
                .when(chosen, |card| card.child(div().flex_none().text_color(rgb(t().accent)).font_weight(FontWeight::BOLD).child("✓")))
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().px_1().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(t().muted)).child("FILE ICONS"))
            .child(div().flex().flex_wrap().gap_2().children(cards))
            .child(div().px_1().text_xs().text_color(rgb(t().muted)).child("Icon themes installed in Zed (Material, Catppuccin, …) show up here."))
            .into_any_element()
    }

    /// Picks the folder new clones go into.
    pub fn choose_default_clone_folder(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(folder) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| {
                this.settings.clone_dir = Some(folder);
                this.save_settings();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
