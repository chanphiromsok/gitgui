//! Author pictures: a profile picture when there is one, else the author's initials on a color
//! that is always the same for the same person.
//!
//! A GitHub no-reply email (`123+user@users.noreply.github.com`) gives the GitHub avatar; any other
//! email asks Gravatar, by a SHA-256 of the address. Pictures are fetched in the background and kept
//! in the data folder, so each is asked for once a week at most. Fetching can be turned off in
//! Settings; the initials need nothing from the network.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::{Image, ImageFormat};
use sha2::{Digest, Sha256};

/// A fetched picture, or the knowledge that there is none, is trusted this long.
const FRESH_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Pictures bigger than this are not kept.
const MAX_BYTES: u64 = 1 << 20;

/// What is known about one email's picture.
#[derive(Clone)]
pub enum Avatar {
    /// Being fetched, or not asked for yet: draw the initials meanwhile.
    Pending,
    /// No picture: draw the initials.
    None,
    Picture(Arc<Image>),
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
/// then kept). Runs on a background thread. `fetch` is what asks the network, so tests can stand in.
pub fn load(email: &str, dir: Option<&Path>, fetch: impl Fn(&str) -> Option<Vec<u8>>) -> Avatar {
    let key = key(email);
    let picture = dir.map(|d| d.join(format!("{key}.img")));
    let none = dir.map(|d| d.join(format!("{key}.none")));
    let from = |bytes: Vec<u8>| match format_of(&bytes) {
        Some(format) => Avatar::Picture(Arc::new(Image::from_bytes(format, bytes))),
        None => Avatar::None,
    };
    if let Some(path) = picture.as_ref().filter(|p| fresh(p))
        && let Ok(bytes) = std::fs::read(path)
    {
        return from(bytes);
    }
    if none.as_ref().is_some_and(|p| fresh(p)) {
        return Avatar::None;
    }
    let fetched = fetch(&url_for(email)).filter(|bytes| format_of(bytes).is_some());
    if let Some(dir) = dir {
        let _ = std::fs::create_dir_all(dir);
        let _ = match &fetched {
            Some(bytes) => std::fs::write(dir.join(format!("{key}.img")), bytes),
            None => std::fs::write(dir.join(format!("{key}.none")), b""),
        };
    }
    fetched.map_or(Avatar::None, from)
}

/// Asks the network for a picture; `None` for anything but a small image.
pub fn fetch(url: &str) -> Option<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(8))).build().into();
    let mut response = agent.get(url).call().ok()?;
    response.body_mut().with_config().limit(MAX_BYTES).read_to_vec().ok()
}

/// Every email's picture this run, by lowercased email.
#[derive(Default)]
pub struct Avatars {
    known: HashMap<String, Avatar>,
    /// Where pictures are kept between runs.
    pub dir: Option<PathBuf>,
}

impl Avatars {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { known: HashMap::new(), dir }
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

    #[test]
    fn a_picture_is_fetched_once_then_read_from_the_folder_and_none_is_remembered() {
        let dir = std::env::temp_dir().join(format!("gitgui-avatars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let asked = std::cell::Cell::new(0);
        let fetch = |_: &str| {
            asked.set(asked.get() + 1);
            Some(PNG.to_vec())
        };
        assert!(matches!(load("a@b.c", Some(&dir), fetch), Avatar::Picture(_)));
        assert!(matches!(load("A@B.C", Some(&dir), fetch), Avatar::Picture(_)));
        assert_eq!(asked.get(), 1, "the second time comes from the folder");

        let nothing = |_: &str| {
            asked.set(asked.get() + 1);
            None
        };
        assert!(matches!(load("x@y.z", Some(&dir), nothing), Avatar::None));
        assert!(matches!(load("x@y.z", Some(&dir), nothing), Avatar::None));
        assert_eq!(asked.get(), 2, "having no picture is remembered too");
        assert!(matches!(load("html@y.z", Some(&dir), |_: &str| Some(b"<html>".to_vec())), Avatar::None), "not an image");
        let _ = std::fs::remove_dir_all(&dir);
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
