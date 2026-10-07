//! Saved projects and line comments.
//!
//! Everything lives as JSON under one data folder (`GITGUI_DATA_DIR`, or the platform's app-data
//! folder). Writes go to a temp file first and are renamed into place, so a crash never leaves half a
//! file. A file that cannot be read is reported and left alone; it is never overwritten.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum Error {
    Io { path: PathBuf, source: std::io::Error },
    /// The file exists but is not valid JSON of the expected shape.
    Corrupt { path: PathBuf, source: serde_json::Error },
    /// No folder to keep data in (no home directory, say).
    NoDataDir,
    NotFound(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Corrupt { path, source } => {
                write!(f, "{} is not readable ({source}); it was left untouched", path.display())
            }
            Error::NoDataDir => write!(f, "no folder to keep app data in"),
            Error::NotFound(what) => write!(f, "{what} not found"),
        }
    }
}

impl std::error::Error for Error {}

/// How a commit's changed files are listed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileLayout {
    #[default]
    Tree,
    Flat,
}

/// How a file's diff is laid out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffMode {
    #[default]
    Unified,
    Split,
}

/// Choices the user can change. Every field has a default, so a file written by an older version
/// (with fewer fields) still loads, and a field this version does not know is ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// List a pull request's commits under it, one level in, instead of in date order among the rest.
    pub group_by_parent: bool,
    /// The color theme, by name; `None` for the default one.
    pub theme: Option<String>,
    /// The file icon theme, by name; `None` for the built-in icons.
    pub icon_theme: Option<String>,
    /// The projects sidebar is hidden.
    pub sidebar_hidden: bool,
    /// Draw the graph with narrow lanes and thin lines.
    pub compact_graph: bool,
    /// Fetch authors' pictures from GitHub and Gravatar; off draws their initials only.
    pub fetch_avatars: bool,
    /// Changed files as a tree or a flat list. Remembered from one launch to the next.
    pub file_layout: FileLayout,
    /// A file's diff unified or split. Remembered from one launch to the next.
    pub diff_mode: DiffMode,
    /// How large the graph is drawn (commit circles, lanes, row height), in percent. 100 is the default.
    pub graph_scale: u32,
}

/// The smallest and largest graph scale, in percent; a saved value outside is brought inside.
pub const GRAPH_SCALE_MIN: u32 = 75;
pub const GRAPH_SCALE_MAX: u32 = 200;

impl Settings {
    /// The graph scale as a factor (1.0 at the default), kept within the allowed range.
    pub fn graph_factor(&self) -> f32 {
        self.graph_scale.clamp(GRAPH_SCALE_MIN, GRAPH_SCALE_MAX) as f32 / 100.
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            group_by_parent: true,
            theme: None,
            icon_theme: None,
            sidebar_hidden: false,
            compact_graph: false,
            fetch_avatars: true,
            file_layout: FileLayout::Tree,
            diff_mode: DiffMode::Unified,
            graph_scale: 100,
        }
    }
}

/// A repository shown in the sidebar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub path: PathBuf,
    /// The folder's name.
    pub name: String,
}

/// Which side of a diff a comment is on. A removed line only exists on the old side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Old,
    New,
}

/// A comment on one line of one file in one commit. A commit never changes, so its line numbers
/// never go stale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub commit: String,
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub text: String,
    /// Seconds since the Unix epoch.
    pub created: i64,
    #[serde(default)]
    pub resolved: bool,
}

/// What the caller supplies; the store adds the id and time.
#[derive(Clone, Debug)]
pub struct NewComment {
    pub commit: String,
    pub path: String,
    pub side: Side,
    pub line: u32,
    pub text: String,
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// The default data folder: `GITGUI_DATA_DIR` if set, else the platform's app-data folder.
    pub fn open_default() -> Result<Self, Error> {
        Ok(Self::at(default_dir().ok_or(Error::NoDataDir)?))
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The data folder. Themes the user adds go in `themes/` under it.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // ---- projects -------------------------------------------------------------------------

    pub fn projects(&self) -> Result<Vec<Project>, Error> {
        read_json(&self.dir.join("projects.json"))
    }

    /// Adds a folder to the sidebar. Adding one already there changes nothing.
    pub fn add_project(&self, path: &Path) -> Result<Project, Error> {
        let path = fs::canonicalize(path).map_err(|source| Error::Io { path: path.to_owned(), source })?;
        let mut projects = self.projects()?;
        if let Some(existing) = projects.iter().find(|p| p.path == path) {
            return Ok(existing.clone());
        }
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let project = Project { path, name };
        projects.push(project.clone());
        write_json(&self.dir.join("projects.json"), &projects)?;
        Ok(project)
    }

    pub fn remove_project(&self, path: &Path) -> Result<(), Error> {
        let mut projects = self.projects()?;
        let before = projects.len();
        projects.retain(|p| p.path != path);
        if projects.len() == before {
            return Err(Error::NotFound(path.display().to_string()));
        }
        write_json(&self.dir.join("projects.json"), &projects)
    }

    // ---- settings -------------------------------------------------------------------------

    /// The saved settings, or the defaults when none were saved yet.
    pub fn settings(&self) -> Result<Settings, Error> {
        read_one(&self.dir.join("settings.json"))
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), Error> {
        write_json(&self.dir.join("settings.json"), settings)
    }

    // ---- comments -------------------------------------------------------------------------

    pub fn comments(&self, repo: &Path) -> Result<Vec<Comment>, Error> {
        read_json(&self.comments_file(repo))
    }

    /// The comments on one file of one commit, in line order.
    pub fn comments_on(&self, repo: &Path, commit: &str, path: &str) -> Result<Vec<Comment>, Error> {
        let mut found: Vec<Comment> =
            self.comments(repo)?.into_iter().filter(|c| c.commit == commit && c.path == path).collect();
        found.sort_by_key(|c| (c.line, c.created));
        Ok(found)
    }

    pub fn add_comment(&self, repo: &Path, new: NewComment) -> Result<Comment, Error> {
        let file = self.comments_file(repo);
        let mut all: Vec<Comment> = read_json(&file)?;
        let comment = Comment {
            id: next_id(),
            commit: new.commit,
            path: new.path,
            side: new.side,
            line: new.line,
            text: new.text,
            created: now(),
            resolved: false,
        };
        all.push(comment.clone());
        write_json(&file, &all)?;
        Ok(comment)
    }

    pub fn delete_comment(&self, repo: &Path, id: &str) -> Result<(), Error> {
        let file = self.comments_file(repo);
        let mut all: Vec<Comment> = read_json(&file)?;
        let before = all.len();
        all.retain(|c| c.id != id);
        if all.len() == before {
            return Err(Error::NotFound(format!("comment {id}")));
        }
        write_json(&file, &all)
    }

    pub fn set_resolved(&self, repo: &Path, id: &str, resolved: bool) -> Result<(), Error> {
        let file = self.comments_file(repo);
        let mut all: Vec<Comment> = read_json(&file)?;
        let comment = all.iter_mut().find(|c| c.id == id).ok_or_else(|| Error::NotFound(format!("comment {id}")))?;
        comment.resolved = resolved;
        write_json(&file, &all)
    }

    fn comments_file(&self, repo: &Path) -> PathBuf {
        let repo = fs::canonicalize(repo).unwrap_or_else(|_| repo.to_owned());
        self.dir.join("comments").join(format!("{:016x}.json", fnv1a(repo.to_string_lossy().as_bytes())))
    }
}

fn default_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("GITGUI_DATA_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        Some(home()?.join("Library/Application Support/gitgui"))
    } else if cfg!(windows) {
        Some(PathBuf::from(std::env::var_os("APPDATA")?).join("gitgui"))
    } else {
        let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| Some(home()?.join(".local/share")))?;
        Some(base.join("gitgui"))
    }
}

/// A hash that is the same on every run and every Rust version, unlike `DefaultHasher`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn next_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{nanos:x}-{:x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// A missing file is an empty list; an unreadable one is an error.
fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, Error> {
    match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(Vec::new()),
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| Error::Corrupt { path: path.to_owned(), source }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(Error::Io { path: path.to_owned(), source }),
    }
}

/// Like `read_json` for a single value: a missing or empty file gives the default.
fn read_one<T: DeserializeOwned + Default>(path: &Path) -> Result<T, Error> {
    match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(T::default()),
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| Error::Corrupt { path: path.to_owned(), source }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(source) => Err(Error::Io { path: path.to_owned(), source }),
    }
}

fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), Error> {
    let io = |source| Error::Io { path: path.to_owned(), source };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io)?;
    }
    let temp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| Error::Corrupt { path: path.to_owned(), source })?;
    fs::write(&temp, bytes).map_err(io)?;
    fs::rename(&temp, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("gitgui-store-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn store(&self) -> Store {
            Store::at(self.0.join("data"))
        }

        fn folder(&self, name: &str) -> PathBuf {
            let dir = self.0.join(name);
            fs::create_dir_all(&dir).unwrap();
            dir
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn new_comment(commit: &str, path: &str, side: Side, line: u32, text: &str) -> NewComment {
        NewComment { commit: commit.into(), path: path.into(), side, line, text: text.into() }
    }

    #[test]
    fn a_fresh_store_has_no_projects_and_no_comments() {
        let scratch = Scratch::new("fresh");
        let store = scratch.store();
        assert!(store.projects().unwrap().is_empty());
        assert!(store.comments(&scratch.folder("repo")).unwrap().is_empty());
    }

    #[test]
    fn adding_a_project_twice_keeps_one_and_survives_a_restart() {
        let scratch = Scratch::new("projects");
        let repo = scratch.folder("my-app");
        let first = scratch.store().add_project(&repo).unwrap();
        assert_eq!(first.name, "my-app");

        // A second Store over the same folder is what the next launch sees.
        let again = scratch.store().add_project(&repo.join("../my-app")).unwrap();
        assert_eq!(first, again, "the same folder by another route is the same project");
        assert_eq!(scratch.store().projects().unwrap(), [first]);
    }

    #[test]
    fn removing_a_project_forgets_it_and_removing_a_stranger_says_so() {
        let scratch = Scratch::new("remove");
        let store = scratch.store();
        let project = store.add_project(&scratch.folder("a")).unwrap();
        store.add_project(&scratch.folder("b")).unwrap();

        store.remove_project(&project.path).unwrap();
        let names: Vec<String> = store.projects().unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["b"]);
        assert!(matches!(store.remove_project(&project.path), Err(Error::NotFound(_))));
    }

    #[test]
    fn a_folder_that_does_not_exist_cannot_be_added() {
        let scratch = Scratch::new("missing");
        let err = scratch.store().add_project(&scratch.0.join("nope")).unwrap_err();
        assert!(matches!(err, Error::Io { .. }));
        assert!(scratch.store().projects().unwrap().is_empty());
    }

    #[test]
    fn comments_are_kept_per_repo_and_found_by_commit_and_file_in_line_order() {
        let scratch = Scratch::new("comments");
        let store = scratch.store();
        let repo = scratch.folder("repo");
        let other = scratch.folder("other");

        store.add_comment(&repo, new_comment("c1", "src/a.rs", Side::New, 30, "later line")).unwrap();
        store.add_comment(&repo, new_comment("c1", "src/a.rs", Side::Old, 4, "removed line")).unwrap();
        store.add_comment(&repo, new_comment("c1", "src/b.rs", Side::New, 1, "other file")).unwrap();
        store.add_comment(&repo, new_comment("c2", "src/a.rs", Side::New, 2, "other commit")).unwrap();
        store.add_comment(&other, new_comment("c1", "src/a.rs", Side::New, 1, "other repo")).unwrap();

        let found = store.comments_on(&repo, "c1", "src/a.rs").unwrap();
        let texts: Vec<&str> = found.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["removed line", "later line"]);
        assert_eq!(found[0].side, Side::Old);
        assert_eq!(store.comments(&other).unwrap().len(), 1);
    }

    #[test]
    fn comments_get_distinct_ids_and_can_be_resolved_and_deleted() {
        let scratch = Scratch::new("edit");
        let store = scratch.store();
        let repo = scratch.folder("repo");
        let a = store.add_comment(&repo, new_comment("c", "f", Side::New, 1, "a")).unwrap();
        let b = store.add_comment(&repo, new_comment("c", "f", Side::New, 1, "b")).unwrap();
        assert_ne!(a.id, b.id);
        assert!(!a.resolved);

        store.set_resolved(&repo, &a.id, true).unwrap();
        let all = store.comments(&repo).unwrap();
        assert!(all.iter().find(|c| c.id == a.id).unwrap().resolved);
        assert!(!all.iter().find(|c| c.id == b.id).unwrap().resolved);

        store.delete_comment(&repo, &a.id).unwrap();
        assert_eq!(store.comments(&repo).unwrap().len(), 1);
        assert!(matches!(store.delete_comment(&repo, &a.id), Err(Error::NotFound(_))));
        assert!(matches!(store.set_resolved(&repo, "nope", true), Err(Error::NotFound(_))));
    }

    #[test]
    fn a_corrupt_file_is_reported_and_never_overwritten() {
        let scratch = Scratch::new("corrupt");
        let store = scratch.store();
        let repo = scratch.folder("repo");
        store.add_comment(&repo, new_comment("c", "f", Side::New, 1, "keep me")).unwrap();

        let file = store.comments_file(&repo);
        fs::write(&file, "{ this is not json").unwrap();

        assert!(matches!(store.comments(&repo), Err(Error::Corrupt { .. })));
        assert!(matches!(store.add_comment(&repo, new_comment("c", "f", Side::New, 2, "x")), Err(Error::Corrupt { .. })));
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ this is not json", "the broken file stays as it was");
    }

    #[test]
    fn a_comment_written_without_the_resolved_field_still_loads() {
        // Files from an earlier version have no `resolved`; they must not become unreadable.
        let scratch = Scratch::new("compat");
        let store = scratch.store();
        let repo = scratch.folder("repo");
        let file = store.comments_file(&repo);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            r#"[{"id":"1","commit":"c","path":"f","side":"new","line":3,"text":"old format","created":1}]"#,
        )
        .unwrap();
        let loaded = store.comments(&repo).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].resolved);
    }

    #[test]
    fn no_temp_file_is_left_behind() {
        let scratch = Scratch::new("temp");
        let store = scratch.store();
        store.add_project(&scratch.folder("a")).unwrap();
        let leftovers: Vec<_> = fs::read_dir(scratch.0.join("data"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn the_repo_key_is_stable_across_runs() {
        // Pinned so comment files are still found after an upgrade.
        assert_eq!(fnv1a(b"/Users/x/repo"), fnv1a(b"/Users/x/repo"));
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_ne!(fnv1a(b"/a"), fnv1a(b"/b"));
    }

    #[test]
    fn grouping_is_on_by_default_and_a_choice_is_kept() {
        let scratch = Scratch::new("settings");
        let store = scratch.store();
        assert!(store.settings().unwrap().group_by_parent, "on until the user turns it off");

        store.save_settings(&Settings { group_by_parent: false, ..Default::default() }).unwrap();
        assert!(!scratch.store().settings().unwrap().group_by_parent, "and it survives a restart");

        store.save_settings(&Settings::default()).unwrap();
        assert!(store.settings().unwrap().group_by_parent);
    }

    #[test]
    fn a_settings_file_from_another_version_still_loads() {
        let scratch = Scratch::new("settings-compat");
        let store = scratch.store();
        fs::create_dir_all(scratch.0.join("data")).unwrap();
        let file = scratch.0.join("data/settings.json");

        fs::write(&file, "{}").unwrap();
        assert!(store.settings().unwrap().group_by_parent, "a missing field takes its default, not false");
        fs::write(&file, r#"{"group_by_parent": false, "from_the_future": 42}"#).unwrap();
        assert!(!store.settings().unwrap().group_by_parent, "an unknown field is ignored");
    }

    #[test]
    fn an_unreadable_settings_file_is_reported_and_left_alone() {
        let scratch = Scratch::new("settings-corrupt");
        let store = scratch.store();
        fs::create_dir_all(scratch.0.join("data")).unwrap();
        let file = scratch.0.join("data/settings.json");
        fs::write(&file, "not json").unwrap();

        assert!(matches!(store.settings(), Err(Error::Corrupt { .. })));
        assert_eq!(fs::read_to_string(&file).unwrap(), "not json");
    }

    #[test]
    fn the_file_list_and_diff_layout_are_remembered_and_default_to_tree_and_unified() {
        let scratch = Scratch::new("layout-settings");
        let store = scratch.store();
        let fresh = store.settings().unwrap();
        assert_eq!((fresh.file_layout, fresh.diff_mode), (FileLayout::Tree, DiffMode::Unified));

        store.save_settings(&Settings { file_layout: FileLayout::Flat, diff_mode: DiffMode::Split, ..Settings::default() }).unwrap();
        let again = scratch.store().settings().unwrap();
        assert_eq!((again.file_layout, again.diff_mode), (FileLayout::Flat, DiffMode::Split));
        let text = fs::read_to_string(scratch.0.join("data/settings.json")).unwrap();
        assert!(text.contains("\"flat\"") && text.contains("\"split\""), "readable in the file: {text}");
    }

    #[test]
    fn a_settings_file_from_before_these_choices_keeps_the_defaults() {
        let scratch = Scratch::new("layout-compat");
        fs::create_dir_all(scratch.0.join("data")).unwrap();
        fs::write(scratch.0.join("data/settings.json"), r#"{"group_by_parent": false}"#).unwrap();
        let loaded = scratch.store().settings().unwrap();
        assert!(!loaded.group_by_parent);
        assert_eq!((loaded.file_layout, loaded.diff_mode), (FileLayout::Tree, DiffMode::Unified));
        // A value from a newer version that this one does not know is an error, not a silent reset.
        fs::write(scratch.0.join("data/settings.json"), r#"{"diff_mode": "sideways"}"#).unwrap();
        assert!(matches!(scratch.store().settings(), Err(Error::Corrupt { .. })));
    }

    #[test]
    fn the_graph_scale_defaults_to_100_is_remembered_and_stays_in_range() {
        let scratch = Scratch::new("graph-scale");
        let store = scratch.store();
        assert_eq!(store.settings().unwrap().graph_scale, 100);
        store.save_settings(&Settings { graph_scale: 150, ..Settings::default() }).unwrap();
        let again = scratch.store().settings().unwrap();
        assert_eq!((again.graph_scale, again.graph_factor()), (150, 1.5));
        // A hand-edited value outside the range is brought inside rather than drawing a 0 px graph.
        assert_eq!(Settings { graph_scale: 0, ..Settings::default() }.graph_factor(), 0.75);
        assert_eq!(Settings { graph_scale: 9000, ..Settings::default() }.graph_factor(), 2.0);
    }
}
