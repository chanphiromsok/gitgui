//! The working tree: what has changed since the last commit, staged or not; staging and unstaging
//! files; diffs of uncommitted changes; and making the commit.

use std::collections::HashMap;

use crate::backend::{Error, GitCli};
use crate::diff::{self, FileDiff};
use crate::model::{FileChange, FileStatus};

/// One changed file, on one side of the index. A file staged and then changed again shows twice:
/// once staged, once not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkFile {
    pub change: FileChange,
    /// In the index, ready to commit.
    pub staged: bool,
    /// Not tracked by git yet.
    pub untracked: bool,
    /// Left with conflicts by a merge, rebase or cherry-pick.
    pub conflicted: bool,
}

fn status_of(code: u8) -> FileStatus {
    match code {
        b'A' | b'?' => FileStatus::Added,
        b'D' => FileStatus::Deleted,
        b'R' => FileStatus::Renamed,
        b'C' => FileStatus::Copied,
        b'T' => FileStatus::TypeChanged,
        _ => FileStatus::Modified,
    }
}

/// Reads `git status --porcelain=v1 -z`: `XY path\0`, with `\0old\0` after a rename or copy. `X` is
/// the index's side, `Y` the working tree's.
fn parse_status(out: &[u8]) -> Vec<WorkFile> {
    let mut files = Vec::new();
    let mut entries = out.split(|b| *b == 0).filter(|e| e.len() > 3);
    while let Some(entry) = entries.next() {
        let (x, y) = (entry[0], entry[1]);
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        let old_path = matches!(x, b'R' | b'C').then(|| entries.next().map(|o| String::from_utf8_lossy(o).into_owned())).flatten();
        let change = |status: u8, old_path: Option<String>| FileChange {
            path: path.clone(),
            old_path,
            status: status_of(status),
            additions: None,
            deletions: None,
        };
        let conflicted = matches!((x, y), (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D'));
        if conflicted {
            files.push(WorkFile { change: change(b'M', None), staged: false, untracked: false, conflicted: true });
            continue;
        }
        if !matches!(x, b' ' | b'?' | b'!') {
            files.push(WorkFile { change: change(x, old_path), staged: true, untracked: false, conflicted: false });
        }
        if !matches!(y, b' ' | b'!') {
            files.push(WorkFile { change: change(y, None), staged: false, untracked: y == b'?', conflicted: false });
        }
    }
    files
}

/// Line counts from `git diff --numstat -z`, by path; `None` for a binary file.
fn parse_counts(out: &[u8]) -> HashMap<String, (Option<u32>, Option<u32>)> {
    let mut counts = HashMap::new();
    let mut tokens = out.split(|b| *b == 0).filter(|t| !t.is_empty());
    while let Some(token) = tokens.next() {
        let text = String::from_utf8_lossy(token);
        let mut parts = text.splitn(3, '\t');
        let (adds, dels, path) = (parts.next(), parts.next(), parts.next().unwrap_or_default());
        // A rename gives an empty path here, then the old and new paths as tokens of their own.
        let path = if path.is_empty() {
            tokens.next();
            tokens.next().map(|t| String::from_utf8_lossy(t).into_owned()).unwrap_or_default()
        } else {
            path.to_owned()
        };
        let number = |n: Option<&str>| n.and_then(|n| n.parse().ok());
        counts.insert(path, (number(adds), number(dels)));
    }
    counts
}

impl GitCli {
    /// Every uncommitted change: staged files first, then the rest (untracked ones included), each
    /// group by path, each file with its line counts.
    pub fn work_status(&self) -> Result<Vec<WorkFile>, Error> {
        let out = self.run(&["status", "--porcelain=v1", "--untracked-files=all", "-z"])?;
        let mut files = parse_status(&out);
        let staged = parse_counts(&self.run(&["diff", "--cached", "--numstat", "-z", "-M", "--no-ext-diff"])?);
        let unstaged = parse_counts(&self.run(&["diff", "--numstat", "-z", "--no-ext-diff"])?);
        for file in &mut files {
            let counts = if file.untracked {
                // A new file adds every line it has; one that is not text has none to count.
                std::fs::read(self.root().join(&file.change.path))
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .map(|text| (Some(text.lines().count() as u32), Some(0)))
            } else if file.staged {
                staged.get(&file.change.path).copied()
            } else {
                unstaged.get(&file.change.path).copied()
            };
            (file.change.additions, file.change.deletions) = counts.unwrap_or((None, None));
        }
        // Staged first, then the rest; by path within each, as a file tree would list them.
        files.sort_by_cached_key(|file| (!file.staged, file.change.path.to_lowercase()));
        Ok(files)
    }

    /// Stages `paths` as they are now: changes, new files and deletions alike.
    pub fn stage(&self, paths: &[String]) -> Result<String, Error> {
        let mut args = vec!["add", "-A", "--"];
        args.extend(paths.iter().map(String::as_str));
        self.write(&args)
    }

    /// Takes `paths` out of the index again; their changes stay in the working tree.
    pub fn unstage(&self, paths: &[String]) -> Result<String, Error> {
        let mut args: Vec<&str> =
            if self.has_head() { vec!["restore", "--staged", "--"] } else { vec!["rm", "--cached", "-r", "-q", "--"] };
        args.extend(paths.iter().map(String::as_str));
        self.write(&args)
    }

    /// Commits what is staged with `message`. Hooks run as usual; when one refuses, its words come back.
    pub fn commit_staged(&self, message: &str) -> Result<String, Error> {
        if message.trim().is_empty() {
            return Err(Error::Parse("a commit needs a message".into()));
        }
        self.write(&["commit", "--cleanup=strip", "-m", message])
    }

    /// What one uncommitted change does: a staged file against HEAD, an unstaged one against the
    /// index, a new file against nothing.
    pub fn work_diff(&self, file: &WorkFile, context: u32) -> Result<FileDiff, Error> {
        let context = format!("-U{context}");
        let path = file.change.path.as_str();
        if file.untracked {
            // `--no-index` says 1 when the files differ, which they always do here.
            let output = self
                .command(&["diff", "--no-index", "--no-color", "--no-ext-diff", &context, "--", "/dev/null", path])
                .output()
                .map_err(Error::Spawn)?;
            if !matches!(output.status.code(), Some(0 | 1)) {
                return Err(Error::Git { status: output.status.code(), stderr: String::from_utf8_lossy(&output.stderr).into_owned() });
            }
            return Ok(diff::parse(&String::from_utf8_lossy(&output.stdout)));
        }
        let mut args = vec!["diff", "--no-color", "--no-ext-diff", &context];
        if file.staged {
            args.extend(["--cached", "-M"]);
        }
        args.push("--");
        if file.staged {
            args.extend(file.change.old_path.as_deref());
        }
        args.push(path);
        Ok(diff::parse(&String::from_utf8_lossy(&self.run(&args)?)))
    }

    /// The file before and after an uncommitted change, for coloring and pictures: HEAD and the
    /// index for a staged file, the index and the disk for the rest.
    pub fn work_sides(&self, file: &WorkFile) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        let path = &file.change.path;
        let old_path = file.change.old_path.as_deref().unwrap_or(path);
        let index = |p: &str| self.file_bytes_at("", p).ok().flatten();
        let (old, new) = if file.staged {
            (self.file_bytes_at("HEAD", old_path).ok().flatten(), index(path))
        } else {
            (if file.untracked { None } else { index(path) }, std::fs::read(self.root().join(path)).ok())
        };
        let gone = file.change.status == FileStatus::Deleted;
        (if file.change.status == FileStatus::Added { None } else { old }, if gone { None } else { new })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_splits_staged_and_unstaged_and_reads_renames() {
        let out = b"M  staged.rs\0MM both.rs\0 M edited.rs\0?? new.txt\0R  new_name.rs\0old_name.rs\0 D gone.rs\0UU clash.rs\0";
        let files = parse_status(out);
        let show: Vec<(String, FileStatus, bool, bool, bool)> = files
            .iter()
            .map(|f| (f.change.path.clone(), f.change.status, f.staged, f.untracked, f.conflicted))
            .collect();
        use FileStatus::*;
        assert_eq!(
            show,
            [
                ("staged.rs".into(), Modified, true, false, false),
                ("both.rs".into(), Modified, true, false, false),
                ("both.rs".into(), Modified, false, false, false),
                ("edited.rs".into(), Modified, false, false, false),
                ("new.txt".into(), Added, false, true, false),
                ("new_name.rs".into(), Renamed, true, false, false),
                ("gone.rs".into(), Deleted, false, false, false),
                ("clash.rs".into(), Modified, false, false, true),
            ]
        );
        assert_eq!(files[5].change.old_path.as_deref(), Some("old_name.rs"));
    }

    #[test]
    fn counts_read_plain_binary_and_renamed_files() {
        let counts = parse_counts(b"3\t1\ta.rs\0-\t-\timg.png\0" as &[u8]);
        assert_eq!(counts["a.rs"], (Some(3), Some(1)));
        assert_eq!(counts["img.png"], (None, None));
        let renamed = parse_counts(b"0\t0\t\0old.rs\0new.rs\0");
        assert_eq!(renamed["new.rs"], (Some(0), Some(0)));
    }
}
