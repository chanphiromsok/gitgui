/// What kind of ref points at a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
    /// `stash@{n}`, added by the backend; git has no such ref name.
    Stash,
}

/// A branch, tag or HEAD pointing at a commit, named without its `refs/...` prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ref {
    pub name: String,
    pub kind: RefKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    /// First parent first. A stash commit keeps only its first parent, the commit it was made on.
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    /// Who made the commit, when that is not the author (a rebase, a cherry-pick, a merge on GitHub).
    pub committer: String,
    pub committer_email: String,
    /// Author time, seconds since the Unix epoch.
    pub time: i64,
    /// Author time in the user's time zone, like `6 Oct 2026 15:22`.
    pub date: String,
    pub summary: String,
    pub refs: Vec<Ref>,
    /// `Some(n)` when this commit is `stash@{n}`.
    pub stash: Option<usize>,
}

impl Commit {
    pub fn short_id(&self) -> &str {
        &self.id[..self.id.len().min(7)]
    }

    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

/// The full record of one commit, for the detail panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitDetail {
    pub id: String,
    pub parents: Vec<String>,
    pub author: String,
    pub author_email: String,
    pub committer: String,
    pub committer_email: String,
    /// Author time in the user's time zone, like `Tue Oct  6 2026 15:22:06 +0700`.
    pub date: String,
    /// The whole message, subject and body.
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
}

/// One file a commit changed, compared with its first parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    /// The path after the commit (the path before it, for a deletion).
    pub path: String,
    /// The path before the commit, for a rename or copy.
    pub old_path: Option<String>,
    pub status: FileStatus,
    /// Lines added; `None` for a binary file.
    pub additions: Option<u32>,
    /// Lines removed; `None` for a binary file.
    pub deletions: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelKind {
    Branch,
    RemoteBranch,
    Tag,
    Stash,
}

/// What a badge on a commit shows. A local branch and the remote branches of the same name share
/// one label, so `main` with `origin/main` reads as `main` tagged `origin`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    pub kind: LabelKind,
    /// Remotes whose branch of this name points at the same commit.
    pub remotes: Vec<String>,
    /// HEAD is on this branch (or this label is a detached HEAD).
    pub head: bool,
}

/// Groups a commit's refs into badges: the branch HEAD is on first, then other local branches,
/// remote-only branches, tags, and stashes.
pub fn labels(refs: &[Ref]) -> Vec<Label> {
    let head_branch = refs
        .windows(2)
        .find(|pair| pair[0].kind == RefKind::Head && pair[1].kind == RefKind::LocalBranch)
        .map(|pair| pair[1].name.as_str());
    let has_head = refs.iter().any(|r| r.kind == RefKind::Head);

    let locals: Vec<&Ref> = refs.iter().filter(|r| r.kind == RefKind::LocalBranch).collect();
    let remotes: Vec<&Ref> = refs.iter().filter(|r| r.kind == RefKind::RemoteBranch).collect();

    // `origin/feature/x` is the remote side of local `feature/x`; this returns `origin`.
    fn remote_of<'a>(remote: &'a str, local: &str) -> Option<&'a str> {
        remote.strip_suffix(local)?.strip_suffix('/')
    }

    let mut out: Vec<Label> = Vec::new();
    for local in &locals {
        out.push(Label {
            name: local.name.clone(),
            kind: LabelKind::Branch,
            remotes: remotes.iter().filter_map(|r| remote_of(&r.name, &local.name)).map(str::to_owned).collect(),
            head: head_branch == Some(local.name.as_str()),
        });
    }
    for remote in &remotes {
        if !locals.iter().any(|local| remote_of(&remote.name, &local.name).is_some()) {
            out.push(Label { name: remote.name.clone(), kind: LabelKind::RemoteBranch, remotes: Vec::new(), head: false });
        }
    }
    if has_head && head_branch.is_none() {
        out.insert(0, Label { name: "HEAD".into(), kind: LabelKind::Branch, remotes: Vec::new(), head: true });
    }
    for tag in refs.iter().filter(|r| r.kind == RefKind::Tag) {
        out.push(Label { name: tag.name.clone(), kind: LabelKind::Tag, remotes: Vec::new(), head: false });
    }
    for stash in refs.iter().filter(|r| r.kind == RefKind::Stash) {
        out.push(Label { name: stash.name.clone(), kind: LabelKind::Stash, remotes: Vec::new(), head: false });
    }
    // The branch HEAD is on leads.
    out.sort_by_key(|label| !label.head);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(name: &str, kind: RefKind) -> Ref {
        Ref { name: name.into(), kind }
    }

    #[test]
    fn a_local_branch_and_its_remote_share_one_label_and_head_leads() {
        let refs = [
            r("HEAD", RefKind::Head),
            r("release/1.0.0", RefKind::LocalBranch),
            r("origin/release/1.0.0", RefKind::RemoteBranch),
            r("feat/x", RefKind::LocalBranch),
        ];
        let got = labels(&refs);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "release/1.0.0");
        assert_eq!(got[0].remotes, ["origin"]);
        assert!(got[0].head);
        assert_eq!(got[1].name, "feat/x");
        assert!(got[1].remotes.is_empty());
        assert!(!got[1].head);
    }

    #[test]
    fn a_remote_branch_without_a_local_one_keeps_its_full_name() {
        let got = labels(&[r("origin/docs/status-report", RefKind::RemoteBranch)]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, LabelKind::RemoteBranch);
        assert_eq!(got[0].name, "origin/docs/status-report");
    }

    #[test]
    fn a_name_that_merely_ends_alike_is_not_a_match() {
        // `origin/prefix-main` is not the remote of local `main`.
        let got = labels(&[r("main", RefKind::LocalBranch), r("origin/prefix-main", RefKind::RemoteBranch)]);
        assert_eq!(got.len(), 2);
        assert!(got[0].remotes.is_empty());
        assert_eq!(got[1].name, "origin/prefix-main");
    }

    #[test]
    fn a_detached_head_gets_its_own_label_first() {
        let got = labels(&[r("HEAD", RefKind::Head), r("v1", RefKind::Tag)]);
        assert_eq!(got[0].name, "HEAD");
        assert!(got[0].head);
        assert_eq!(got[1].kind, LabelKind::Tag);
    }

    #[test]
    fn stashes_and_tags_follow_branches() {
        let got = labels(&[r("stash@{0}", RefKind::Stash), r("v2", RefKind::Tag), r("dev", RefKind::LocalBranch)]);
        let kinds: Vec<LabelKind> = got.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, [LabelKind::Branch, LabelKind::Tag, LabelKind::Stash]);
    }
}
