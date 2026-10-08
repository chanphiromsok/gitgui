//! How local branches stand against the remote branches they track (their upstreams), read for every branch
//! with one `git for-each-ref`, and when an automatic fetch is due.
//!
//! The counts are as of the last fetch: git compares with the remote-tracking branches it has, not with the
//! remote itself, so nothing here touches the network.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::backend::{Error, GitCli};
use crate::model::{Commit, RefKind};

/// Where a local branch stands against its upstream, the remote branch it pushes to and pulls from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Upstream {
    /// Not on a remote: never pushed, made without tracking anything, or tracking another local branch.
    None,
    /// It tracks `name` (`origin/feature/x`) on `remote`; `ahead` commits are only here, `behind` only there.
    Tracking { name: String, remote: String, ahead: usize, behind: usize },
    /// It tracked `name`, which is gone: deleted on the remote, then pruned here by a fetch.
    Gone { name: String, remote: String },
}

impl Upstream {
    /// The remote branch, `origin/feature/x`, when there is (or was) one.
    pub fn name(&self) -> Option<&str> {
        match self {
            Upstream::None => None,
            Upstream::Tracking { name, .. } | Upstream::Gone { name, .. } => Some(name),
        }
    }

    /// Commits here that the upstream does not have: what a push would send.
    pub fn ahead(&self) -> usize {
        if let Upstream::Tracking { ahead, .. } = self { *ahead } else { 0 }
    }

    /// Commits on the upstream that are not here: what a pull would bring.
    pub fn behind(&self) -> usize {
        if let Upstream::Tracking { behind, .. } = self { *behind } else { 0 }
    }
}

/// Which commits are only here and which only there, for the branches that track a remote one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Divergence {
    /// Commits a push would send: on a local branch and not on its upstream.
    pub unpushed: HashSet<String>,
    /// Commits a pull would bring: on an upstream and not on its local branch.
    pub unpulled: HashSet<String>,
}

/// Every commit among `by_id` that `start` can reach, itself included.
fn reach<'a>(start: &str, by_id: &HashMap<&'a str, &'a Commit>) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut todo = vec![start];
    while let Some(id) = todo.pop() {
        let Some(commit) = by_id.get(id) else { continue };
        if seen.insert(commit.id.as_str()) {
            todo.extend(commit.parents.iter().map(String::as_str));
        }
    }
    seen
}

/// The commits that differ between each local branch and the remote one it tracks, among `commits` (as of the last
/// fetch). `tracked` is `(branch, upstream)` by the names the refs have (`feat/x`, `origin/feat/x`); only branches that
/// have moved apart need to be given. `unpublished` is the branch HEAD is on when it tracks nothing: its commits that
/// no remote branch has are unpushed too, so a branch not on a remote yet shows what a first push would send.
pub fn divergence(commits: &[Commit], tracked: &[(String, String)], unpublished: Option<&str>) -> Divergence {
    let by_id: HashMap<&str, &Commit> = commits.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut tips: HashMap<&str, &str> = HashMap::new();
    for commit in commits {
        for r in &commit.refs {
            if matches!(r.kind, RefKind::LocalBranch | RefKind::RemoteBranch) {
                tips.insert(r.name.as_str(), commit.id.as_str());
            }
        }
    }
    let mut found = Divergence::default();
    for (branch, upstream) in tracked {
        let (Some(local), Some(remote)) = (tips.get(branch.as_str()), tips.get(upstream.as_str())) else { continue };
        if local == remote {
            continue;
        }
        let (here, there) = (reach(local, &by_id), reach(remote, &by_id));
        found.unpushed.extend(here.difference(&there).map(|id| (*id).to_owned()));
        found.unpulled.extend(there.difference(&here).map(|id| (*id).to_owned()));
    }
    if let Some(head) = unpublished.and_then(|branch| tips.get(branch)) {
        let mut on_remote: HashSet<&str> = HashSet::new();
        for commit in commits {
            if commit.refs.iter().any(|r| r.kind == RefKind::RemoteBranch) {
                on_remote.extend(reach(&commit.id, &by_id));
            }
        }
        // Only when some remote branch exists at all: with none, every commit would be "unpushed" and say nothing.
        if !on_remote.is_empty() {
            found.unpushed.extend(reach(head, &by_id).difference(&on_remote).map(|id| (*id).to_owned()));
        }
    }
    found
}

/// One local branch and its upstream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchSync {
    pub branch: String,
    pub upstream: Upstream,
}

/// Branch, upstream, the upstream's remote, and `ahead 2, behind 1` (or `gone`, or nothing when in step), split by NUL.
/// `lstrip=2` rather than `:short`, which turns a name that is also a tag's into `heads/name`.
const TRACKING_FORMAT: &str =
    "--format=%(refname:lstrip=2)%00%(upstream:lstrip=2)%00%(upstream:remotename)%00%(upstream:track,nobracket)";

/// Parses `git for-each-ref` printed with [`TRACKING_FORMAT`]: one branch a line.
pub fn parse_tracking(output: &str) -> Vec<BranchSync> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let branch = fields.next().filter(|b| !b.is_empty())?.to_owned();
            let (name, remote, track) = (fields.next().unwrap_or(""), fields.next().unwrap_or(""), fields.next().unwrap_or(""));
            // An upstream on remote `.` is another local branch: still nothing on a remote.
            let upstream = if name.is_empty() || remote == "." {
                Upstream::None
            } else {
                let remote = if remote.is_empty() { name.split('/').next().unwrap_or(name) } else { remote };
                let (name, remote) = (name.to_owned(), remote.to_owned());
                match parse_track(track) {
                    Some((ahead, behind)) => Upstream::Tracking { name, remote, ahead, behind },
                    None => Upstream::Gone { name, remote },
                }
            };
            Some(BranchSync { branch, upstream })
        })
        .collect()
}

/// `ahead 2, behind 1` → (2, 1); nothing → (0, 0), in step; `gone` → `None`.
fn parse_track(track: &str) -> Option<(usize, usize)> {
    if track.trim() == "gone" {
        return None;
    }
    let (mut ahead, mut behind) = (0, 0);
    for part in track.split(',').map(str::trim) {
        if let Some(n) = part.strip_prefix("ahead ") {
            ahead = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            behind = n.trim().parse().unwrap_or(0);
        }
    }
    Some((ahead, behind))
}

/// Why pulling `branch` would have nothing to do, in words for the person: it is not on a remote, or its remote
/// branch is gone. `None` when it tracks a remote branch.
pub fn nothing_to_pull(branch: &str, upstream: &Upstream) -> Option<String> {
    match upstream {
        Upstream::None => Some(format!("{branch} is not on a remote yet, so there is nothing to pull. Push it first.")),
        Upstream::Gone { name, .. } => {
            Some(format!("{name} was deleted from the remote, so there is nothing to pull. Push {branch} to put it back there."))
        }
        Upstream::Tracking { .. } => None,
    }
}

/// The remote a branch with no upstream is pushed to: `origin`, else the first one.
pub fn default_remote(remotes: &[String]) -> Option<&str> {
    remotes.iter().find(|r| *r == "origin").or(remotes.first()).map(String::as_str)
}

/// Whether the open project is due an automatic fetch: fetching every `every_minutes` (0 is off), and the last
/// fetch, by hand or not, was at least that long ago, or there has been none yet.
pub fn fetch_due(every_minutes: u32, last: Option<Instant>, now: Instant) -> bool {
    if every_minutes == 0 {
        return false;
    }
    last.is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(u64::from(every_minutes) * 60))
}

impl GitCli {
    /// Every local branch and where it stands against its upstream, from one `git for-each-ref` (git counts each
    /// branch's commits itself; no process per branch).
    pub fn branch_sync(&self) -> Result<Vec<BranchSync>, Error> {
        let out = self.run(&["for-each-ref", TRACKING_FORMAT, "refs/heads"])?;
        Ok(parse_tracking(&String::from_utf8_lossy(&out)))
    }

    /// Where one local branch stands against its upstream.
    pub fn upstream_of(&self, branch: &str) -> Result<Upstream, Error> {
        let out = self.run(&["for-each-ref", TRACKING_FORMAT, &format!("refs/heads/{branch}")])?;
        let found = parse_tracking(&String::from_utf8_lossy(&out)).into_iter().find(|b| b.branch == branch);
        Ok(found.map_or(Upstream::None, |b| b.upstream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, parents: &[&str], refs: &[(&str, RefKind)]) -> Commit {
        Commit {
            id: id.into(),
            parents: parents.iter().map(|p| (*p).into()).collect(),
            author: String::new(),
            email: String::new(),
            time: 0,
            date: String::new(),
            summary: String::new(),
            refs: refs.iter().map(|(name, kind)| crate::model::Ref { name: (*name).into(), kind: *kind }).collect(),
            stash: None,
            committer: String::new(),
            committer_email: String::new(),
        }
    }

    /// feat: l2 -> l1 -> b; origin/feat: r1 -> b (one commit each side).
    fn diverged() -> Vec<Commit> {
        vec![
            commit("l2", &["l1"], &[("feat", RefKind::LocalBranch)]),
            commit("r1", &["b"], &[("origin/feat", RefKind::RemoteBranch)]),
            commit("l1", &["b"], &[]),
            commit("b", &[], &[]),
        ]
    }

    #[test]
    fn each_side_of_a_diverged_branch_has_its_own_commits() {
        let found = divergence(&diverged(), &[("feat".into(), "origin/feat".into())], None);
        assert_eq!(found.unpushed, HashSet::from(["l2".to_owned(), "l1".to_owned()]));
        assert_eq!(found.unpulled, HashSet::from(["r1".to_owned()]));
    }

    #[test]
    fn a_branch_not_on_a_remote_yet_shows_what_a_first_push_would_send() {
        let mut commits = diverged();
        commits[1].refs = vec![crate::model::Ref { name: "origin/main".into(), kind: RefKind::RemoteBranch }];
        // feat tracks nothing; origin/main is at r1, so r1 and b are on a remote and l2 and l1 are not.
        let found = divergence(&commits, &[], Some("feat"));
        assert_eq!(found.unpushed, HashSet::from(["l2".to_owned(), "l1".to_owned()]));
        assert!(found.unpulled.is_empty());
    }

    #[test]
    fn with_no_remote_branch_at_all_nothing_is_unpushed() {
        let commits = vec![commit("c2", &["c1"], &[("main", RefKind::LocalBranch)]), commit("c1", &[], &[])];
        assert_eq!(divergence(&commits, &[], Some("main")), Divergence::default());
    }

    fn tracking(name: &str, ahead: usize, behind: usize) -> Upstream {
        Upstream::Tracking { name: name.into(), remote: "origin".into(), ahead, behind }
    }

    #[test]
    fn each_branch_gets_its_upstream_and_the_counts_git_printed() {
        let out = "main\0origin/main\0origin\0ahead 1\n\
                   feat/x\0origin/feat/x\0origin\0ahead 2, behind 3\n\
                   behind\0origin/develop\0origin\0behind 12\n\
                   synced\0origin/synced\0origin\0\n";
        let got = parse_tracking(out);
        let upstreams: Vec<(&str, &Upstream)> = got.iter().map(|b| (b.branch.as_str(), &b.upstream)).collect();
        assert_eq!(
            upstreams,
            [
                ("main", &tracking("origin/main", 1, 0)),
                ("feat/x", &tracking("origin/feat/x", 2, 3)),
                ("behind", &tracking("origin/develop", 0, 12)),
                ("synced", &tracking("origin/synced", 0, 0)),
            ]
        );
        assert_eq!((got[1].upstream.ahead(), got[1].upstream.behind()), (2, 3));
    }

    #[test]
    fn a_branch_never_pushed_or_tracking_a_local_branch_is_not_on_a_remote() {
        let got = parse_tracking("local\0\0\0\ntracks-main\0main\0.\0\n");
        assert_eq!(got[0].upstream, Upstream::None);
        assert_eq!(got[1].upstream, Upstream::None, "remote `.` is this repository");
        assert_eq!(got[0].upstream.name(), None);
    }

    #[test]
    fn an_upstream_deleted_on_the_remote_is_gone_not_in_step() {
        let got = parse_tracking("old\0origin/old\0origin\0gone\n");
        assert_eq!(got[0].upstream, Upstream::Gone { name: "origin/old".into(), remote: "origin".into() });
        assert_eq!((got[0].upstream.ahead(), got[0].upstream.behind()), (0, 0));
        assert_eq!(got[0].upstream.name(), Some("origin/old"));
    }

    #[test]
    fn a_remote_git_does_not_name_is_read_from_the_upstream() {
        let got = parse_tracking("x\0fork/x\0\0behind 1\n");
        assert_eq!(got[0].upstream, Upstream::Tracking { name: "fork/x".into(), remote: "fork".into(), ahead: 0, behind: 1 });
    }

    #[test]
    fn nothing_printed_is_no_branches_and_a_short_line_is_not_a_panic() {
        assert!(parse_tracking("").is_empty());
        assert_eq!(parse_tracking("lonely\n")[0].upstream, Upstream::None);
    }

    #[test]
    fn origin_is_pushed_to_first_else_the_first_remote() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(default_remote(&names(&["fork", "origin"])), Some("origin"));
        assert_eq!(default_remote(&names(&["fork", "upstream"])), Some("fork"));
        assert_eq!(default_remote(&[]), None);
    }

    #[test]
    fn a_fetch_is_due_when_on_and_the_last_one_was_long_enough_ago() {
        let now = Instant::now() + Duration::from_secs(3600);
        let ago = |minutes: u64| Some(now - Duration::from_secs(minutes * 60));
        assert!(!fetch_due(0, None, now), "off never fetches");
        assert!(!fetch_due(0, ago(600), now));
        assert!(fetch_due(5, None, now), "never fetched: fetch now");
        assert!(!fetch_due(5, ago(4), now));
        assert!(fetch_due(5, ago(5), now));
        assert!(!fetch_due(15, ago(14), now));
        assert!(fetch_due(30, ago(31), now));
        assert!(!fetch_due(5, Some(now + Duration::from_secs(1)), now), "a last fetch after now is not overdue");
    }
}
