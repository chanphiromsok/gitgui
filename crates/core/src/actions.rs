//! Operations that change the repository: checkout, branch and tag management, merge, rebase,
//! cherry-pick and push.
//!
//! Ground rules, so a click can never do more than it says:
//! - nothing is forced: no `--force`, no `reset --hard`, and deleting an unmerged branch needs `force`
//!   to be asked for by name;
//! - a merge, rebase or cherry-pick that meets conflicts is reported as [`Outcome::Conflicts`], can be
//!   finished with [`GitCli::continue_operation`] once every file is resolved, and undone with [`GitCli::abort`];
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
    /// It stopped on conflicts in this many files and is waiting: resolve them and
    /// [`GitCli::continue_operation`], or [`GitCli::abort`] to put everything back.
    Conflicts { operation: Operation, files: usize },
}

/// A side of a conflict named by what it means, never "ours" or "theirs" (which a rebase swaps).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideName {
    /// "On main", "Coming in from feature/x", "Already on main", "Your commit: Add retries", "The picked commit: …".
    pub title: String,
    /// The same in a word or two, for buttons: "main", "feature/x", "your commit", "picked commit".
    pub short: String,
    /// Where, in a sentence: "on main", "on feature/x", "in your commit", "in the picked commit".
    pub place: String,
}

/// A merge, rebase or cherry-pick that stopped, as git's own files in the repository describe it. Read from git each
/// time, so it is the same after the app is started again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationState {
    pub operation: Operation,
    /// What HEAD already has (git's "ours").
    pub current: SideName,
    /// What is being applied (git's "theirs").
    pub incoming: SideName,
    /// The commit being applied: `MERGE_HEAD`, `REBASE_HEAD` or `CHERRY_PICK_HEAD`.
    pub incoming_commit: Option<String>,
    /// The branch merged into, rebased, or picked onto; `None` on a detached HEAD.
    pub branch: Option<String>,
    /// The other branch in plain words: the one merged in, the new base of a rebase, the picked commit's short id.
    pub other: String,
    /// Where a rebase is: commit `n` of `m`.
    pub step: Option<(usize, usize)>,
}

impl OperationState {
    /// One line that says what is going on: "Merging feature into main", "Rebasing feature onto main · commit 2 of 3".
    pub fn headline(&self) -> String {
        let branch = self.branch.as_deref().unwrap_or("the detached HEAD");
        match self.operation {
            Operation::Merge => format!("Merging {} into {branch}", self.other),
            Operation::Rebase => match self.step {
                Some((n, of)) => format!("Rebasing {branch} onto {} · commit {n} of {of}", self.other),
                None => format!("Rebasing {branch} onto {}", self.other),
            },
            Operation::CherryPick => format!("Cherry-picking {} onto {branch}", self.other),
        }
    }
}

/// What a test merge says about a merge, rebase, pull or cherry-pick before it is run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preflight {
    /// It went through with no conflict.
    Clean,
    /// It stopped on conflicts in these files.
    Conflicts(Vec<String>),
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

    /// The repository's git folder (`.git`, or the one a worktree points at).
    fn git_dir(&self) -> Option<PathBuf> {
        let dir = self.write(&["rev-parse", "--absolute-git-dir"]).ok()?;
        Some(PathBuf::from(dir.trim()))
    }

    /// The operation git is in the middle of here, if a previous one stopped on conflicts.
    pub fn in_progress(&self) -> Option<Operation> {
        let dir = self.git_dir()?;
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

    /// What the operation in progress is doing, with its sides named by meaning; `None` when there is none.
    pub fn operation_state(&self) -> Option<OperationState> {
        let operation = self.in_progress()?;
        let dir = self.git_dir()?;
        let read = |path: PathBuf| std::fs::read_to_string(path).ok().map(|text| text.lines().next().unwrap_or("").trim().to_owned());
        let current_branch = || crate::backend::Backend::current_branch(self).ok().flatten();
        let (branch, incoming, other, step) = match operation {
            Operation::Merge => {
                let incoming = read(dir.join("MERGE_HEAD"));
                let named = read(dir.join("MERGE_MSG")).and_then(|line| merged_name(&line));
                let other = named.or_else(|| incoming.as_deref().and_then(|id| self.ref_at(id, None))).unwrap_or_else(|| short(incoming.as_deref()));
                (current_branch(), incoming, other, None)
            }
            Operation::Rebase => {
                let state = ["rebase-merge", "rebase-apply"].into_iter().map(|d| dir.join(d)).find(|d| d.exists())?;
                let branch = read(state.join("head-name")).and_then(|name| name.strip_prefix("refs/heads/").map(str::to_owned));
                let onto = read(state.join("onto"));
                let other = onto.as_deref().and_then(|id| self.ref_at(id, branch.as_deref())).unwrap_or_else(|| short(onto.as_deref()));
                let incoming = read(dir.join("REBASE_HEAD")).or_else(|| read(state.join("stopped-sha")));
                let number = |name: &str| read(state.join(name)).and_then(|n| n.parse::<usize>().ok());
                let step = number("msgnum").zip(number("end")).or_else(|| number("next").zip(number("last")));
                (branch, incoming, other, step)
            }
            Operation::CherryPick => {
                let incoming = read(dir.join("CHERRY_PICK_HEAD"));
                (current_branch(), incoming.clone(), short(incoming.as_deref()), None)
            }
        };
        let summary = incoming
            .as_deref()
            .filter(|id| check_name(id).is_ok())
            .and_then(|id| self.write(&["log", "-1", "--format=%s", id]).ok())
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        let (current, incoming_name) = side_names(operation, branch.as_deref(), &other, &summary);
        Some(OperationState { operation, current, incoming: incoming_name, incoming_commit: incoming, branch, other, step })
    }

    /// A branch (else a remote branch, else a tag) that points at `id`, other than `exclude`.
    fn ref_at(&self, id: &str, exclude: Option<&str>) -> Option<String> {
        check_name(id).ok()?;
        let points = format!("--points-at={id}");
        let out = self.write(&["for-each-ref", &points, "--format=%(refname)", "refs/heads", "refs/remotes", "refs/tags"]).ok()?;
        let refs: Vec<&str> = out.lines().collect();
        pick_ref(&refs, exclude)
    }

    /// Finishes the operation in progress once every file is resolved: commits a merge, or goes on with a rebase or
    /// cherry-pick, with no editor. A rebase that meets conflicts in its next commit stops again, as
    /// [`Outcome::Conflicts`].
    pub fn continue_operation(&self, operation: Operation) -> Result<Outcome, Error> {
        let left = self.unmerged()?.len();
        if left > 0 {
            return Err(Error::Parse(format!(
                "{left} file{} still {} conflicts; resolve {} first.",
                if left == 1 { "" } else { "s" },
                if left == 1 { "has" } else { "have" },
                if left == 1 { "it" } else { "them" }
            )));
        }
        // The commit message git prepared is kept as it is.
        let args: &[&str] = match operation {
            Operation::Merge => &["-c", "core.editor=true", "commit", "--no-edit"],
            Operation::Rebase => &["-c", "core.editor=true", "rebase", "--continue"],
            Operation::CherryPick => &["-c", "core.editor=true", "cherry-pick", "--continue"],
        };
        self.finish(operation, args)
    }

    /// Leaves out the commit a rebase or cherry-pick stopped on (and whatever was resolved in it), and goes on.
    pub fn skip_commit(&self, operation: Operation) -> Result<Outcome, Error> {
        match operation {
            Operation::Merge => Err(Error::Parse("a merge has no commit to skip; abort it instead.".into())),
            Operation::Rebase => self.finish(operation, &["-c", "core.editor=true", "rebase", "--skip"]),
            Operation::CherryPick => self.finish(operation, &["-c", "core.editor=true", "cherry-pick", "--skip"]),
        }
    }

    /// A test merge of `theirs` into `ours` (from `base`, when given, as a cherry-pick merges) with `git merge-tree
    /// --write-tree`, which touches neither the working tree nor the index. `None` when git cannot say: a git older
    /// than 2.38, or a name it does not know.
    fn test_merge(&self, ours: &str, theirs: &str, base: Option<&str>) -> Option<Preflight> {
        check_name(ours).ok()?;
        check_name(theirs).ok()?;
        let base = base.map(|b| format!("--merge-base={b}"));
        let mut args = vec!["merge-tree", "--write-tree", "--name-only", "--no-messages", "-z"];
        args.extend(base.as_deref());
        args.extend([ours, theirs]);
        let output = self.command(&args).stdin(Stdio::null()).output().ok()?;
        parse_merge_tree(output.status.code(), &output.stdout)
    }

    /// Would merging `branch` into the current branch conflict? Exact: it is the merge git would do.
    pub fn preflight_merge(&self, branch: &str) -> Option<Preflight> {
        self.test_merge("HEAD", branch, None)
    }

    /// Would rebasing onto `onto` conflict? Only a hint: the tips are merged once, while a rebase replays each commit.
    pub fn preflight_rebase(&self, onto: &str) -> Option<Preflight> {
        self.test_merge("HEAD", onto, None)
    }

    /// Would pulling with rebase conflict, as of the last fetch? Only a hint, as for a rebase.
    pub fn preflight_pull(&self) -> Option<Preflight> {
        self.test_merge("HEAD", "@{upstream}", None)
    }

    /// Would cherry-picking `id` conflict? Exact: the commit's changes are merged from its parent, as a cherry-pick does.
    pub fn preflight_cherry_pick(&self, id: &str) -> Option<Preflight> {
        check_name(id).ok()?;
        self.test_merge("HEAD", id, Some(&format!("{id}^")))
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
            let files = self.unmerged().map_or(0, |u| u.len());
            // Stopped with nothing in conflict (a commit left empty by its resolution): git's own words say what to do.
            if files > 0 {
                return Ok(Outcome::Conflicts { operation, files });
            }
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

/// The id cut to seven characters, or "a commit" when there is none.
fn short(id: Option<&str>) -> String {
    id.map_or_else(|| "a commit".to_owned(), |id| id.chars().take(7).collect())
}

/// The branch a merge brings in, from the first line of `MERGE_MSG`: `Merge branch 'feature' into main`,
/// `Merge remote-tracking branch 'origin/main'`, `Merge tag 'v1.0'`, `Merge commit 'abc123'`.
fn merged_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Merge ")?;
    let rest = ["remote-tracking branch ", "branch ", "tag ", "commit "].iter().find_map(|kind| rest.strip_prefix(kind))?;
    let name = rest.strip_prefix('\'')?.split('\'').next()?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// A name for a commit from the refs that point at it (full names): a local branch other than `exclude` first, then a
/// remote branch, then a tag.
fn pick_ref(refs: &[&str], exclude: Option<&str>) -> Option<String> {
    let local = refs.iter().filter_map(|r| r.strip_prefix("refs/heads/")).find(|name| Some(*name) != exclude);
    let remote = || refs.iter().filter_map(|r| r.strip_prefix("refs/remotes/")).find(|name| !name.ends_with("/HEAD"));
    let tag = || refs.iter().find_map(|r| r.strip_prefix("refs/tags/"));
    local.or_else(remote).or_else(tag).map(str::to_owned)
}

/// The two sides of a conflict, named by meaning. `branch` is the branch merged into, rebased, or picked onto; `other`
/// the branch merged in or rebased onto; `summary` the subject of the commit being applied.
fn side_names(operation: Operation, branch: Option<&str>, other: &str, summary: &str) -> (SideName, SideName) {
    let name = |title: String, short: &str, place: String| SideName { title, short: short.to_owned(), place };
    let here = branch.unwrap_or("HEAD");
    let commit = |lead: &str| if summary.is_empty() { lead.to_owned() } else { format!("{lead}: {summary}") };
    match operation {
        Operation::Merge => (
            name(format!("On {here}"), here, format!("on {here}")),
            name(format!("Coming in from {other}"), other, format!("on {other}")),
        ),
        Operation::Rebase => (
            name(format!("Already on {other}"), other, format!("on {other}")),
            name(commit("Your commit"), "your commit", "in your commit".into()),
        ),
        Operation::CherryPick => (
            name(format!("On {here}"), here, format!("on {here}")),
            name(commit("The picked commit"), "picked commit", "in the picked commit".into()),
        ),
    }
}

/// Reads `git merge-tree --write-tree --name-only --no-messages -z`: exit 0 with the tree id is clean, exit 1 with the
/// tree id and the conflicted paths is a conflict. Anything else (a failure also exits 1, with nothing printed) says
/// nothing.
fn parse_merge_tree(code: Option<i32>, stdout: &[u8]) -> Option<Preflight> {
    let mut fields = stdout.split(|b| *b == 0);
    let tree = fields.next()?;
    if tree.len() < 40 || !tree.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    match code? {
        0 => Some(Preflight::Clean),
        1 => {
            let mut paths: Vec<String> = fields.filter(|f| !f.is_empty()).map(|f| String::from_utf8_lossy(f).into_owned()).collect();
            paths.dedup();
            Some(Preflight::Conflicts(paths))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_could_be_options_or_split_into_arguments_are_refused() {
        for bad in ["", "-D", "--force", "has space", "tab\tname", "new\nline"] {
            assert!(check_name(bad).is_err(), "{bad:?} should be refused");
        }
        for fine in ["main", "feat/redesign-booking", "origin/release/1.0.0", "v1.2.3", "abc123"] {
            assert!(check_name(fine).is_ok(), "{fine:?} should be allowed");
        }
    }

    #[test]
    fn the_branch_a_merge_brings_in_is_read_from_its_message() {
        assert_eq!(merged_name("Merge branch 'feature/x'").as_deref(), Some("feature/x"));
        assert_eq!(merged_name("Merge branch 'feature' into main").as_deref(), Some("feature"));
        assert_eq!(merged_name("Merge remote-tracking branch 'origin/main'").as_deref(), Some("origin/main"));
        assert_eq!(merged_name("Merge tag 'v1.0'").as_deref(), Some("v1.0"));
        assert_eq!(merged_name("Merge commit 'abc123'").as_deref(), Some("abc123"));
        assert_eq!(merged_name("Merge branch 'dev' of https://example.com/repo").as_deref(), Some("dev"));
        assert_eq!(merged_name("Squashed work"), None);
    }

    #[test]
    fn a_commit_is_named_by_a_branch_first_and_never_by_the_branch_being_rebased() {
        let refs = ["refs/tags/v1", "refs/remotes/origin/HEAD", "refs/remotes/origin/main", "refs/heads/feature", "refs/heads/main"];
        assert_eq!(pick_ref(&refs, Some("feature")).as_deref(), Some("main"));
        assert_eq!(pick_ref(&refs[..3], None).as_deref(), Some("origin/main"));
        assert_eq!(pick_ref(&refs[..1], None).as_deref(), Some("v1"));
        assert_eq!(pick_ref(&[], None), None);
    }

    #[test]
    fn the_sides_are_named_by_what_they_mean_and_a_rebase_does_not_swap_them() {
        let (current, incoming) = side_names(Operation::Merge, Some("main"), "feature/x", "Add retries");
        assert_eq!((current.title.as_str(), incoming.title.as_str()), ("On main", "Coming in from feature/x"));
        assert_eq!((current.short.as_str(), incoming.short.as_str()), ("main", "feature/x"));
        let (current, incoming) = side_names(Operation::Rebase, Some("feature/x"), "main", "Add retries");
        assert_eq!((current.title.as_str(), incoming.title.as_str()), ("Already on main", "Your commit: Add retries"));
        assert_eq!((incoming.short.as_str(), incoming.place.as_str(), current.place.as_str()), ("your commit", "in your commit", "on main"));
        let (current, incoming) = side_names(Operation::CherryPick, None, "abc1234", "");
        assert_eq!((current.title.as_str(), incoming.title.as_str()), ("On HEAD", "The picked commit"));
    }

    #[test]
    fn a_test_merge_is_clean_conflicted_or_says_nothing() {
        let tree = "71ee6c83a3dbd3b8542ee4e95fe3f08e3eb45c93";
        assert_eq!(parse_merge_tree(Some(0), format!("{tree}\0").as_bytes()), Some(Preflight::Clean));
        let conflicted = format!("{tree}\0a.rs\0src/b.rs\0");
        assert_eq!(parse_merge_tree(Some(1), conflicted.as_bytes()), Some(Preflight::Conflicts(vec!["a.rs".into(), "src/b.rs".into()])));
        assert_eq!(parse_merge_tree(Some(1), b""), None, "a failure exits 1 too, with nothing printed");
        assert_eq!(parse_merge_tree(Some(129), b"usage: git merge-tree"), None, "an old git");
    }

    #[test]
    fn the_headline_says_what_is_going_on() {
        let state = |operation, step| OperationState {
            operation,
            current: SideName { title: String::new(), short: String::new(), place: String::new() },
            incoming: SideName { title: String::new(), short: String::new(), place: String::new() },
            incoming_commit: None,
            branch: Some("feature".into()),
            other: "main".into(),
            step,
        };
        assert_eq!(state(Operation::Merge, None).headline(), "Merging main into feature");
        assert_eq!(state(Operation::Rebase, Some((2, 3))).headline(), "Rebasing feature onto main · commit 2 of 3");
    }
}
