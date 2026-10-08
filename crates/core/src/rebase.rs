//! What a rebase leaves behind: the branch's tip from before it, which git keeps in the reflog, and the commits
//! that are the same changes under new ids.

use std::collections::HashMap;

use crate::actions::check_name;
use crate::backend::{Backend, Error, GitCli, check_rev};

/// A branch whose last move was a finished rebase (or a `pull --rebase`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rebased {
    pub branch: String,
    /// Where the branch was before the rebase.
    pub old_tip: String,
    /// Where it is now.
    pub new_tip: String,
    /// What it was rebased onto, when the reflog says.
    pub onto: Option<String>,
    /// How many commits sit on `onto` now (the ones the rebase rewrote), when `onto` is known.
    pub commits: usize,
}

/// Does a reflog subject say a rebase finished? `rebase (finish): refs/heads/x onto 1a2b…`, `rebase -i (finish): …`,
/// `pull --rebase (finish): …`.
fn finished_rebase(subject: &str) -> bool {
    subject.contains("(finish)") && (subject.starts_with("rebase") || subject.starts_with("pull --rebase") || subject.starts_with("pull -r"))
}

/// The most commits looked at on each side when finding the same changes under different ids.
const TWIN_COMMITS: usize = 200;

impl GitCli {
    /// The rebase that is the last thing to have happened to `branch`, if one is, and its old tip is still there.
    pub fn last_rebase(&self, branch: &str) -> Option<Rebased> {
        check_name(branch).ok()?;
        let spec = format!("refs/heads/{branch}");
        let out = self.run(&["reflog", "show", "--format=%H%x1f%gs", "-n", "2", &spec]).ok()?;
        let text = String::from_utf8_lossy(&out);
        let mut entries = text.lines().filter_map(|line| line.split_once('\u{1f}'));
        let (new_tip, why) = entries.next()?;
        let (old_tip, _) = entries.next()?;
        if !finished_rebase(why) || old_tip == new_tip {
            return None;
        }
        // Nothing has moved the branch since, and the old tip is still an object git has.
        let now = self.run(&["rev-parse", "--verify", "-q", &spec]).ok()?;
        if String::from_utf8_lossy(&now).trim() != new_tip {
            return None;
        }
        self.run(&["cat-file", "-e", &format!("{old_tip}^{{commit}}")]).ok()?;
        let onto = why
            .rsplit_once(" onto ")
            .map(|(_, id)| id.trim().to_owned())
            .filter(|id| (7..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit()));
        let commits = onto
            .as_ref()
            .and_then(|onto| self.run(&["rev-list", "--count", &format!("{onto}..{new_tip}")]).ok())
            .and_then(|out| String::from_utf8_lossy(&out).trim().parse().ok())
            .unwrap_or(0);
        Some(Rebased { branch: branch.to_owned(), old_tip: old_tip.to_owned(), new_tip: new_tip.to_owned(), onto, commits })
    }

    /// Moves the branch back to where it was before the rebase. Only the branch that is checked out, only while
    /// nothing has moved it, and never over changes that are not committed: the rebased commits stay in the reflog.
    pub fn undo_rebase(&self, rebased: &Rebased) -> Result<String, Error> {
        check_rev(&rebased.old_tip)?;
        let branch = rebased.branch.as_str();
        if Backend::current_branch(self)?.as_deref() != Some(branch) {
            return Err(Error::Parse(format!("Check out {branch} first: it is not the branch that is open.")));
        }
        let head = self.run(&["rev-parse", "HEAD"])?;
        if String::from_utf8_lossy(&head).trim() != rebased.new_tip {
            return Err(Error::Parse(format!("{branch} has moved since the rebase, so it is not moved back.")));
        }
        if self.work_status()?.iter().any(|file| !file.untracked) {
            return Err(Error::Parse("Commit or stash your changes first: moving the branch back would overwrite them.".into()));
        }
        // `--keep` stops by itself if a file would be lost.
        self.write(&["reset", "--keep", &rebased.old_tip])
    }

    /// Pairs of commits that make the same changes under different ids: one only on `a` (not on `b`), one only on
    /// `b` (not on `a`). That is how a rebased or cherry-picked commit shows up beside the one it was copied from.
    pub fn twins(&self, a: &str, b: &str) -> Vec<(String, String)> {
        if check_rev(a).is_err() || check_rev(b).is_err() {
            return Vec::new();
        }
        let patches = |range: String| -> Vec<(String, String)> {
            let limit = format!("--max-count={TWIN_COMMITS}");
            let Ok(out) = self.pipeline(
                &["log", "-p", "--no-merges", "--no-color", "--no-ext-diff", "--no-decorate", &limit, &range],
                &["patch-id", "--stable"],
            ) else {
                return Vec::new();
            };
            String::from_utf8_lossy(&out)
                .lines()
                .filter_map(|line| {
                    let mut parts = line.split_whitespace();
                    Some((parts.next()?.to_owned(), parts.next()?.to_owned()))
                })
                .collect()
        };
        let there: HashMap<String, String> = patches(format!("{a}..{b}")).into_iter().collect();
        patches(format!("{b}..{a}"))
            .into_iter()
            .filter_map(|(patch, here)| there.get(&patch).filter(|other| **other != here).map(|other| (here, other.clone())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_finished_rebase_counts() {
        assert!(finished_rebase("rebase (finish): refs/heads/feat onto 1a2b3c4d"));
        assert!(finished_rebase("rebase -i (finish): refs/heads/feat onto 1a2b3c4d"));
        assert!(finished_rebase("pull --rebase (finish): refs/heads/feat onto 1a2b3c4d"));
        assert!(!finished_rebase("rebase (pick): feat: one"));
        assert!(!finished_rebase("commit: feat: one"));
        assert!(!finished_rebase("merge main: Fast-forward"));
    }
}
