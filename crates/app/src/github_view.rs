//! The look of GitHub in the window: sections under the open project, the tabs beside the graph, the lists, and
//! the sign-in card in Settings. The doing is in `github_ui`.

use gitgui_forge::{self as forge, Error, Issue, Label, Pull, PullState};
use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px, rgb};

use crate::github_ui::{ListFilter, Lists, MainTab, ago, most_relevant_pulls, visible_issues, visible_pulls};
use crate::theme::t;
use crate::ui::{self, PR_COLOR, button, ghost};
use crate::workspace::Workspace;

/// How many items a project's section in the sidebar lists before "View all".
const SIDEBAR_ITEMS: usize = 4;

fn dot(color: u32, hollow: bool) -> impl IntoElement {
    let dot = div().flex_none().size(px(8.)).rounded_full();
    if hollow { dot.border_1().border_color(rgb(color)) } else { dot.bg(rgb(color)) }
}

fn pull_dot(state: PullState) -> impl IntoElement {
    match state {
        PullState::Open => dot(t().added, false),
        PullState::Draft => dot(t().muted, true),
        PullState::Merged => dot(PR_COLOR, false),
        PullState::Closed => dot(t().removed, false),
    }
}

fn one_line(text: impl Into<SharedString>) -> gpui::Div {
    div().min_w_0().overflow_hidden().line_clamp(1).text_ellipsis().child(text.into())
}

/// A sentence for a failed read.
fn failure_text(error: &Error, owner: &str, name: &str) -> String {
    match error {
        Error::NoAccess => format!("gitgui can't see {owner}/{name} yet: the GitHub app isn't installed on it."),
        other => other.to_string(),
    }
}

/// The same, for the middle of the window, where there is room to say what to do about it.
fn failure_detail(error: &Error, owner: &str, name: &str) -> String {
    match error {
        Error::NoAccess => format!(
            "gitgui can't see {owner}/{name} yet. Signing in only says who you are: the GitHub app also has to be installed on \
             {owner}, with this repository chosen. If {owner} is an organization, one of its owners has to approve it."
        ),
        other => other.to_string(),
    }
}

impl Workspace {
    fn now(&self) -> u64 {
        forge::now()
    }

    // ---- the sidebar ------------------------------------------------------------------------------------

    /// Pull requests and issues, as two short sections under the open project; `None` when it is not on GitHub.
    pub(crate) fn render_github_sidebar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, owner, name) = self.github_repo()?;
        let header = |title: &'static str, count: Option<usize>| {
            div()
                .h(px(24.))
                .pl(px(18.))
                .pr_2()
                .flex()
                .items_center()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(t().muted))
                .child(match count {
                    Some(n) => format!("{title}  {n}"),
                    None => title.to_owned(),
                })
        };
        let quiet = |text: String| div().pl(px(18.)).pr_2().pb_1().text_xs().text_color(rgb(t().muted)).child(text);
        let mut section = div().flex().flex_col().pb_1();
        // The two sections say what they are; the heading is for the states with nothing listed.
        if !matches!(self.github_lists(), Some(Lists::Ready { .. })) || !self.github.signed_in() {
            section = section.child(header("GITHUB", None));
        }

        if !self.github.signed_in() {
            section = section.child(quiet("Sign in to see this repository's pull requests and issues.".to_owned())).child(
                div().pl(px(14.)).flex().child(
                    ghost("github-side-sign-in", "Sign in with GitHub")
                        .debug_selector(|| "github-side-sign-in".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.github_sign_in(cx))),
                ),
            );
            return Some(section.into_any_element());
        }
        match self.github_lists() {
            None | Some(Lists::Loading) => section = section.child(quiet("Reading GitHub…".to_owned())),
            Some(Lists::Failed(error)) => {
                section = section.child(quiet(failure_text(error, &owner, &name))).child(
                    div().pl(px(14.)).flex().child(match error {
                        Error::NoAccess => ghost("github-side-install", "Install on GitHub")
                            .debug_selector(|| "github-side-install".to_owned())
                            .on_click(cx.listener(|this, _, _, cx| this.github_open_url(&forge::install_url(), cx)))
                            .into_any_element(),
                        _ => ghost("github-side-retry", "Try again")
                            .on_click(cx.listener(|this, _, _, cx| this.github_refresh(true, cx)))
                            .into_any_element(),
                    }),
                );
            }
            Some(Lists::Ready { pulls, issues, .. }) => {
                let me = self.github.login.as_deref();
                let open_pulls = visible_pulls(pulls, ListFilter::Open, me).len();
                let open_issues = visible_issues(issues, ListFilter::Open, me).len();
                section = section.child(header("PULL REQUESTS", Some(open_pulls)));
                for pull in most_relevant_pulls(pulls, me, SIDEBAR_ITEMS) {
                    let number = pull.number;
                    section = section.child(
                        div()
                            .id(("side-pull", number as usize))
                            .h(px(22.))
                            .pl(px(18.))
                            .pr_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(t().hover)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.github_set_tab(MainTab::Pulls, cx);
                                this.github_select(Some(number), cx);
                            }))
                            .child(pull_dot(pull.state))
                            .child(one_line(format!("#{number} {}", pull.title))),
                    );
                }
                if open_pulls > SIDEBAR_ITEMS {
                    section = section.child(self.view_all("side-all-pulls", MainTab::Pulls, cx));
                }
                section = section.child(header("ISSUES", Some(open_issues)));
                let mut recent: Vec<&Issue> = visible_issues(issues, ListFilter::Open, me);
                recent.truncate(SIDEBAR_ITEMS);
                for issue in recent {
                    let number = issue.number;
                    section = section.child(
                        div()
                            .id(("side-issue", number as usize))
                            .h(px(22.))
                            .pl(px(18.))
                            .pr_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(t().hover)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.github_set_tab(MainTab::Issues, cx);
                                this.github_select(Some(number), cx);
                            }))
                            .child(dot(t().added, true))
                            .child(one_line(format!("#{number} {}", issue.title))),
                    );
                }
                if open_issues > SIDEBAR_ITEMS {
                    section = section.child(self.view_all("side-all-issues", MainTab::Issues, cx));
                }
            }
        }
        Some(section.into_any_element())
    }

    fn view_all(&self, id: &'static str, tab: MainTab, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(id)
            .h(px(22.))
            .pl(px(18.))
            .pr_2()
            .flex()
            .items_center()
            .text_xs()
            .text_color(rgb(t().accent))
            .cursor_pointer()
            .hover(|style| style.text_color(rgb(t().text_strong)))
            .on_click(cx.listener(move |this, _, _, cx| this.github_set_tab(tab, cx)))
            .child("View all")
            .into_any_element()
    }

    // ---- the tabs ---------------------------------------------------------------------------------------

    /// `Graph | Pull requests | Issues` above the graph, for a project that is on GitHub.
    pub(crate) fn render_main_tabs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.github_repo()?;
        let counts = match self.github_lists() {
            Some(Lists::Ready { pulls, issues, .. }) => {
                let me = self.github.login.as_deref();
                Some((visible_pulls(pulls, ListFilter::Open, me).len(), visible_issues(issues, ListFilter::Open, me).len()))
            }
            _ => None,
        };
        let tab = |id: &'static str, label: String, this: MainTab| {
            ui::toggle(id, label, self.github.tab == this)
                .debug_selector(move || id.to_owned())
                .on_click(cx.listener(move |workspace, _, _, cx| workspace.github_set_tab(this, cx)))
        };
        Some(
            div()
                .flex_none()
                .h(px(32.))
                .px_3()
                .flex()
                .items_center()
                .gap_1()
                .border_b_1()
                .border_color(rgb(t().border))
                .child(tab("tab-graph", "Graph".to_owned(), MainTab::Graph))
                .child(tab(
                    "tab-pulls",
                    counts.map_or_else(|| "Pull requests".to_owned(), |(pulls, _)| format!("Pull requests  {pulls}")),
                    MainTab::Pulls,
                ))
                .child(tab(
                    "tab-issues",
                    counts.map_or_else(|| "Issues".to_owned(), |(_, issues)| format!("Issues  {issues}")),
                    MainTab::Issues,
                ))
                .into_any_element(),
        )
    }

    // ---- the lists --------------------------------------------------------------------------------------

    /// An invisible child that tells the workspace where the lists begin and end, so a drag of the divider knows how wide
    /// the panel has to be to meet the pointer, and how much the list must keep.
    fn github_right_edge(&self) -> AnyElement {
        let edges = self.github.edges.clone();
        gpui::canvas(
            move |bounds, _, _| edges.set((f32::from(bounds.origin.x), f32::from(bounds.origin.x + bounds.size.width))),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    /// What the middle shows instead of the graph while the Pull requests or Issues tab is chosen.
    pub(crate) fn render_github_main(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((_, owner, name)) = self.github_repo() else { return div().into_any_element() };
        let centered = |content: AnyElement| div().flex_1().min_h_0().flex().items_center().justify_center().child(content).into_any_element();

        if !self.github.signed_in() {
            return centered(
                div()
                    .max_w(px(420.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(div().text_base().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child("Sign in to GitHub"))
                    .child(
                        div().text_center().text_color(rgb(t().muted)).child(
                            "See this repository's pull requests and issues here. gitgui can read them and nothing else: \
                             not your code, and it changes nothing.",
                        ),
                    )
                    .child(
                        button("github-sign-in", "Sign in with GitHub")
                            .debug_selector(|| "github-sign-in".to_owned())
                            .bg(rgb(t().accent))
                            .text_color(rgb(t().on_accent))
                            .on_click(cx.listener(|this, _, _, cx| this.github_sign_in(cx))),
                    )
                    .into_any_element(),
            );
        }
        let (pulls, issues) = match self.github_lists() {
            None | Some(Lists::Loading) => return centered(div().text_color(rgb(t().muted)).child("Reading GitHub…").into_any_element()),
            Some(Lists::Failed(error)) => {
                let action = match error {
                    Error::NoAccess => button("github-install", "Install on GitHub")
                        .on_click(cx.listener(|this, _, _, cx| this.github_open_url(&forge::install_url(), cx))),
                    _ => button("github-retry", "Try again").on_click(cx.listener(|this, _, _, cx| this.github_refresh(true, cx))),
                };
                return centered(
                    div()
                        .max_w(px(460.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_3()
                        .child(div().text_center().text_color(rgb(t().muted)).child(failure_detail(error, &owner, &name)))
                        .child(action)
                        .into_any_element(),
                );
            }
            Some(Lists::Ready { pulls, issues, .. }) => (pulls, issues),
        };

        let me = self.github.login.as_deref();
        let is_pulls = self.github.tab == MainTab::Pulls;
        let filter = self.github.filter;
        let chip = |id: &'static str, label: String, this: ListFilter| {
            ui::toggle(id, label, filter == this)
                .debug_selector(move || id.to_owned())
                .on_click(cx.listener(move |workspace, _, _, cx| workspace.github_set_filter(this, cx)))
        };
        let count = |this: ListFilter| if is_pulls { visible_pulls(pulls, this, me).len() } else { visible_issues(issues, this, me).len() };
        let chips = div()
            .flex_none()
            .h(px(36.))
            .px_3()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(rgb(t().border))
            .child(chip("filter-open", format!("Open  {}", count(ListFilter::Open)), ListFilter::Open))
            .child(chip("filter-mine", "Mine".to_owned(), ListFilter::Mine))
            .children(is_pulls.then(|| chip("filter-review", "Needs my review".to_owned(), ListFilter::Review)))
            .child(chip("filter-closed", if is_pulls { "Closed and merged".to_owned() } else { "Closed".to_owned() }, ListFilter::Closed))
            .child(div().flex_1())
            .child(
                ghost("github-reload", "Reload")
                    .debug_selector(|| "github-reload".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.github_refresh(true, cx))),
            );

        let now = self.now();
        let selected = self.github.selected;
        let rows: Vec<AnyElement> = if is_pulls {
            visible_pulls(pulls, filter, me).into_iter().map(|pull| self.pull_row(pull, selected, now, cx)).collect()
        } else {
            visible_issues(issues, filter, me).into_iter().map(|issue| self.issue_row(issue, selected, now, cx)).collect()
        };
        let empty = rows.is_empty();
        let list = div()
            .id("github-list")
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .children(rows)
            .when(empty, |list| list.child(div().p_4().text_color(rgb(t().muted)).child("Nothing here.")));

        let detail = selected.and_then(|number| {
            if is_pulls {
                pulls.iter().find(|pull| pull.number == number).map(|pull| self.pull_detail(pull, now, cx))
            } else {
                issues.iter().find(|issue| issue.number == number).map(|issue| self.issue_detail(issue, now, cx))
            }
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(chips)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.github_right_edge())
                    .child(list)
                    .children(detail.is_some().then(|| self.splitter("github-divider", crate::workspace::Splitter::GithubDetail, cx)))
                    .children(detail),
            )
            .into_any_element()
    }

    fn row_shell(&self, id: (&'static str, usize), selected: bool, number: u32, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex_none()
            .w_full()
            .min_h(px(46.))
            .px_3()
            .py_1()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(t().border))
            .cursor_pointer()
            .when(selected, |row| row.bg(rgb(t().selected)))
            .hover(|style| style.bg(rgb(if selected { t().selected } else { t().hover })))
            .on_click(cx.listener(move |this, _, _, cx| this.github_select(Some(number), cx)))
    }

    fn pull_row(&self, pull: &Pull, selected: Option<u32>, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let number = pull.number;
        let mut meta = format!("{} → {} · {} · {}", pull.head, pull.base, pull.author, ago(pull.updated, now));
        if pull.state == PullState::Draft {
            meta.push_str(" · draft");
        }
        self.row_shell(("pull", number as usize), selected == Some(number), number, cx)
            .debug_selector(move || format!("pull-{number}"))
            .child(pull_dot(pull.state))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(one_line(format!("{}  #{number}", pull.title)).font_weight(FontWeight::MEDIUM).text_color(rgb(t().text_strong)))
                    .child(one_line(meta).text_xs().text_color(rgb(t().muted))),
            )
            .children(pull.labels.iter().take(2).map(label_chip))
            .children(assignee_stack(&pull.assignees))
            .into_any_element()
    }

    fn issue_row(&self, issue: &Issue, selected: Option<u32>, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let number = issue.number;
        let mut meta = format!("{} · {}", issue.author, ago(issue.updated, now));
        if issue.comments > 0 {
            meta.push_str(&format!(" · {} comment{}", issue.comments, if issue.comments == 1 { "" } else { "s" }));
        }
        self.row_shell(("issue", number as usize), selected == Some(number), number, cx)
            .debug_selector(move || format!("issue-{number}"))
            .child(if issue.open { dot(t().added, true).into_any_element() } else { dot(PR_COLOR, false).into_any_element() })
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(one_line(format!("{}  #{number}", issue.title)).font_weight(FontWeight::MEDIUM).text_color(rgb(t().text_strong)))
                    .child(one_line(meta).text_xs().text_color(rgb(t().muted))),
            )
            .children(issue.labels.iter().take(2).map(label_chip))
            .children(assignee_stack(&issue.assignees))
            .into_any_element()
    }

    /// The panel at the right of a list: the title and where the item stands, its properties (who it is assigned to, its
    /// labels, who is to review it), the buttons, and its description read as markdown. It scrolls as one.
    fn detail_shell(&self, title: String, state: (&'static str, u32), meta: String, properties: Vec<AnyElement>, buttons: Vec<AnyElement>, body: &str) -> AnyElement {
        let blocks = crate::markdown::parse(body);
        let (state_name, state_color) = state;
        div()
            .id("github-detail")
            .debug_selector(|| "github-detail".to_owned())
            .flex_none()
            .w(px(self.github.detail_width))
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("github-detail-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_base().font_weight(FontWeight::BOLD).text_color(rgb(t().text_strong)).child(title))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_none()
                                            .px_2()
                                            .h(px(18.))
                                            .flex()
                                            .items_center()
                                            .rounded_full()
                                            .bg(rgb(crate::theme::mix(t().bg, state_color, 0.18)))
                                            .text_color(rgb(state_color))
                                            .text_size(px(11.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(state_name),
                                    )
                                    .child(div().min_w_0().flex_1().text_xs().text_color(rgb(t().muted)).child(meta)),
                            ),
                    )
                    .child(div().flex().flex_col().gap_2().children(properties))
                    .child(div().flex().flex_wrap().gap_2().children(buttons))
                    .child(div().h(px(1.)).bg(rgb(t().border)))
                    .child(if blocks.is_empty() {
                        div().text_color(rgb(t().muted)).child("No description provided.").into_any_element()
                    } else {
                        crate::markdown::render(&blocks)
                    }),
            )
            .into_any_element()
    }

    fn pull_detail(&self, pull: &Pull, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let (number, url) = (pull.number, pull.url.clone());
        let state = match pull.state {
            PullState::Open => ("Open", t().added),
            PullState::Draft => ("Draft", t().muted),
            PullState::Merged => ("Merged", PR_COLOR),
            PullState::Closed => ("Closed", t().removed),
        };
        let mut meta = format!("#{number} · {} opened {}", pull.author, ago(pull.updated, now));
        if pull.comments > 0 {
            meta.push_str(&format!(" · {} comment{}", pull.comments, if pull.comments == 1 { "" } else { "s" }));
        }
        let properties = vec![
            property("Branch", div().text_color(rgb(t().text)).child(format!("{} → {}", pull.head, pull.base)).into_any_element()),
            property("Assignees", people(&pull.assignees, "Unassigned")),
            property("Reviewers", people(&pull.reviewers, "None asked")),
            property("Labels", labels(&pull.labels)),
        ];
        let mut buttons = vec![
            button("github-show-in-graph", "Show in graph")
                .debug_selector(|| "github-show-in-graph".to_owned())
                .bg(rgb(t().accent))
                .text_color(rgb(t().on_accent))
                .on_click(cx.listener(move |this, _, _, cx| this.github_show_in_graph(number, cx)))
                .into_any_element(),
        ];
        if pull.same_repo && matches!(pull.state, PullState::Open | PullState::Draft) {
            buttons.push(
                button("github-checkout", "Check out branch")
                    .debug_selector(|| "github-checkout".to_owned())
                    .on_click(cx.listener(move |this, _, window, cx| this.github_checkout(number, window, cx)))
                    .into_any_element(),
            );
        }
        buttons.push(
            button("github-open", "Open on GitHub")
                .on_click(cx.listener(move |this, _, _, cx| this.github_open_url(&url, cx)))
                .into_any_element(),
        );
        self.detail_shell(pull.title.clone(), state, meta, properties, buttons, &pull.body)
    }

    fn issue_detail(&self, issue: &Issue, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let url = issue.url.clone();
        let state = if issue.open { ("Open", t().added) } else { ("Closed", PR_COLOR) };
        let mut meta = format!("#{} · {} opened {}", issue.number, issue.author, ago(issue.updated, now));
        if issue.comments > 0 {
            meta.push_str(&format!(" · {} comment{}", issue.comments, if issue.comments == 1 { "" } else { "s" }));
        }
        let properties = vec![property("Assignees", people(&issue.assignees, "Unassigned")), property("Labels", labels(&issue.labels))];
        let buttons = vec![
            button("github-open", "Open on GitHub")
                .on_click(cx.listener(move |this, _, _, cx| this.github_open_url(&url, cx)))
                .into_any_element(),
        ];
        self.detail_shell(issue.title.clone(), state, meta, properties, buttons, &issue.body)
    }

    // ---- Settings ---------------------------------------------------------------------------------------

    /// The GitHub page of Settings: who is signed in, and what that allows.
    pub(crate) fn github_settings_cards(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        use crate::settings_view::{card, row};
        let status: AnyElement = match (&self.github.token, &self.github.flow) {
            (Some(_), _) => div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().text_color(rgb(t().text)).child(match &self.github.login {
                    Some(login) => format!("Signed in as {login}"),
                    None => "Signed in".to_owned(),
                }))
                .child(
                    button("github-sign-out", "Sign out")
                        .debug_selector(|| "github-sign-out".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.github_sign_out(cx))),
                )
                .into_any_element(),
            (None, Some(flow)) => div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().text_color(rgb(t().muted)).child(format!("Waiting for you to approve {} on github.com", flow.code.user_code)))
                .child(button("github-cancel", "Cancel").on_click(cx.listener(|this, _, _, cx| this.github_cancel_sign_in(cx))))
                .into_any_element(),
            (None, None) => button("github-sign-in-settings", "Sign in with GitHub")
                .debug_selector(|| "github-sign-in-settings".to_owned())
                .bg(rgb(t().accent))
                .text_color(rgb(t().on_accent))
                .on_click(cx.listener(|this, _, _, cx| this.github_sign_in(cx)))
                .into_any_element(),
        };
        vec![card(
            None,
            vec![
                row(
                    "GitHub account",
                    "Shows a repository's pull requests and issues next to its graph. You approve on github.com with a short code; \
                     no password comes here.",
                    status,
                ),
                row(
                    "What it can do",
                    "Read pull requests and issues, on the repositories you install the app on. It cannot read your code, push, or \
                     change anything on GitHub. The sign-in lasts eight hours at a time and renews itself.",
                    div().text_color(rgb(t().muted)).child("Read only"),
                ),
                row(
                    "Where it is kept",
                    "In your system's credential store (the macOS Keychain, the Windows Credential Manager), never in a file of ours. \
                     Sign out removes it, and so does removing the app at github.com/settings/apps/authorizations.",
                    div().text_color(rgb(t().muted)).child(if self.github.fixture { "Test data" } else { "System keychain" }),
                ),
            ],
        )]
    }
}

/// A label of the repository: its color as a dot, its name in the window's own text (so it reads on every theme).
fn label_chip(label: &Label) -> gpui::Div {
    div()
        .flex_none()
        .px_1p5()
        .h(px(18.))
        .flex()
        .items_center()
        .gap_1()
        .rounded_sm()
        .border_1()
        .border_color(rgb(t().border))
        .text_size(px(11.))
        .text_color(rgb(t().text))
        .child(div().flex_none().size(px(7.)).rounded_full().bg(rgb(label.color)))
        .child(SharedString::from(label.name.clone()))
}

/// One property of an item as a row: a name in a column, and what it is beside.
fn property(name: &'static str, value: AnyElement) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap_2()
        .text_xs()
        .child(div().flex_none().w(px(70.)).pt(px(2.)).text_color(rgb(t().muted)).child(name))
        .child(div().min_w_0().flex_1().child(value))
        .into_any_element()
}

/// People as small chips (their initials on their own color, and their login); `none` says it when there are none.
fn people(logins: &[String], none: &'static str) -> AnyElement {
    if logins.is_empty() {
        return div().pt(px(2.)).text_color(rgb(t().muted)).child(none).into_any_element();
    }
    div()
        .flex()
        .flex_wrap()
        .gap_1()
        .children(logins.iter().map(|login| {
            div()
                .flex_none()
                .pl_0p5()
                .pr_2()
                .h(px(20.))
                .flex()
                .items_center()
                .gap_1()
                .rounded_full()
                .border_1()
                .border_color(rgb(t().border))
                .text_color(rgb(t().text))
                .child(ui::avatar(login, login, crate::avatars::Avatar::None, 16.))
                .child(SharedString::from(login.clone()))
        }))
        .into_any_element()
}

/// The labels as chips, or a word for none.
fn labels(list: &[Label]) -> AnyElement {
    if list.is_empty() {
        return div().pt(px(2.)).text_color(rgb(t().muted)).child("None").into_any_element();
    }
    div().flex().flex_wrap().gap_1().children(list.iter().map(label_chip)).into_any_element()
}

/// A few people as overlapping initials at the end of a list row, with how many more there are.
fn assignee_stack(logins: &[String]) -> Option<AnyElement> {
    if logins.is_empty() {
        return None;
    }
    let shown = logins.iter().take(2);
    let more = logins.len().saturating_sub(2);
    Some(
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .children(shown.map(|login| ui::avatar(login, login, crate::avatars::Avatar::None, 18.)))
            .children((more > 0).then(|| div().text_size(px(11.)).text_color(rgb(t().muted)).child(format!("+{more}"))),)
            .into_any_element(),
    )
}
