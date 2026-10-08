//! Operations that change the repository: checkout, branch and tag management, merge, rebase,
//! cherry-pick and push.
//!
//! Ground rules, so a click can never do more than it says:
//! - nothing is forced: no `--force`, no `reset --hard`, and deleting an unmerged branch needs `force`
//!   to be asked for by name;
//! - a merge, rebase or cherry-pick that meets conflicts is reported as [`Outcome::Conflicts`] and can
//!   be undone with [`GitCli::abort`];
//! - git never waits on a terminal: credential prompts fail instead of hanging, and no editor opens;
//! - names are checked before they reach git.

use std::path::PathBuf;
use std::process::{Command as Process, Stdio};

use crate::backend::{Backend, Error, GitCli};
use crate::sync::{default_remote, nothing_to_pull};

/// A multi-step operation git can leave half done when it meets conflicts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Merge,
    Rebase,
    CherryPick,
}

impl Operation {
    pub fn name(self) -> &'static str {
        match self {
            Operation::Merge => "merge",
            Operation::Rebase => "rebase",
            Operation::CherryPick => "cherry-pick",
        }
    }
}

/// How a merge, rebase or cherry-pick ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It finished. The text says what happened, from git's own output.
    Done(String),
    /// It stopped on conflicts in this many files and is waiting: resolve them and continue in an
    /// editor or a terminal, or [`GitCli::abort`] to put everything back.
    Conflicts { operation: Operation, files: usize },
}

/// What to check out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckoutTarget {
    /// A local branch, by name.
    Branch(String),
    /// A remote branch like `origin/feat/x`: makes a local branch that tracks it.
    RemoteBranch(String),
    /// A commit or tag, leaving HEAD detached.
    Detached(String),
}

/// A name or id that is safe to pass to git as an argument: not empty, not an option, no spaces or
/// control characters.
fn check_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || name.starts_with('-') || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::Parse(format!("not a usable name: {name:?}")));
    }
    Ok(())
}

impl GitCli {
    /// Runs a command that changes things. Terminal prompts and editors are turned off so it cannot hang.
    pub(crate) fn write(&self, args: &[&str]) -> Result<String, Error> {
        let output = self.write_output(args)?;
        if output.status.success() {
            Ok(text_of(&output))
        } else {
            Err(Error::Git { status: output.status.code(), stderr: text_of(&output) })
        }
    }

    fn write_output(&self, args: &[&str]) -> Result<std::process::Output, Error> {
        Process::new("git")
            .arg("-C")
            .arg(self.root())
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("GCM_INTERACTIVE", "never")
            .stdin(Stdio::null())
            .output()
            .map_err(Error::Spawn)
    }

    /// Checks out a branch, a remote branch (as a new tracking branch) or a commit.
    /// Git refuses, and says so, when local changes would be overwritten.
    pub fn checkout(&self, target: &CheckoutTarget) -> Result<String, Error> {
        match target {
            CheckoutTarget::Branch(name) => {
                check_name(name)?;
                self.write(&["switch", name])
            }
            CheckoutTarget::RemoteBranch(name) => {
                check_name(name)?;
                self.write(&["switch", "--track", name])
            }
            CheckoutTarget::Detached(id) => {
                check_name(id)?;
                self.write(&["switch", "--detach", id])
            }
        }
    }

    /// Makes a branch at `at` (HEAD when `None`), and switches to it when `switch`.
    pub fn create_branch(&self, name: &str, at: Option<&str>, switch: bool) -> Result<String, Error> {
        self.check_new_branch_name(name)?;
        let at = at.unwrap_or("HEAD");
        check_name(at)?;
        // Not tracking the branch it starts from: a new branch pushed to its own name, not to `develop`.
        if switch { self.write(&["switch", "-c", name, "--no-track", at]) } else { self.write(&["branch", "--no-track", name, at]) }
    }

    /// Makes a lightweight tag at `at`.
    pub fn create_tag(&self, name: &str, at: &str) -> Result<String, Error> {
        check_name(name)?;
        check_name(at)?;
        self.write(&["check-ref-format", &format!("refs/tags/{name}")])?;
        self.write(&["tag", name, at])
    }

    pub fn rename_branch(&self, old: &str, new: &str) -> Result<String, Error> {
        check_name(old)?;
        self.check_new_branch_name(new)?;
        self.write(&["branch", "-m", old, new])
    }

    /// Deletes a branch. Without `force`, git refuses one that is not merged into HEAD or its upstream;
    /// a squash-merged branch looks unmerged to git, so deleting it needs `force`.
    pub fn delete_branch(&self, name: &str, force: bool) -> Result<String, Error> {
        check_name(name)?;
        self.write(&["branch", if force { "-D" } else { "-d" }, name])
    }

    fn check_new_branch_name(&self, name: &str) -> Result<(), Error> {
        check_name(name)?;
        self.write(&["check-ref-format", "--branch", name]).map(|_| ())
    }

    /// Merges `branch` into the current branch.
    pub fn merge(&self, branch: &str) -> Result<Outcome, Error> {
        check_name(branch)?;
        self.finish(Operation::Merge, &["merge", "--no-edit", branch])
    }

    /// Rebases the current branch onto `onto`.
    pub fn rebase(&self, onto: &str) -> Result<Outcome, Error> {
        check_name(onto)?;
        self.finish(Operation::Rebase, &["rebase", onto])
    }

    /// `git pull --rebase`: fetches the current branch's upstream and replays the commits that are only
    /// here on top of it, so history stays a straight line instead of gaining a merge commit.
    /// Git refuses, and says so, with uncommitted changes, a detached HEAD or no upstream. On conflicts
    /// it stops like a rebase and [`GitCli::abort`] puts everything back.
    pub fn pull_rebase(&self) -> Result<Outcome, Error> {
        let Some(branch) = self.current_branch()? else {
            return Err(Error::Parse("switch to a branch first: there is no branch checked out to pull into.".into()));
        };
        // A branch made here has no remote branch until it is pushed; git's own words for that are about tracking.
        if let Some(why) = nothing_to_pull(&branch, &self.upstream_of(&branch)?) {
            return Err(Error::Parse(why));
        }
        self.finish(Operation::Rebase, &["pull", "--rebase", "--no-stat"])
    }

    /// The remotes' names: `origin`, and any others.
    pub fn remotes(&self) -> Result<Vec<String>, Error> {
        let out = self.run(&["remote"])?;
        Ok(String::from_utf8_lossy(&out).lines().map(str::trim).filter(|r| !r.is_empty()).map(str::to_owned).collect())
    }

    /// Brings in what the remotes have without touching any local branch; branches gone from a remote leave the list.
    pub fn fetch(&self) -> Result<String, Error> {
        if self.remotes()?.is_empty() {
            return Err(Error::Parse("this repository has no remote to fetch from.".into()));
        }
        self.write(&["fetch", "--all", "--prune"])
    }

    /// [`GitCli::fetch`], and whether it changed anything here: a remote branch or tag that came, moved or went.
    pub fn fetch_changed(&self) -> Result<bool, Error> {
        let refs = || self.run(&["for-each-ref", "--format=%(objectname) %(refname)", "refs/remotes", "refs/tags"]);
        let before = refs()?;
        self.fetch()?;
        Ok(refs()? != before)
    }

    /// Applies one commit on top of the current branch. A merge commit is refused: which side to
    /// follow is a choice this does not make for you.
    pub fn cherry_pick(&self, id: &str) -> Result<Outcome, Error> {
        check_name(id)?;
        let parents = self.write(&["rev-list", "--parents", "-n", "1", id])?;
        if parents.split_whitespace().count() > 2 {
            return Err(Error::Parse("that is a merge commit; cherry-pick one of the commits it merged instead".into()));
        }
        self.finish(Operation::CherryPick, &["cherry-pick", id])
    }

    /// Gives up a merge, rebase or cherry-pick that stopped on conflicts, restoring what was there before.
    pub fn abort(&self, operation: Operation) -> Result<String, Error> {
        self.write(&[operation.name(), "--abort"])
    }

    /// The operation git is in the middle of here, if a previous one stopped on conflicts.
    pub fn in_progress(&self) -> Option<Operation> {
        let dir = self.write(&["rev-parse", "--absolute-git-dir"]).ok()?;
        let dir = PathBuf::from(dir.trim());
        if dir.join("rebase-merge").exists() || dir.join("rebase-apply").exists() {
            Some(Operation::Rebase)
        } else if dir.join("MERGE_HEAD").exists() {
            Some(Operation::Merge)
        } else if dir.join("CHERRY_PICK_HEAD").exists() {
            Some(Operation::CherryPick)
        } else {
            None
        }
    }

    /// Pushes a branch to its remote (its upstream's, else `origin`). Never forces: if the remote has
    /// commits you do not, git rejects the push and that is reported. A branch with no upstream gets
    /// one set.
    pub fn push_branch(&self, name: &str) -> Result<String, Error> {
        check_name(name)?;
        let remote = self
            .write(&["config", "--get", &format!("branch.{name}.remote")])
            .ok()
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty() && r != ".");
        match remote {
            Some(remote) => self.write(&["push", &remote, name]),
            None => {
                let remotes = self.remotes()?;
                let remote = default_remote(&remotes).ok_or_else(|| Error::Parse("this repository has no remote to push to".into()))?;
                self.write(&["push", "--set-upstream", remote, name])
            }
        }
    }

    /// Runs a merge, rebase or cherry-pick and sorts the result into done, conflicted, or failed.
    fn finish(&self, operation: Operation, args: &[&str]) -> Result<Outcome, Error> {
        let output = self.write_output(args)?;
        if output.status.success() {
            return Ok(Outcome::Done(text_of(&output)));
        }
        if self.in_progress() == Some(operation) {
            let unmerged = self.write(&["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            return Ok(Outcome::Conflicts { operation, files: unmerged.lines().filter(|l| !l.is_empty()).count() });
        }
        Err(Error::Git { status: output.status.code(), stderr: text_of(&output) })
    }
}

/// Git's output, stdout then stderr, as one string.
fn text_of(output: &std::process::Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

#[cfg(test)]
mod tests {
    use super::check_name;

    #[test]
    fn names_that_could_be_options_or_split_into_arguments_are_refused() {
        for bad in ["", "-D", "--force", "has space", "tab\tname", "new\nline"] {
            assert!(check_name(bad).is_err(), "{bad:?} should be refused");
        }
        for fine in ["main", "feat/redesign-booking", "origin/release/1.0.0", "v1.2.3", "abc123"] {
            assert!(check_name(fine).is_ok(), "{fine:?} should be allowed");
        }
    }
}
