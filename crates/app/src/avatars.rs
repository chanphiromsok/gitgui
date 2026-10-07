//! Author pictures: a profile picture when there is one, else the author's initials on a color
//! that is always the same for the same person.
//!
//! A GitHub no-reply email (`123+user@users.noreply.github.com`) gives the GitHub avatar; any other
//! email asks Gravatar, by a SHA-256 of the address. When Gravatar has none and the repository is on
//! GitHub, GitHub itself is asked who made one of that author's commits (through the `gh` command, so
//! your own login covers private repositories): that finds the picture of an author who commits with
//! a plain email address. Pictures are fetched in the background and kept in the data folder, so each
//! is asked for once a week at most; a failed ask (offline, `gh` not signed in) is not remembered.
//! Fetching can be turned off in Settings; the initials need nothing from the network.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::{Image, ImageFormat};
use sha2::{Digest, Sha256};

/// A fetched picture, or the knowledge that there is none, is trusted this long.
const FRESH_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Pictures bigger than this are not kept.
const MAX_BYTES: u64 = 1 << 20;
/// How many pictures are asked for at once. Each ask holds a background thread until the network
/// answers (up to 8 s); scrolling through a history with many authors would otherwise start one per
/// author at the same moment, each with its own connection.
pub const MOST_AT_ONCE: usize = 4;

/// What is known about one email's picture.
#[derive(Clone)]
pub enum Avatar {
    /// Being fetched, or not asked for yet: draw the initials meanwhile.
    Pending,
    /// No picture: draw the initials.
    None,
    Picture(Arc<Image>),
}

/// How asking the network for a picture ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fetched {
    Image(Vec<u8>),
    /// The site answered that it has no such picture (404). Worth remembering for a while.
    Missing,
    /// No answer, or an odd one: offline, timed out, rate limited. Not remembered, so it is tried again.
    Failed,
}

/// One commit an author made in a GitHub repository, to ask GitHub who made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    /// `owner/name`.
    pub repo: String,
    pub sha: String,
}

/// `Ada Lovelace` → `AL`, `octocat` → `O`, `jean-luc picard` → `JP`.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| c.is_whitespace() || c == '-' || c == '_' || c == '.').filter(|w| !w.is_empty()).collect();
    let first = |word: &str| word.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    match words.as_slice() {
        [] => "?".into(),
        [one] => first(one),
        [a, .., b] => format!("{}{}", first(a), first(b)),
    }
}

/// A number that is always the same for the same email, to pick its color.
pub fn hue(email: &str) -> usize {
    email.trim().to_ascii_lowercase().bytes().fold(5381usize, |h, b| h.wrapping_mul(33) ^ b as usize)
}

/// Where the picture for `email` lives: GitHub's for a GitHub no-reply address, else Gravatar's
/// (which answers 404 when it has none).
pub fn url_for(email: &str) -> String {
    let email = email.trim().to_ascii_lowercase();
    if let Some(user) = email.strip_suffix("@users.noreply.github.com") {
        return match user.split_once('+') {
            Some((id, _)) if id.chars().all(|c| c.is_ascii_digit()) => format!("https://avatars.githubusercontent.com/u/{id}?s=64"),
            _ => format!("https://github.com/{}.png?size=64", user.rsplit('+').next().unwrap_or(user)),
        };
    }
    format!("https://gravatar.com/avatar/{}?s=64&d=404", key(&email))
}

/// The cache file name for an email: its SHA-256, so the folder does not list addresses.
fn key(email: &str) -> String {
    Sha256::digest(email.trim().to_ascii_lowercase().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn format_of(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [0xff, 0xd8, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', ..] => Some(ImageFormat::Gif),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        _ => None,
    }
}

fn fresh(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .is_ok_and(|at| SystemTime::now().duration_since(at).is_ok_and(|age| age < FRESH_FOR))
}

/// The picture for `email`: from the cache folder if it is fresh there, else from the network (and
/// then kept). Runs on a background thread. `fetch` and `lookup` are what ask the network, so tests can
/// stand in. `lookup` returns the picture's address for a commit's author, `Ok(None)` when GitHub says
/// the commit's email is not linked to an account, and `Err` when it could not be asked.
pub fn load(
    email: &str,
    dir: Option<&Path>,
    hint: Option<&Hint>,
    fetch: impl Fn(&str) -> Fetched,
    lookup: impl Fn(&Hint) -> Result<Option<String>, ()>,
) -> Avatar {
    let key = key(email);
    let picture = dir.map(|d| d.join(format!("{key}.img")));
    // `.miss` says nothing was found; its text says whether GitHub was asked (`github`) or could not be.
    let miss = dir.map(|d| d.join(format!("{key}.miss")));
    let from = |bytes: Vec<u8>| match format_of(&bytes) {
        Some(format) => Avatar::Picture(Arc::new(Image::from_bytes(format, bytes))),
        None => Avatar::None,
    };
    if let Some(path) = picture.as_ref().filter(|p| fresh(p))
        && let Ok(bytes) = std::fs::read(path)
    {
        return from(bytes);
    }
    if let Some(path) = miss.as_ref().filter(|p| fresh(p)) {
        let asked_github = std::fs::read_to_string(path).is_ok_and(|text| text == "github");
        // Nothing found before, and GitHub could not be asked then but can be now: ask it.
        if asked_github || hint.is_none() {
            return Avatar::None;
        }
    }
    let keep = |bytes: &[u8]| {
        if let Some(dir) = dir {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(dir.join(format!("{key}.img")), bytes);
        }
    };
    let image = |fetched: Fetched| match fetched {
        Fetched::Image(bytes) if format_of(&bytes).is_some() => Some(Some(bytes)),
        Fetched::Failed => None,
        _ => Some(None),
    };

    // Gravatar, or GitHub's own address for a no-reply email.
    match image(fetch(&url_for(email))) {
        None => return Avatar::None,
        Some(Some(bytes)) => {
            keep(&bytes);
            return from(bytes);
        }
        Some(None) => {}
    }
    // A plain email: ask GitHub who made one of this author's commits.
    let mut asked_github = false;
    if let Some(hint) = hint.filter(|_| !email.trim().to_ascii_lowercase().ends_with("@users.noreply.github.com")) {
        match lookup(hint) {
            Err(()) => return Avatar::None,
            Ok(None) => asked_github = true,
            Ok(Some(url)) => match image(fetch(&url)) {
                None => return Avatar::None,
                Some(Some(bytes)) => {
                    keep(&bytes);
                    return from(bytes);
                }
                Some(None) => asked_github = true,
            },
        }
    }
    if let Some(dir) = dir {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join(format!("{key}.miss")), if asked_github { "github" } else { "" });
    }
    Avatar::None
}

/// Asks the network for a picture: an image, "there is none" (404), or no answer.
pub fn fetch(url: &str) -> Fetched {
    // One agent for every ask, so connections (and TLS setup) are reused.
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    let agent = AGENT.get_or_init(|| ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(8))).build().into());
    match agent.get(url).call() {
        Ok(mut response) => match response.body_mut().with_config().limit(MAX_BYTES).read_to_vec() {
            Ok(bytes) => Fetched::Image(bytes),
            Err(_) => Fetched::Failed,
        },
        Err(ureq::Error::StatusCode(404 | 410)) => Fetched::Missing,
        Err(_) => Fetched::Failed,
    }
}

/// Asks GitHub, through the `gh` command (so with your own login, which private repositories need),
/// for the picture of whoever made the commit. `Ok(None)` when the commit's email is linked to no account.
pub fn gh_lookup(hint: &Hint) -> Result<Option<String>, ()> {
    use std::process::{Command, Stdio};
    let name_ok = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    let valid = hint.repo.split_once('/').is_some_and(|(owner, name)| name_ok(owner) && name_ok(name))
        && (7..=64).contains(&hint.sha.len())
        && hint.sha.chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        return Err(());
    }
    // A program started from the Dock does not get the shell's PATH, so look where Homebrew puts it too.
    let mut child = ["gh", "/opt/homebrew/bin/gh", "/usr/local/bin/gh"]
        .iter()
        .find_map(|gh| {
            Command::new(gh)
                .args(["api", &format!("repos/{}/commits/{}", hint.repo, hint.sha), "--jq", ".author.avatar_url // empty"])
                .env("GH_PROMPT_DISABLED", "1")
                .env("NO_COLOR", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        })
        .ok_or(())?;
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(10) => std::thread::sleep(Duration::from_millis(40)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
        }
    };
    if !status.success() {
        return Err(());
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().ok_or(())?, &mut text).map_err(|_| ())?;
    let url = text.trim();
    if url.is_empty() {
        return Ok(None);
    }
    // Only GitHub's own picture host; ask for a small one.
    if !url.starts_with("https://avatars.githubusercontent.com/") {
        return Ok(None);
    }
    Ok(Some(if url.contains('?') { format!("{url}&s=64") } else { format!("{url}?s=64") }))
}

/// Every email's picture this run, by lowercased email.
#[derive(Default)]
pub struct Avatars {
    known: HashMap<String, Avatar>,
    /// Where pictures are kept between runs.
    pub dir: Option<PathBuf>,
    /// Pictures being asked for now, and emails waiting their turn.
    fetching: usize,
    waiting: VecDeque<String>,
}

impl Avatars {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir, ..Self::default() }
    }

    /// Whether `email`, just marked as asked for, may be asked now; if not it waits its turn.
    pub fn start(&mut self, email: &str) -> bool {
        if self.fetching < MOST_AT_ONCE {
            self.fetching += 1;
            true
        } else {
            self.waiting.push_back(email.to_owned());
            false
        }
    }

    /// One ask is done: the next email to ask for, if one is waiting (it takes the free turn).
    pub fn finish(&mut self) -> Option<String> {
        self.fetching = self.fetching.saturating_sub(1);
        let next = self.waiting.pop_front()?;
        self.fetching += 1;
        Some(next)
    }

    /// What is known for `email`, and whether it still has to be asked for (it is then marked as
    /// being asked for, so it is asked once).
    pub fn get(&mut self, email: &str) -> (Avatar, bool) {
        let email = email.trim().to_ascii_lowercase();
        match self.known.get(&email) {
            Some(avatar) => (avatar.clone(), false),
            None => {
                self.known.insert(email, Avatar::Pending);
                (Avatar::Pending, true)
            }
        }
    }

    pub fn set(&mut self, email: &str, avatar: Avatar) {
        self.known.insert(email.trim().to_ascii_lowercase(), avatar);
    }

    /// Forgets everything asked this run, so turning pictures back on asks again.
    pub fn clear(&mut self) {
        self.known.clear();
        self.waiting.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    #[test]
    fn initials_take_the_first_and_last_word() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("Ada King Lovelace"), "AL");
        assert_eq!(initials("octocat"), "O");
        assert_eq!(initials("jean-luc picard"), "JP");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("rom"), "R");
        assert_eq!(initials("Rom"), "R");
        assert_eq!(initials("Kim heang"), "KH");
    }

    #[test]
    fn the_same_person_gets_the_same_color_whatever_the_case() {
        assert_eq!(hue("Ada@Example.com"), hue(" ada@example.com"));
        assert_ne!(hue("ada@example.com"), hue("bob@example.com"));
    }

    #[test]
    fn github_no_reply_emails_go_to_github_and_others_to_gravatar() {
        assert_eq!(url_for("12345+octocat@users.noreply.github.com"), "https://avatars.githubusercontent.com/u/12345?s=64");
        assert_eq!(url_for("octocat@users.noreply.github.com"), "https://github.com/octocat.png?size=64");
        assert_eq!(
            url_for(" MyEmailAddress@example.com "),
            // Gravatar's own documented example address.
            "https://gravatar.com/avatar/84059b07d4be67b806386c0aad8070a23f18836bbaae342275dc0a83414c32ee?s=64&d=404"
        );
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gitgui-avatars-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const NO_LOOKUP: fn(&Hint) -> Result<Option<String>, ()> = |_| Err(());

    #[test]
    fn a_picture_is_fetched_once_then_read_from_the_folder_and_none_is_remembered() {
        let dir = temp("once");
        let asked = std::cell::Cell::new(0);
        let fetch = |_: &str| {
            asked.set(asked.get() + 1);
            Fetched::Image(PNG.to_vec())
        };
        assert!(matches!(load("a@b.c", Some(&dir), None, fetch, NO_LOOKUP), Avatar::Picture(_)));
        assert!(matches!(load("A@B.C", Some(&dir), None, fetch, NO_LOOKUP), Avatar::Picture(_)));
        assert_eq!(asked.get(), 1, "the second time comes from the folder");

        let nothing = |_: &str| {
            asked.set(asked.get() + 1);
            Fetched::Missing
        };
        assert!(matches!(load("x@y.z", Some(&dir), None, nothing, NO_LOOKUP), Avatar::None));
        assert!(matches!(load("x@y.z", Some(&dir), None, nothing, NO_LOOKUP), Avatar::None));
        assert_eq!(asked.get(), 2, "having no picture is remembered too");
        assert!(
            matches!(load("html@y.z", Some(&dir), None, |_: &str| Fetched::Image(b"<html>".to_vec()), NO_LOOKUP), Avatar::None),
            "not an image"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_ask_is_not_remembered_as_having_no_picture() {
        let dir = temp("failed");
        let asked = std::cell::Cell::new(0);
        let offline = |_: &str| {
            asked.set(asked.get() + 1);
            Fetched::Failed
        };
        assert!(matches!(load("a@b.c", Some(&dir), None, offline, NO_LOOKUP), Avatar::None));
        assert!(matches!(load("a@b.c", Some(&dir), None, offline, NO_LOOKUP), Avatar::None));
        assert_eq!(asked.get(), 2, "asked again, because the first answer was not an answer");
        let back = |_: &str| Fetched::Image(PNG.to_vec());
        assert!(matches!(load("a@b.c", Some(&dir), None, back, NO_LOOKUP), Avatar::Picture(_)), "found once the network is back");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_author_gravatar_does_not_know_is_found_through_github() {
        let dir = temp("github");
        let hint = Hint { repo: "acme/app".into(), sha: "abc1234".into() };
        let gravatar_has_none_github_has_one = |url: &str| {
            if url.starts_with("https://gravatar.com/") { Fetched::Missing } else { Fetched::Image(PNG.to_vec()) }
        };
        let found = |_: &Hint| Ok(Some("https://avatars.githubusercontent.com/u/9?v=4&s=64".to_owned()));
        assert!(matches!(load("kim@gmail.com", Some(&dir), Some(&hint), gravatar_has_none_github_has_one, found), Avatar::Picture(_)));
        // Kept: the next time nothing is asked at all.
        let never = |_: &str| -> Fetched { panic!("asked the network for a kept picture") };
        assert!(matches!(load("kim@gmail.com", Some(&dir), Some(&hint), never, NO_LOOKUP), Avatar::Picture(_)));

        // `gh` could not be asked (not installed, not signed in): nothing is remembered, so it is tried again.
        let missing = |_: &str| Fetched::Missing;
        assert!(matches!(load("lee@gmail.com", Some(&dir), Some(&hint), missing, NO_LOOKUP), Avatar::None));
        let linked_now = |_: &Hint| Ok(Some("https://avatars.githubusercontent.com/u/7".to_owned()));
        let github_only = |url: &str| if url.contains("avatars.githubusercontent.com") { Fetched::Image(PNG.to_vec()) } else { Fetched::Missing };
        assert!(matches!(load("lee@gmail.com", Some(&dir), Some(&hint), github_only, linked_now), Avatar::Picture(_)));

        // GitHub says the email is linked to no account: remembered, not asked again.
        let unlinked = std::cell::Cell::new(0);
        let none = |_: &Hint| {
            unlinked.set(unlinked.get() + 1);
            Ok(None)
        };
        assert!(matches!(load("ghost@gmail.com", Some(&dir), Some(&hint), missing, none), Avatar::None));
        assert!(matches!(load("ghost@gmail.com", Some(&dir), Some(&hint), missing, none), Avatar::None));
        assert_eq!(unlinked.get(), 1);

        // Nothing found while there was no commit to ask about: asked again once there is one.
        assert!(matches!(load("new@gmail.com", Some(&dir), None, missing, NO_LOOKUP), Avatar::None));
        assert!(matches!(load("new@gmail.com", Some(&dir), Some(&hint), github_only, linked_now), Avatar::Picture(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_github_lookup_refuses_odd_repo_names_and_ids_before_running_anything() {
        for (repo, sha) in [("a/b c", "abc1234"), ("../x", "abc1234"), ("a/b", "--help"), ("a/b", "xyz"), ("noslash", "abc1234")] {
            assert!(gh_lookup(&Hint { repo: repo.into(), sha: sha.into() }).is_err(), "{repo} {sha}");
        }
    }

    #[test]
    fn only_a_few_pictures_are_asked_for_at_once_and_the_rest_wait_their_turn() {
        let mut avatars = Avatars::default();
        let started: Vec<bool> = (0..MOST_AT_ONCE + 2).map(|n| avatars.start(&format!("{n}@x"))).collect();
        assert_eq!(started.iter().filter(|s| **s).count(), MOST_AT_ONCE);
        assert_eq!(avatars.finish().as_deref(), Some(format!("{MOST_AT_ONCE}@x").as_str()), "the first waiting goes next");
        assert_eq!(avatars.finish().as_deref(), Some(format!("{}@x", MOST_AT_ONCE + 1).as_str()));
        for _ in 0..MOST_AT_ONCE {
            assert_eq!(avatars.finish(), None);
        }
        assert!(avatars.start("again@x"), "a free turn is taken at once");
    }

    #[test]
    fn asking_marks_an_email_so_it_is_asked_once() {
        let mut avatars = Avatars::default();
        assert!(avatars.get("A@b.c").1);
        assert!(!avatars.get("a@b.c").1);
        avatars.set("a@b.c", Avatar::None);
        assert!(matches!(avatars.get("a@b.c").0, Avatar::None));
    }
}
