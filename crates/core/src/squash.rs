//! Telling that a branch has already been merged into a trunk even when git's history does not say so.
//!
//! A squash merge (or a rebase merge) copies a branch's changes into one new commit on the trunk. That
//! commit has no link to the branch, so the graph shows the branch as still unmerged. These are the
//! signals used to see through it, strongest first.

use std::collections::HashSet;

use crate::lineage::branch_rank;
use crate::model::{Commit, RefKind};

/// A branch or remote branch and the commit it points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchTip {
    pub name: String,
    pub id: String,
}

/// How a merge was recognized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// The branch's commits are already part of the target's history: a plain or fast-forward merge.
    Contained,
    /// A commit on the target makes exactly the same changes as the whole branch does.
    SamePatch,
    /// A commit on the target names a pull request that the branch's own commits name too.
    PullRequest,
    /// A commit on the target repeats the branch's commit messages, as GitHub's squash message does.
    Messages,
}

impl Evidence {
    /// Whether this is a guess from messages rather than a fact from history or content.
    pub fn is_probable(self) -> bool {
        matches!(self, Evidence::PullRequest | Evidence::Messages)
    }
}

/// What a scan found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeScan {
    pub clues: Vec<MergeClue>,
    /// Branches left unchecked because time ran out. Not finding a clue for these means nothing.
    pub unchecked: usize,
}

/// "This branch has already been merged into that one."
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeClue {
    pub branch: String,
    pub into: String,
    /// The commit on the target that carries the branch's changes; `None` when the branch is simply
    /// part of the target's history.
    pub commit: Option<String>,
    pub evidence: Evidence,
    /// The pull request number, when one was found.
    pub pr: Option<u32>,
}

/// The branches worth checking and the trunks to check them against. Trunk-like branches (`main`,
/// `release/…`, `develop`) are the targets, best first; every other branch is a candidate. A remote
/// branch that has a local copy is checked once, as the local one.
pub fn scan_inputs(commits: &[Commit]) -> (Vec<BranchTip>, Vec<BranchTip>) {
    const MOST_BRANCHES: usize = 60;
    let mut locals: Vec<BranchTip> = Vec::new();
    let mut remotes: Vec<BranchTip> = Vec::new();
    for commit in commits {
        for r in &commit.refs {
            let tip = BranchTip { name: r.name.clone(), id: commit.id.clone() };
            match r.kind {
                RefKind::LocalBranch => locals.push(tip),
                RefKind::RemoteBranch => remotes.push(tip),
                _ => {}
            }
        }
    }
    let is_target = |name: &str| branch_rank(name) >= 50;
    let mut targets: Vec<BranchTip> = locals.iter().chain(&remotes).filter(|t| is_target(&t.name)).cloned().collect();
    targets.sort_by(|a, b| branch_rank(&b.name).cmp(&branch_rank(&a.name)).then_with(|| a.name.cmp(&b.name)));

    let local_names: HashSet<&str> = locals.iter().map(|t| t.name.as_str()).collect();
    let mut branches: Vec<BranchTip> = locals.iter().filter(|t| !is_target(&t.name)).cloned().collect();
    branches.extend(
        remotes
            .iter()
            .filter(|t| !is_target(&t.name))
            .filter(|t| t.name.split_once('/').is_none_or(|(_, rest)| !local_names.contains(rest)))
            .cloned(),
    );
    branches.truncate(MOST_BRANCHES);
    (branches, targets)
}

/// Every `#123` in `text`, in order. A `#` inside a word (`abc#1`) does not count.
pub fn pr_numbers(text: &str) -> Vec<u32> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    for (at, byte) in bytes.iter().enumerate() {
        if *byte != b'#' || (at > 0 && bytes[at - 1].is_ascii_alphanumeric()) {
            continue;
        }
        let digits: String = text[at + 1..].chars().take_while(char::is_ascii_digit).collect();
        if let Ok(number) = digits.parse() {
            found.push(number);
        }
    }
    found
}

/// The number in a trailing `(#123)`, which is how GitHub titles a squash commit.
pub fn pr_suffix(subject: &str) -> Option<u32> {
    let inner = subject.trim_end().strip_suffix(')')?;
    let (_, number) = inner.rsplit_once("(#")?;
    number.parse().ok()
}

/// The number in `Merge pull request #123 from ...`.
fn pr_merge_number(subject: &str) -> Option<u32> {
    let rest = subject.strip_prefix("Merge pull request #")?;
    rest.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
}

/// The pull request a target commit's subject names (as a squash title or a PR merge), if any.
pub fn subject_pr(subject: &str) -> Option<u32> {
    pr_suffix(subject).or_else(|| pr_merge_number(subject))
}

/// The pull request the target commit names, if the branch's own commits name the same one.
pub fn shared_pr(branch_texts: &[String], target_subject: &str) -> Option<u32> {
    let wanted = subject_pr(target_subject)?;
    branch_texts.iter().any(|text| pr_numbers(text).contains(&wanted)).then_some(wanted)
}

/// A commit subject without the ` (#123)` GitHub adds to a squash title.
fn bare(subject: &str) -> &str {
    let subject = subject.trim();
    match (pr_suffix(subject), subject.rfind(" (#")) {
        (Some(_), Some(at)) => subject[..at].trim_end(),
        _ => subject,
    }
}

/// Whether `message` (a target commit's subject and body) repeats the branch's commit subjects.
/// GitHub's squash body lists each commit as `* subject`; a one-commit branch keeps its subject as
/// the title. Very short subjects (`wip`, `fix`) say nothing and are ignored.
pub fn repeats_subjects(branch_subjects: &[String], message: &str) -> bool {
    let mut subjects: Vec<&str> = branch_subjects.iter().map(|s| bare(s)).filter(|s| s.chars().count() >= 8).collect();
    subjects.sort_unstable();
    subjects.dedup();
    if subjects.is_empty() {
        return false;
    }
    let hits = subjects.iter().filter(|subject| message.contains(**subject)).count();
    let needed = if subjects.len() == 1 { 1 } else { 2.max((subjects.len() * 3).div_ceil(5)) };
    hits >= needed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn finds_pull_request_numbers_but_not_hashes_inside_words() {
        assert_eq!(pr_numbers("fix: split POIList (PR #42 review)"), [42]);
        assert_eq!(pr_numbers("address PR #36 and #37"), [36, 37]);
        assert_eq!(pr_numbers("color #ffcc00 and issue#9 and #"), Vec::<u32>::new());
        assert_eq!(pr_numbers("#7 at the start"), [7]);
    }

    #[test]
    fn a_squash_title_ends_in_the_pr_number() {
        assert_eq!(pr_suffix("feat: redesign booking (#42)"), Some(42));
        assert_eq!(pr_suffix("feat: redesign booking (#42)  "), Some(42));
        assert_eq!(pr_suffix("feat: redesign (see #42)"), None);
        assert_eq!(pr_suffix("feat: redesign"), None);
        assert_eq!(subject_pr("Merge pull request #42 from bstnt/feat/x"), Some(42));
        assert_eq!(subject_pr("fix: something"), None);
    }

    #[test]
    fn a_target_commit_matches_when_branch_commits_name_the_same_pull_request() {
        let branch = texts(&["fix: split POIList and StopRow (PR #42 review)", "feat: redesign track shipment"]);
        assert_eq!(shared_pr(&branch, "feat: redesign booking (#42)"), Some(42));
        assert_eq!(shared_pr(&branch, "feat: something else (#43)"), None);
        assert_eq!(shared_pr(&branch, "no number here"), None);
        // The number only counts if the branch itself mentions it.
        assert_eq!(shared_pr(&texts(&["feat: redesign track shipment"]), "feat: redesign booking (#42)"), None);
    }

    #[test]
    fn a_github_squash_body_repeats_the_branch_commit_subjects() {
        let branch = texts(&["feat: redesign booking", "fix: booking delivery form validation", "chore: refresh locale"]);
        let body = "feat: redesign booking (#36)\n\n* feat: redesign booking\n* fix: booking delivery form validation\n* chore: refresh locale";
        assert!(repeats_subjects(&branch, body));
        // Two of three is enough; one of three is not.
        assert!(repeats_subjects(&branch, "title\n\n* feat: redesign booking\n* chore: refresh locale"));
        assert!(!repeats_subjects(&branch, "title\n\n* feat: redesign booking"));
    }

    #[test]
    fn a_one_commit_branch_matches_its_own_subject_with_or_without_the_pr_suffix() {
        let branch = texts(&["fix: polish a empty state of notification"]);
        assert!(repeats_subjects(&branch, "fix: polish a empty state of notification (#38)"));
        assert!(!repeats_subjects(&branch, "fix: something unrelated entirely"));
        // And the branch's own subject may carry a suffix, if it was amended after a squash.
        assert!(repeats_subjects(&texts(&["fix: polish a empty state of notification (#38)"]), "fix: polish a empty state of notification"));
    }

    #[test]
    fn short_or_empty_subjects_prove_nothing() {
        assert!(!repeats_subjects(&texts(&["wip", "fix"]), "wip fix wip"));
        assert!(!repeats_subjects(&[], "anything"));
    }

    #[test]
    fn only_messages_and_pull_requests_are_guesses() {
        assert!(!Evidence::Contained.is_probable());
        assert!(!Evidence::SamePatch.is_probable());
        assert!(Evidence::PullRequest.is_probable());
        assert!(Evidence::Messages.is_probable());
    }

    fn tip_commit(id: &str, refs: &[(&str, RefKind)]) -> Commit {
        Commit {
            id: id.into(),
            parents: vec![],
            author: "A".into(),
            email: "a@b".into(),
            time: 0,
            date: String::new(),
            summary: id.into(),
            refs: refs.iter().map(|(n, k)| crate::model::Ref { name: (*n).into(), kind: *k }).collect(),
            stash: None,
            committer: String::new(),
            committer_email: String::new(),
        }
    }

    #[test]
    fn trunks_are_the_targets_and_the_rest_are_checked_once_each() {
        let commits = [
            tip_commit("a", &[("release/1.0.0", RefKind::LocalBranch), ("origin/release/1.0.0", RefKind::RemoteBranch)]),
            tip_commit("b", &[("feat/x", RefKind::LocalBranch), ("origin/feat/x", RefKind::RemoteBranch)]),
            tip_commit("c", &[("origin/feat/remote-only", RefKind::RemoteBranch), ("main", RefKind::LocalBranch)]),
            tip_commit("d", &[("v1", RefKind::Tag), ("feat/local-only", RefKind::LocalBranch)]),
        ];
        let (branches, targets) = scan_inputs(&commits);
        let names = |tips: &[BranchTip]| tips.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&targets), ["main", "origin/release/1.0.0", "release/1.0.0"]);
        assert_eq!(names(&branches), ["feat/x", "feat/local-only", "origin/feat/remote-only"]);
    }
}
