//! Names for the branch lines of the graph, and which branches count as trunks.

use crate::graph::Lineage;
use crate::model::{Commit, LabelKind, labels};
use crate::squash::subject_pr;

/// What a commit is, for the icon beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitKind {
    Commit,
    /// A merge of branches that is not a pull request.
    Merge,
    /// A pull request landing on a branch: a `Merge pull request #N` commit, or a squash commit titled `... (#N)`.
    PullRequest,
}

pub fn commit_kind(commit: &Commit) -> CommitKind {
    if subject_pr(&commit.summary).is_some() {
        CommitKind::PullRequest
    } else if commit.is_merge() {
        CommitKind::Merge
    } else {
        CommitKind::Commit
    }
}

/// How trunk-like a branch is: at a fork point the higher ranked line carries on, so `main` runs
/// straight through the commits feature branches were cut from. A `remote/` prefix is ignored.
pub fn branch_rank(name: &str) -> i32 {
    let name = name.strip_prefix("origin/").or_else(|| name.strip_prefix("upstream/")).unwrap_or(name);
    match name.to_ascii_lowercase().as_str() {
        "main" | "master" | "trunk" => 100,
        "develop" | "development" | "dev" => 80,
        "staging" | "production" | "prod" => 50,
        lower if lower.starts_with("release/") || lower.starts_with("release-") || lower.starts_with("releases/") => 60,
        _ => 10,
    }
}

/// The rank of the best branch pointing at this commit, or 0 when no branch does.
pub fn commit_rank(commit: &Commit) -> i32 {
    labels(&commit.refs)
        .iter()
        .filter(|label| matches!(label.kind, LabelKind::Branch | LabelKind::RemoteBranch))
        .map(|label| branch_rank(&label.name))
        .max()
        .unwrap_or(0)
}

/// The branch a merge commit brought in, read from its subject:
/// `Merge pull request #42 from owner/feat/x` → `feat/x`, `Merge branch 'x' into y` → `x`,
/// `Merge remote-tracking branch 'origin/x'` → `x`, `Merged in x (pull request #3)` → `x`.
pub fn merged_branch_name(subject: &str) -> Option<String> {
    let subject = subject.trim();
    if let Some(rest) = subject.strip_prefix("Merge pull request #") {
        let from = rest.split_once(" from ")?.1.split_whitespace().next()?;
        // GitHub names the source `owner/branch`.
        return from.split_once('/').map(|(_, branch)| branch.to_owned()).filter(|b| !b.is_empty());
    }
    if let Some(rest) = subject.strip_prefix("Merged in ") {
        return rest.split_whitespace().next().map(str::to_owned);
    }
    for prefix in ["Merge remote-tracking branch '", "Merge branch '", "Merge tag '"] {
        if let Some(rest) = subject.strip_prefix(prefix) {
            let name = rest.split('\'').next()?;
            let name = name.strip_prefix("origin/").or_else(|| name.strip_prefix("upstream/")).unwrap_or(name);
            return (!name.is_empty()).then(|| name.to_owned());
        }
    }
    // `git merge some/branch -m ...` and friends: `Merge some/branch: message` or `Merge some/branch`.
    let rest = subject.strip_prefix("Merge ")?;
    let word = rest.split(|c: char| c.is_whitespace() || c == ':').next()?;
    (!word.is_empty() && word != "branch" && word != "commit" && word != "pull" && word != "remote-tracking")
        .then(|| word.to_owned())
}

/// A display name for each line: the branch at its newest commit if there is one, else (for a line
/// that started as a merge's second parent) the branch the merge message names. `commit_at` maps a
/// row to its commit, `None` for a row that is not one.
pub fn lineage_names<'a>(lineages: &[Lineage], commit_at: impl Fn(usize) -> Option<&'a Commit>) -> Vec<Option<String>> {
    lineages
        .iter()
        .map(|line| {
            let by_branch = commit_at(line.tip_row).and_then(|commit| {
                labels(&commit.refs)
                    .into_iter()
                    .find(|label| matches!(label.kind, LabelKind::Branch | LabelKind::RemoteBranch))
                    .map(|label| label.name)
            });
            by_branch.or_else(|| {
                line.opened_by_merge.and_then(&commit_at).and_then(|merge| merged_branch_name(&merge.summary))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ref, RefKind};

    #[test]
    fn trunks_outrank_features_and_remote_prefixes_do_not_matter() {
        assert!(branch_rank("main") > branch_rank("develop"));
        assert!(branch_rank("develop") > branch_rank("release/1.0.0"));
        assert!(branch_rank("release/1.0.0") > branch_rank("feat/redesign-booking"));
        assert_eq!(branch_rank("origin/release/1.0.0"), branch_rank("release/1.0.0"));
        assert_eq!(branch_rank("origin/main"), branch_rank("main"));
        assert_eq!(branch_rank("Main"), branch_rank("main"));
        assert_eq!(branch_rank("feat/main-menu"), branch_rank("anything"), "only the whole name counts");
    }

    fn commit(summary: &str, refs: &[(&str, RefKind)]) -> Commit {
        Commit {
            id: "x".into(),
            parents: vec![],
            author: "A".into(),
            email: "a@b".into(),
            time: 0,
            date: String::new(),
            summary: summary.into(),
            refs: refs.iter().map(|(name, kind)| Ref { name: (*name).into(), kind: *kind }).collect(),
            stash: None,
        }
    }

    #[test]
    fn a_commit_ranks_by_its_best_branch() {
        let c = commit("s", &[("feat/x", RefKind::LocalBranch), ("origin/release/1.0.0", RefKind::RemoteBranch), ("v1", RefKind::Tag)]);
        assert_eq!(commit_rank(&c), branch_rank("release/1.0.0"));
        assert_eq!(commit_rank(&commit("s", &[("v1", RefKind::Tag)])), 0);
    }

    #[test]
    fn reads_the_merged_branch_from_common_merge_subjects() {
        let cases = [
            ("Merge pull request #42 from bstnt/feat/redesign-booking", Some("feat/redesign-booking")),
            ("Merge pull request #5 from owner/branch", Some("branch")),
            ("Merge branch 'release/1.0.0' into feat/redesign-booking", Some("release/1.0.0")),
            ("Merge branch 'x'", Some("x")),
            ("Merge remote-tracking branch 'origin/release/1.0.0' into feat/redesign-booking", Some("release/1.0.0")),
            ("Merged in feature/x (pull request #12)", Some("feature/x")),
            ("Merge heist/profile-location-sheet: booking delivery API and location sheet", Some("heist/profile-location-sheet")),
            ("Merge feat/y", Some("feat/y")),
            ("feat: redesign booking", None),
            ("Merge", None),
            ("Merge pull request #7", None),
        ];
        for (subject, expected) in cases {
            assert_eq!(merged_branch_name(subject).as_deref(), expected, "for {subject:?}");
        }
    }

    #[test]
    fn a_line_is_named_by_its_branch_else_by_the_merge_that_brought_it_in() {
        let tip = commit("work", &[("feat/x", RefKind::LocalBranch)]);
        let merge = commit("Merge pull request #1 from o/gone-branch", &[]);
        let plain = commit("nothing", &[]);
        let rows = [&merge, &tip, &plain];

        let line = |tip_row, opened_by_merge| Lineage {
            tip_row,
            last_row: tip_row,
            commits: 1,
            opened_by_merge,
            fork_row: None,
            base: None,
            rank: 0,
        };
        let lineages = [line(1, None), line(2, Some(0)), line(2, None)];
        let names = lineage_names(&lineages, |row| rows.get(row).copied());
        assert_eq!(names, [Some("feat/x".to_owned()), Some("gone-branch".to_owned()), None]);
    }

    #[test]
    fn pull_requests_merges_and_plain_commits_are_told_apart() {
        let plain = commit("fix: something", &[]);
        assert_eq!(commit_kind(&plain), CommitKind::Commit);
        assert_eq!(commit_kind(&commit("Merge pull request #42 from bstnt/feat/x", &[])), CommitKind::PullRequest);
        assert_eq!(commit_kind(&commit("feat: redesign booking (#36)", &[])), CommitKind::PullRequest, "a squash commit");
        assert_eq!(commit_kind(&commit("fix: mentions #36 in the middle", &[])), CommitKind::Commit);
        let mut merge = commit("Merge branch 'release/1.0.0' into feat/x", &[]);
        merge.parents = vec!["a".into(), "b".into()];
        assert_eq!(commit_kind(&merge), CommitKind::Merge);
    }
}
