//! GitHub inside the window: signing in, and a repository's pull requests and issues next to its graph.
//!
//! Everything here only reads. The sign-in is GitHub's device flow (see `gitgui-forge`): the window shows a code, the
//! person approves it on github.com, and the token it gets back lives in the system's credential store.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gitgui_core::{CheckoutTarget, Host, default_remote};
use gitgui_forge::{self as forge, Api, DeviceCode, Error, Fixture, Http, Issue, Poll, Pull, PullState, Token, Transport};
use gpui::{ClipboardItem, Context};

use crate::menu::{Action, Dialog, Notice};
use crate::workspace::{Phase, Workspace};

/// A list read less than this long ago is not read again by a plain refresh.
const FRESH: Duration = Duration::from_secs(60);

/// What the middle of the window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MainTab {
    #[default]
    Graph,
    Pulls,
    Issues,
}

/// Which items of a list show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListFilter {
    #[default]
    Open,
    /// Opened by the signed-in person (for issues, or assigned to them).
    Mine,
    /// A pull request whose review the signed-in person is asked for.
    Review,
    /// Closed, or merged.
    Closed,
}

/// A project's pull requests and issues, as last read.
pub enum Lists {
    Loading,
    Ready { pulls: Vec<Pull>, issues: Vec<Issue>, at: Instant },
    Failed(Error),
}

/// A sign-in that is waiting for the person to approve a code on github.com.
pub struct Flow {
    pub code: DeviceCode,
    cancel: Arc<AtomicBool>,
}

pub struct GithubState {
    transport: Arc<dyn Transport + Send + Sync>,
    /// The answers are not GitHub's (`GITGUI_GITHUB_FIXTURE=folder`, or a test): the credential store is left alone.
    pub fixture: bool,
    pub token: Option<Token>,
    pub login: Option<String>,
    /// The credential store has been asked for a saved sign-in this run.
    asked_store: bool,
    pub flow: Option<Flow>,
    pub lists: HashMap<PathBuf, Lists>,
    pub tab: MainTab,
    pub filter: ListFilter,
    /// The number of the pull request or issue open in the detail panel.
    pub selected: Option<u32>,
}

impl GithubState {
    pub fn new() -> Self {
        let blank = |transport: Arc<dyn Transport + Send + Sync>, fixture: bool, token: Option<Token>| Self {
            transport,
            fixture,
            asked_store: fixture,
            token,
            login: None,
            flow: None,
            lists: HashMap::new(),
            tab: MainTab::Graph,
            filter: ListFilter::Open,
            selected: None,
        };
        match std::env::var_os("GITGUI_GITHUB_FIXTURE").map(PathBuf::from) {
            Some(dir) => blank(
                Arc::new(Fixture(dir)),
                true,
                Some(Token { access: "fixture".into(), refresh: None, expires_at: None, refresh_expires_at: None }),
            ),
            None => blank(Arc::new(Http), false, None),
        }
    }

    pub fn signed_in(&self) -> bool {
        self.token.is_some()
    }

    /// For tests: signed in (or not), answered by `transport`, and the credential store never touched.
    #[cfg(test)]
    pub(crate) fn with_transport(transport: Arc<dyn Transport + Send + Sync>, signed_in: bool) -> Self {
        let mut state = Self::new();
        state.transport = transport;
        state.fixture = true;
        state.asked_store = true;
        state.token = signed_in.then(|| Token { access: "test".into(), refresh: None, expires_at: None, refresh_expires_at: None });
        state
    }
}

/// How long ago, in a few words.
pub fn ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then);
    match seconds {
        0..=59 => "just now".to_owned(),
        60..=3_599 => format!("{} min ago", seconds / 60),
        3_600..=86_399 => format!("{} h ago", seconds / 3_600),
        86_400..=2_591_999 => format!("{} d ago", seconds / 86_400),
        _ => format!("{} mo ago", seconds / 2_592_000),
    }
}

fn is_open(state: PullState) -> bool {
    matches!(state, PullState::Open | PullState::Draft)
}

/// The pull requests `filter` shows, the ones changed last first. `me` is the signed-in login.
pub fn visible_pulls<'a>(pulls: &'a [Pull], filter: ListFilter, me: Option<&str>) -> Vec<&'a Pull> {
    let is_me = |name: &str| me.is_some_and(|me| me.eq_ignore_ascii_case(name));
    let mut found: Vec<&Pull> = pulls
        .iter()
        .filter(|pull| match filter {
            ListFilter::Open => is_open(pull.state),
            ListFilter::Mine => is_open(pull.state) && is_me(&pull.author),
            ListFilter::Review => is_open(pull.state) && pull.reviewers.iter().any(|name| is_me(name)),
            ListFilter::Closed => !is_open(pull.state),
        })
        .collect();
    found.sort_by_key(|pull| std::cmp::Reverse(pull.updated));
    found
}

/// The issues `filter` shows, the ones changed last first. (Review means nothing for an issue: it shows the open ones.)
pub fn visible_issues<'a>(issues: &'a [Issue], filter: ListFilter, me: Option<&str>) -> Vec<&'a Issue> {
    let is_me = |name: &str| me.is_some_and(|me| me.eq_ignore_ascii_case(name));
    let mut found: Vec<&Issue> = issues
        .iter()
        .filter(|issue| match filter {
            ListFilter::Open | ListFilter::Review => issue.open,
            ListFilter::Mine => issue.open && (is_me(&issue.author) || issue.assignees.iter().any(|name| is_me(name))),
            ListFilter::Closed => !issue.open,
        })
        .collect();
    found.sort_by_key(|issue| std::cmp::Reverse(issue.updated));
    found
}

/// What the sidebar lists first: open pull requests waiting for the person's review, then theirs, then the rest.
pub fn most_relevant_pulls<'a>(pulls: &'a [Pull], me: Option<&str>, most: usize) -> Vec<&'a Pull> {
    let is_me = |name: &str| me.is_some_and(|me| me.eq_ignore_ascii_case(name));
    let mut open = visible_pulls(pulls, ListFilter::Open, me);
    open.sort_by_key(|pull| {
        let rank = if pull.reviewers.iter().any(|name| is_me(name)) {
            0
        } else if is_me(&pull.author) {
            1
        } else {
            2
        };
        (rank, std::cmp::Reverse(pull.updated))
    });
    open.truncate(most);
    open
}

/// What a read of one repository brings back.
struct Fetched {
    /// A refreshed token, when the old one had run out (already kept in the credential store).
    renewed: Option<Token>,
    login: Option<String>,
    pulls: Vec<Pull>,
    issues: Vec<Issue>,
}

fn fetch(transport: &dyn Transport, mut token: Token, owner: &str, name: &str, need_login: bool, persist: bool) -> Result<Fetched, Error> {
    let now = forge::now();
    let mut renewed = None;
    if token.needs_refresh(now) {
        token = forge::token::refresh(transport, forge::CLIENT_ID, &token, now)?;
        // The old refresh token is dead the moment a new one is issued: keep the pair before anything else.
        if persist {
            let _ = forge::vault::save(&token);
        }
        renewed = Some(token.clone());
    }
    let api = Api::new(transport, &token.access);
    let login = if need_login { Some(api.user()?) } else { None };
    Ok(Fetched { renewed, login, pulls: api.pulls(owner, name)?, issues: api.issues(owner, name)? })
}

impl Workspace {
    /// The open project's place on GitHub: its path here, and the owner and name there. `None` for a project that is
    /// not on GitHub (or not read yet).
    pub(crate) fn github_repo(&self) -> Option<(PathBuf, String, String)> {
        let repo = self.repo.as_ref()?;
        let Phase::Ready(view) = &repo.phase else { return None };
        let web = view.web.as_ref().filter(|web| web.host == Host::GitHub)?;
        let rest = web.base.strip_prefix("https://github.com/")?;
        let (owner, name) = rest.split_once('/')?;
        Some((repo.project.path.clone(), owner.to_owned(), name.trim_end_matches('/').to_owned()))
    }

    /// A project has been read: if it is on GitHub, read its lists (asking the credential store for a saved sign-in
    /// the first time).
    pub(crate) fn github_after_read(&mut self, cx: &mut Context<Self>) {
        if self.github_repo().is_none() {
            self.github.tab = MainTab::Graph;
            self.github.selected = None;
            return;
        }
        if self.github.asked_store {
            return self.github_refresh(false, cx);
        }
        self.github.asked_store = true;
        self.spawn_load(cx, forge::vault::load, |this, saved, cx| {
            if let Ok(Some(token)) = saved
                && this.github.token.is_none()
            {
                this.github.token = Some(token);
            }
            this.github_refresh(false, cx);
            cx.notify();
        });
    }

    /// Reads the open project's pull requests and issues again (not if they were read a minute ago, unless `force`).
    pub fn github_refresh(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some((path, owner, name)) = self.github_repo() else { return };
        let Some(token) = self.github.token.clone() else { return };
        match self.github.lists.get(&path) {
            Some(Lists::Loading) => return,
            Some(Lists::Ready { at, .. }) if !force && at.elapsed() < FRESH => return,
            Some(Lists::Ready { .. }) => {}
            _ => {
                self.github.lists.insert(path.clone(), Lists::Loading);
            }
        }
        let transport = self.github.transport.clone();
        let need_login = self.github.login.is_none();
        let persist = !self.github.fixture;
        cx.notify();
        self.spawn_load(
            cx,
            move || fetch(&*transport, token, &owner, &name, need_login, persist),
            move |this, result, cx| {
                match result {
                    Ok(found) => {
                        if let Some(token) = found.renewed {
                            this.github.token = Some(token);
                        }
                        if let Some(login) = found.login {
                            this.github.login = Some(login);
                        }
                        this.github.lists.insert(path, Lists::Ready { pulls: found.pulls, issues: found.issues, at: Instant::now() });
                    }
                    Err(Error::SignedOut) => {
                        this.github_forget_token(cx);
                        this.github.lists.remove(&path);
                        this.say(Notice::warn("Your GitHub sign-in has ended. Sign in again to see pull requests and issues."), cx);
                    }
                    Err(error) => {
                        this.github.lists.insert(path, Lists::Failed(error));
                    }
                }
                cx.notify();
            },
        );
    }

    // ---- signing in and out -------------------------------------------------------------------------------

    /// Asks GitHub for a code, opens the page where it is entered, and waits for the approval in the background.
    pub fn github_sign_in(&mut self, cx: &mut Context<Self>) {
        if self.github.flow.is_some() {
            return;
        }
        self.busy = Some("Asking GitHub for a code…".into());
        cx.notify();
        let transport = self.github.transport.clone();
        self.spawn_load(
            cx,
            move || forge::device::request_code(&*transport, forge::CLIENT_ID),
            |this, result, cx| {
                this.busy = None;
                match result {
                    Ok(code) => this.github_wait_for_approval(code, cx),
                    Err(error) => this.fail(error, cx),
                }
                cx.notify();
            },
        );
    }

    fn github_wait_for_approval(&mut self, code: DeviceCode, cx: &mut Context<Self>) {
        // The code is on the clipboard before the page opens, so the first thing to do there is paste.
        cx.write_to_clipboard(ClipboardItem::new_string(code.user_code.clone()));
        cx.open_url(&code.verification_uri);
        let cancel = Arc::new(AtomicBool::new(false));
        self.github.flow = Some(Flow { code: code.clone(), cancel: cancel.clone() });
        self.dialog = Some(Dialog {
            title: "Sign in to GitHub".into(),
            body: format!(
                "Enter this code on the GitHub page that just opened:\n\n{}\n\nIt is copied: paste it there. GitHub then asks \
                 you to approve gitgui, which can read the pull requests and issues of the repositories you choose. It cannot \
                 read your code or change anything.",
                code.user_code
            )
            .into(),
            confirm: "Open GitHub again".into(),
            danger: false,
            prompt: None,
            folder: None,
            check: None,
            action: Action::GithubSignIn,
        });
        let transport = self.github.transport.clone();
        let stop = cancel.clone();
        self.spawn_load(
            cx,
            move || wait_for_token(&*transport, &code, &stop),
            move |this, result, cx| {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                this.github.flow = None;
                if this.dialog.as_ref().is_some_and(|dialog| dialog.action == Action::GithubSignIn) {
                    this.dialog = None;
                }
                match result {
                    Ok(token) => this.github_signed_in(token, cx),
                    Err(error) => this.fail(error, cx),
                }
                cx.notify();
            },
        );
    }

    /// "Open GitHub again" in the sign-in window: the code is on the clipboard once more, and the page opens.
    pub(crate) fn github_reopen(&mut self, cx: &mut Context<Self>) {
        if let Some(flow) = &self.github.flow {
            cx.write_to_clipboard(ClipboardItem::new_string(flow.code.user_code.clone()));
            cx.open_url(&flow.code.verification_uri);
            self.busy = Some(format!("Waiting for you to approve {} on github.com…", flow.code.user_code).into());
            cx.notify();
        }
    }

    /// Stops waiting for an approval.
    pub fn github_cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        if let Some(flow) = self.github.flow.take() {
            flow.cancel.store(true, Ordering::Relaxed);
        }
        if self.busy.as_ref().is_some_and(|busy| busy.contains("on github.com")) {
            self.busy = None;
        }
        cx.notify();
    }

    fn github_signed_in(&mut self, token: Token, cx: &mut Context<Self>) {
        self.busy = None;
        let kept = token.clone();
        self.github.token = Some(token);
        self.github.login = None;
        self.github.lists.clear();
        if !self.github.fixture {
            self.spawn_load(cx, move || forge::vault::save(&kept), |this, saved, cx| {
                if let Err(why) = saved {
                    this.say(Notice::warn(format!("You're signed in, but the sign-in couldn't be kept for next time: {why}")), cx);
                }
            });
        }
        self.github_refresh(true, cx);
        self.say(Notice::info("Signed in to GitHub."), cx);
    }

    /// Forgets the sign-in here and in the credential store.
    pub fn github_sign_out(&mut self, cx: &mut Context<Self>) {
        self.github_cancel_sign_in(cx);
        self.github_forget_token(cx);
        self.github.tab = MainTab::Graph;
        self.say(Notice::info("Signed out of GitHub."), cx);
    }

    fn github_forget_token(&mut self, cx: &mut Context<Self>) {
        self.github.token = None;
        self.github.login = None;
        self.github.lists.clear();
        self.github.selected = None;
        if !self.github.fixture {
            self.spawn_load(cx, forge::vault::delete, |_, _, _| {});
        }
    }

    // ---- the lists --------------------------------------------------------------------------------------

    pub fn github_set_tab(&mut self, tab: MainTab, cx: &mut Context<Self>) {
        if self.github.tab != tab {
            self.github.tab = tab;
            self.github.selected = None;
            self.github.filter = ListFilter::Open;
            if tab != MainTab::Graph {
                self.github_refresh(false, cx);
            }
            cx.notify();
        }
    }

    pub fn github_set_filter(&mut self, filter: ListFilter, cx: &mut Context<Self>) {
        self.github.filter = filter;
        self.github.selected = None;
        cx.notify();
    }

    pub fn github_select(&mut self, number: Option<u32>, cx: &mut Context<Self>) {
        self.github.selected = number;
        cx.notify();
    }

    /// The open project's lists, once read.
    pub(crate) fn github_lists(&self) -> Option<&Lists> {
        let (path, ..) = self.github_repo()?;
        self.github.lists.get(&path)
    }

    fn github_pull(&self, number: u32) -> Option<&Pull> {
        match self.github_lists()? {
            Lists::Ready { pulls, .. } => pulls.iter().find(|pull| pull.number == number),
            _ => None,
        }
    }

    /// Back to the graph, showing that pull request's branch with the one it goes into.
    pub fn github_show_in_graph(&mut self, number: u32, cx: &mut Context<Self>) {
        let Some(pull) = self.github_pull(number).cloned() else { return };
        let known = match self.repo.as_ref().map(|repo| &repo.phase) {
            Some(Phase::Ready(view)) => view
                .commits
                .iter()
                .any(|c| c.refs.iter().any(|r| r.name == pull.head || r.name.split_once('/').is_some_and(|(_, rest)| rest == pull.head))),
            _ => false,
        };
        if !known {
            return self.say(Notice::warn(format!("{} isn't in this history yet. Fetch, then try again.", pull.head)), cx);
        }
        self.github.tab = MainTab::Graph;
        self.github.selected = None;
        self.isolate_branch(&pull.head, false, cx);
    }

    /// Checks out the pull request's branch from the remote (after the usual look at what that does).
    pub fn github_checkout(&mut self, number: u32, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(pull) = self.github_pull(number).cloned() else { return };
        let remote = match self.repo.as_ref().map(|repo| &repo.phase) {
            Some(Phase::Ready(view)) => default_remote(&view.remotes).map(str::to_owned),
            _ => None,
        };
        let Some(remote) = remote.filter(|_| pull.same_repo) else {
            return self.say(Notice::warn("That branch is in someone's fork, so it can't be checked out from this remote."), cx);
        };
        self.choose(Action::Checkout(CheckoutTarget::RemoteBranch(format!("{remote}/{}", pull.head))), window, cx);
    }

    pub fn github_open_url(&self, url: &str, cx: &mut Context<Self>) {
        cx.open_url(url);
    }
}

/// Asks GitHub whether the code was approved, as often as it allows, until it was, was refused, or ran out; or until
/// `stop` is set.
fn wait_for_token(transport: &dyn Transport, code: &DeviceCode, stop: &AtomicBool) -> Result<Token, Error> {
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = code.interval;
    let mut failures = 0;
    loop {
        // In one-second steps, so a cancel is noticed at once.
        for _ in 0..interval {
            if stop.load(Ordering::Relaxed) {
                return Err(Error::Denied);
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        if Instant::now() >= deadline {
            return Err(Error::Expired);
        }
        match forge::device::poll(transport, forge::CLIENT_ID, code, forge::now()) {
            Ok(Poll::Pending) => failures = 0,
            Ok(Poll::SlowDown { interval: wait }) => interval = wait,
            Ok(Poll::Done(token)) => return Ok(token),
            // A dropped connection is not the end of the wait; five in a row is.
            Err(Error::Network(_)) if failures < 5 => failures += 1,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull(number: u32, state: PullState, author: &str, reviewers: &[&str], updated: u64) -> Pull {
        Pull {
            number,
            title: format!("pull {number}"),
            state,
            author: author.into(),
            head: format!("feat/{number}"),
            base: "main".into(),
            same_repo: true,
            updated,
            url: String::new(),
            body: String::new(),
            labels: Vec::new(),
            reviewers: reviewers.iter().map(|r| (*r).to_owned()).collect(),
        }
    }

    #[test]
    fn a_time_is_told_in_a_few_words() {
        assert_eq!(ago(100, 130), "just now");
        assert_eq!(ago(0, 300), "5 min ago");
        assert_eq!(ago(0, 7_300), "2 h ago");
        assert_eq!(ago(0, 3 * 86_400 + 5), "3 d ago");
        assert_eq!(ago(0, 90 * 86_400), "3 mo ago");
        assert_eq!(ago(500, 100), "just now", "a clock a little behind is not the future");
    }

    #[test]
    fn the_filters_pick_open_mine_review_and_closed() {
        let pulls = vec![
            pull(1, PullState::Open, "kim", &["rom"], 30),
            pull(2, PullState::Draft, "rom", &[], 20),
            pull(3, PullState::Merged, "rom", &[], 40),
            pull(4, PullState::Open, "lee", &[], 10),
        ];
        let numbers = |filter| visible_pulls(&pulls, filter, Some("Rom")).iter().map(|p| p.number).collect::<Vec<_>>();
        assert_eq!(numbers(ListFilter::Open), [1, 2, 4], "newest change first, drafts are open");
        assert_eq!(numbers(ListFilter::Mine), [2]);
        assert_eq!(numbers(ListFilter::Review), [1]);
        assert_eq!(numbers(ListFilter::Closed), [3]);
        assert!(visible_pulls(&pulls, ListFilter::Mine, None).is_empty(), "nobody is signed in");
    }

    #[test]
    fn the_sidebar_lists_what_waits_for_the_person_first() {
        let pulls = vec![
            pull(1, PullState::Open, "lee", &[], 90),
            pull(2, PullState::Open, "rom", &[], 50),
            pull(3, PullState::Open, "kim", &["rom"], 10),
            pull(4, PullState::Merged, "rom", &[], 99),
        ];
        let first: Vec<u32> = most_relevant_pulls(&pulls, Some("rom"), 2).iter().map(|p| p.number).collect();
        assert_eq!(first, [3, 2], "asked for a review, then their own; the merged one is not open");
    }

    #[test]
    fn issues_are_mine_when_opened_or_assigned() {
        let issue = |number, open, author: &str, assignees: &[&str]| Issue {
            number,
            title: String::new(),
            open,
            author: author.into(),
            updated: u64::from(number),
            url: String::new(),
            body: String::new(),
            labels: Vec::new(),
            assignees: assignees.iter().map(|a| (*a).to_owned()).collect(),
            comments: 0,
        };
        let issues = vec![issue(1, true, "rom", &[]), issue(2, true, "kim", &["rom"]), issue(3, true, "kim", &[]), issue(4, false, "rom", &[])];
        let mine: Vec<u32> = visible_issues(&issues, ListFilter::Mine, Some("rom")).iter().map(|i| i.number).collect();
        assert_eq!(mine, [2, 1]);
        assert_eq!(visible_issues(&issues, ListFilter::Closed, Some("rom")).len(), 1);
    }
}
