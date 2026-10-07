//! Narrowing the graph: which branches to show, and which commits match a search.
//!
//! Hiding works on branches, not on single commits: a branch that is left out takes away the
//! commits only it has. What is left is always every commit some shown branch comes from, so each
//! commit's parents are still there and the graph keeps its shape.

use std::collections::HashSet;

use crate::model::{Commit, RefKind};

/// Which branches the graph shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    /// Every branch, remote ones included, with tags and stashes.
    #[default]
    All,
    /// Local branches, and the remote copies that point where they do.
    Local,
    /// Only the history of the commit HEAD is on.
    Current,
}

/// What a search box query asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    /// Nothing typed.
    None,
    /// Words to find in the message, author, branch names or commit id.
    Text(String),
    /// `path:src/app` — commits that changed this file or folder.
    Path(String),
    /// `code:fn main` — commits that added or removed this text (`git log -S`).
    Code(String),
}

impl Query {
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        let tail = |prefix: &str| text.strip_prefix(prefix).map(str::trim).filter(|rest| !rest.is_empty()).map(str::to_owned);
        if text.is_empty() {
            Query::None
        } else if let Some(path) = tail("path:") {
            Query::Path(path)
        } else if let Some(code) = tail("code:") {
            Query::Code(code)
        } else if text == "path:" || text == "code:" {
            Query::None
        } else {
            Query::Text(text.to_owned())
        }
    }
}

/// The commit's message, author, branch or tag names, or id (from its start) contain every word of
/// `query`, ignoring case.
pub fn matches_text(commit: &Commit, query: &str) -> bool {
    let haystack = |word: &str| {
        contains(&commit.summary, word)
            || contains(&commit.author, word)
            || contains(&commit.email, word)
            || commit.refs.iter().any(|r| contains(&r.name, word))
            || (word.len() >= 4 && commit.id.starts_with(&word.to_ascii_lowercase()))
    };
    query.split_whitespace().all(haystack)
}

fn contains(text: &str, word: &str) -> bool {
    text.to_lowercase().contains(&word.to_lowercase())
}

/// The commits to draw for `scope`, newest first as given, leaving out branches named in `hidden`
/// (as the graph names them: `feat/x`, or `origin/feat/x` for a remote-only branch). Their badges
/// go with them. The branch HEAD is on is always shown. With `stashes`, a stash shows when the
/// commit it was made on does; without, none do.
pub fn filter_commits(commits: &[Commit], scope: Scope, hidden: &HashSet<String>, stashes: bool) -> Vec<Commit> {
    if scope == Scope::All && hidden.is_empty() && stashes {
        return commits.to_vec();
    }
    let locals: HashSet<(&str, &str)> = commits
        .iter()
        .flat_map(|c| c.refs.iter().filter(|r| r.kind == RefKind::LocalBranch).map(|r| (r.name.as_str(), c.id.as_str())))
        .collect();
    let head_branch: Option<&str> = commits.iter().find_map(|c| {
        c.refs.windows(2).find(|pair| pair[0].kind == RefKind::Head && pair[1].kind == RefKind::LocalBranch).map(|pair| pair[1].name.as_str())
    });
    // `origin/feat/x` → `feat/x`.
    let tail = |name: &str| name.split_once('/').map_or(name.to_owned(), |(_, rest)| rest.to_owned());
    let is_hidden = |name: &str| Some(name) != head_branch && hidden.contains(name);

    let trimmed: Vec<Commit> = commits
        .iter()
        .map(|commit| {
            let mut commit = commit.clone();
            commit.refs.retain(|r| match r.kind {
                RefKind::LocalBranch => !is_hidden(&r.name),
                RefKind::RemoteBranch => {
                    let local = tail(&r.name);
                    let mirrors_local = locals.contains(&(local.as_str(), commit.id.as_str()));
                    let gone = is_hidden(&r.name) || is_hidden(&local);
                    !gone && (scope != Scope::Local || mirrors_local)
                }
                RefKind::Stash => stashes,
                RefKind::Head | RefKind::Tag => true,
            });
            commit
        })
        .collect();

    let is_tip = |commit: &Commit| {
        commit.refs.iter().any(|r| match scope {
            Scope::All => r.kind != RefKind::Stash,
            Scope::Local => matches!(r.kind, RefKind::LocalBranch | RefKind::Head),
            Scope::Current => r.kind == RefKind::Head,
        })
    };
    // Newest first, so every child is seen before its parents.
    let mut wanted: HashSet<&str> = HashSet::new();
    for commit in trimmed.iter() {
        if is_tip(commit) || wanted.contains(commit.id.as_str()) {
            wanted.insert(commit.id.as_str());
            wanted.extend(commit.parents.iter().map(String::as_str));
        }
    }
    let mut wanted: HashSet<String> = wanted.into_iter().map(str::to_owned).collect();
    // A stash hangs off the commit it was made on: it shows where that commit does.
    for commit in trimmed.iter().filter(|c| c.refs.iter().any(|r| r.kind == RefKind::Stash)) {
        if commit.parents.first().is_some_and(|base| wanted.contains(base)) {
            wanted.insert(commit.id.clone());
        }
    }
    trimmed.into_iter().filter(|c| wanted.contains(&c.id)).collect()
}

/// How many stashes there are.
pub fn stash_count(commits: &[Commit]) -> usize {
    commits.iter().filter(|c| c.refs.iter().any(|r| r.kind == RefKind::Stash)).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Ref;

    fn commit(id: &str, parents: &[&str], refs: &[(&str, RefKind)]) -> Commit {
        Commit {
            id: id.into(),
            parents: parents.iter().map(|p| (*p).into()).collect(),
            author: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            time: 0,
            date: String::new(),
            summary: format!("feat: work on {id}"),
            refs: refs.iter().map(|(name, kind)| Ref { name: (*name).into(), kind: *kind }).collect(),
            stash: None,
            committer: String::new(),
            committer_email: String::new(),
        }
    }

    /// docs (remote only) on r1; feat (HEAD) on r1; merged (squash-merged, local) on r0; release: r1 -> r0.
    fn history() -> Vec<Commit> {
        vec![
            commit("d1", &["r1"], &[("origin/docs", RefKind::RemoteBranch)]),
            commit("f1", &["r1"], &[("HEAD", RefKind::Head), ("feat", RefKind::LocalBranch), ("origin/feat", RefKind::RemoteBranch)]),
            commit("s1", &["f1"], &[("stash@{0}", RefKind::Stash)]),
            commit("m1", &["r0"], &[("merged", RefKind::LocalBranch)]),
            commit("r1", &["r0"], &[("release", RefKind::LocalBranch), ("origin/release", RefKind::RemoteBranch), ("v1", RefKind::Tag)]),
            commit("r0", &[], &[]),
        ]
    }

    fn ids(commits: &[Commit]) -> Vec<&str> {
        commits.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn all_with_nothing_hidden_is_everything() {
        assert_eq!(ids(&filter_commits(&history(), Scope::All, &HashSet::new(), true)), ["d1", "f1", "s1", "m1", "r1", "r0"]);
    }

    #[test]
    fn current_is_the_history_of_head_with_its_badges() {
        let shown = filter_commits(&history(), Scope::Current, &HashSet::new(), false);
        assert_eq!(ids(&shown), ["f1", "r1", "r0"]);
        assert_eq!(shown[1].refs.len(), 3, "release, its remote copy and the tag stay on the base");
    }

    #[test]
    fn local_drops_remote_only_branches_and_stashes_but_keeps_remote_copies() {
        let shown = filter_commits(&history(), Scope::Local, &HashSet::new(), false);
        assert_eq!(ids(&shown), ["f1", "m1", "r1", "r0"]);
        assert!(shown[0].refs.iter().any(|r| r.name == "origin/feat"));
    }

    #[test]
    fn hidden_branches_take_only_their_own_commits_and_never_the_current_one() {
        let hidden: HashSet<String> = ["merged", "origin/docs", "feat"].map(String::from).into();
        let shown = filter_commits(&history(), Scope::All, &hidden, true);
        assert_eq!(ids(&shown), ["f1", "s1", "r1", "r0"]);
        assert!(shown[0].refs.iter().any(|r| r.name == "feat"), "HEAD's branch stays");
    }

    #[test]
    fn stashes_show_where_the_commit_they_were_made_on_does() {
        let ids_for = |scope, stashes| ids(&filter_commits(&history(), scope, &HashSet::new(), stashes)).into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(!ids_for(Scope::All, false).contains(&"s1".to_owned()), "turned off");
        assert!(ids_for(Scope::Local, true).contains(&"s1".to_owned()), "made on feat, which is local");
        assert_eq!(ids_for(Scope::Current, true), ["f1", "s1", "r1", "r0"], "made on HEAD's commit");
        // A stash made on a commit that is not shown is not shown either.
        let mut history = history();
        history.insert(1, commit("s2", &["d1"], &[("stash@{1}", RefKind::Stash)]));
        let shown = filter_commits(&history, Scope::Local, &HashSet::new(), true);
        assert!(!ids(&shown).contains(&"s2"), "made on a remote-only branch");
        assert_eq!(stash_count(&history), 2);
    }

    #[test]
    fn queries_are_read_from_their_prefix() {
        assert_eq!(Query::parse("  "), Query::None);
        assert_eq!(Query::parse("path: src/app "), Query::Path("src/app".into()));
        assert_eq!(Query::parse("code:fn main"), Query::Code("fn main".into()));
        assert_eq!(Query::parse("path:"), Query::None);
        assert_eq!(Query::parse("fix login"), Query::Text("fix login".into()));
    }

    #[test]
    fn text_matches_every_word_anywhere_and_ids_from_the_start() {
        let mut c = commit("abcdef12", &[], &[("feat/login", RefKind::LocalBranch)]);
        c.summary = "feat: work".into();
        assert!(matches_text(&c, "WORK ada"));
        assert!(matches_text(&c, "login"));
        assert!(matches_text(&c, "abcd"));
        assert!(!matches_text(&c, "bcde"), "an id matches from its start");
        assert!(!matches_text(&c, "work bob"));
    }
}
