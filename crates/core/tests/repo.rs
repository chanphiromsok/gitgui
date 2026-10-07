//! Runs the git backend against real repositories built with the `git` binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use gitgui_core::{
    Backend, BranchTip, CheckoutTarget, Error, Evidence, GitCli, LaneLayout, LogOptions, Operation, Outcome, Query, RefKind,
};

struct TempRepo(PathBuf);

impl TempRepo {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gitgui-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Self(dir);
        repo.git(&["init", "-q", "-b", "main"]);
        repo
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(args)
            // Keep the developer's own git config out of the fixture.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Ada")
            .env("GIT_AUTHOR_EMAIL", "ada@example.com")
            .env("GIT_COMMITTER_NAME", "Ada")
            .env("GIT_COMMITTER_EMAIL", "ada@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn commit(&self, file: &str, message: &str) {
        std::fs::write(self.0.join(file), message).unwrap();
        self.git(&["add", file]);
        self.git(&["commit", "-q", "-m", message]);
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// a ── b ───── m   (main)
///  \         /
///   └── c ──┘     (feature)
fn merged_repo(name: &str) -> TempRepo {
    let repo = TempRepo::new(name);
    repo.commit("a.txt", "a");
    repo.git(&["tag", "v1"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.commit("c.txt", "c");
    repo.git(&["checkout", "-q", "main"]);
    repo.commit("b.txt", "b");
    repo.git(&["merge", "-q", "--no-ff", "-m", "m", "feature"]);
    repo
}

#[test]
fn log_returns_children_before_parents_with_refs() {
    let repo = merged_repo("log");
    let commits = GitCli::new(repo.path()).log(&LogOptions::default()).unwrap();

    let summaries: Vec<&str> = commits.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries.len(), 4);
    assert_eq!(summaries[0], "m");
    assert_eq!(*summaries.last().unwrap(), "a");

    let merge = &commits[0];
    assert_eq!(merge.parents.len(), 2);
    assert_eq!(merge.parents[0], commits.iter().find(|c| c.summary == "b").unwrap().id);
    let has = |name: &str, kind| merge.refs.iter().any(|r| r.name == name && r.kind == kind);
    assert!(has("HEAD", RefKind::Head));
    assert!(has("main", RefKind::LocalBranch));

    let root = commits.last().unwrap();
    assert!(root.parents.is_empty());
    assert!(root.refs.iter().any(|r| r.name == "v1" && r.kind == RefKind::Tag));
    assert_eq!(merge.author, "Ada");
}

#[test]
fn layout_of_a_real_merge_uses_two_lanes_and_closes_them() {
    let repo = merged_repo("layout");
    let commits = GitCli::new(repo.path()).log(&LogOptions::default()).unwrap();

    let mut layout = LaneLayout::new();
    let rows: Vec<_> = commits.iter().map(|c| layout.push(&c.id, &c.parents)).collect();

    assert_eq!(rows[0].lane, 0, "the merge sits on the main lane");
    assert_eq!(rows.iter().map(|r| r.width).max(), Some(2));
    assert_eq!(layout.open_lanes(), 0, "nothing is left waiting after the root commit");
}

#[test]
fn a_file_reads_as_it_was_at_a_commit_and_is_none_where_it_is_missing() {
    let repo = merged_repo("file-at");
    let git = GitCli::new(repo.path());
    let commits = git.log(&LogOptions::default()).unwrap();
    let c = &commits.iter().find(|c| c.summary == "c").unwrap().id;
    assert_eq!(git.file_at(c, "c.txt").unwrap().as_deref(), Some("c"));
    assert_eq!(git.file_at(&format!("{c}^1"), "c.txt").unwrap(), None, "not there before c");
    assert_eq!(git.file_at(c, "b.txt").unwrap(), None, "b is on the other branch");
}

#[test]
fn the_remote_url_is_origin_s_or_none() {
    let repo = merged_repo("remote-url");
    let git = GitCli::new(repo.path());
    assert_eq!(git.remote_url().unwrap(), None);
    repo.git(&["remote", "add", "upstream", "git@github.com:a/up.git"]);
    assert_eq!(git.remote_url().unwrap().as_deref(), Some("git@github.com:a/up.git"), "the only one");
    repo.git(&["remote", "add", "origin", "https://github.com/a/b.git"]);
    assert_eq!(git.remote_url().unwrap().as_deref(), Some("https://github.com/a/b.git"), "origin first");
}

#[test]
fn search_finds_commits_by_the_file_they_touched_and_the_text_they_changed() {
    let repo = merged_repo("search");
    let git = GitCli::new(repo.path());
    let commits = git.log(&LogOptions::default()).unwrap();
    let id = |summary: &str| commits.iter().find(|c| c.summary == summary).unwrap().id.clone();

    assert_eq!(git.search(&Query::Path("c.txt".into())).unwrap(), [id("c")].into());
    assert_eq!(git.search(&Query::Code("b".into())).unwrap(), [id("b")].into());
    assert!(git.search(&Query::Path("nothing-here".into())).unwrap().is_empty());
    assert!(git.search(&Query::Text("b".into())).unwrap().is_empty(), "text is matched in the app, not by git");
}

#[test]
fn paging_with_skip_and_max_count_matches_one_big_read() {
    let repo = merged_repo("paging");
    let git = GitCli::new(repo.path());
    let whole = git.log(&LogOptions::default()).unwrap();

    let mut paged = Vec::new();
    for skip in (0..whole.len()).step_by(3) {
        paged.extend(git.log(&LogOptions { max_count: Some(3), skip }).unwrap());
    }
    assert_eq!(whole, paged);
}

#[test]
fn a_repo_with_no_commits_has_an_empty_log() {
    let repo = TempRepo::new("empty");
    let commits = GitCli::new(repo.path()).log(&LogOptions::default()).unwrap();
    assert!(commits.is_empty());
}

#[test]
fn a_folder_that_is_not_a_repo_is_a_git_error() {
    let dir = std::env::temp_dir().join(format!("gitgui-test-{}-norepo", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let result = GitCli::new(&dir).log(&LogOptions::default());
    let _ = std::fs::remove_dir_all(&dir);
    assert!(matches!(result, Err(Error::Git { .. })), "got {result:?}");
}

#[test]
fn a_stash_shows_as_one_commit_without_its_internal_commits() {
    let repo = merged_repo("stash");
    std::fs::write(repo.path().join("a.txt"), "changed").unwrap();
    repo.git(&["stash", "push", "-q", "-m", "wip"]);

    let commits = GitCli::new(repo.path()).log(&LogOptions::default()).unwrap();

    assert_eq!(commits.len(), 5, "four real commits plus the stash, and no `index on` commit");
    assert!(commits.iter().all(|c| !c.summary.starts_with("index on")));
    let stash = commits.iter().find(|c| c.stash == Some(0)).expect("the stash commit");
    assert_eq!(stash.parents.len(), 1, "only the commit it was made on");
    assert!(stash.refs.iter().any(|r| r.name == "stash@{0}" && r.kind == RefKind::Stash));
}

#[test]
fn refs_outside_branches_and_tags_add_no_commits() {
    let repo = merged_repo("pullrefs");
    repo.git(&["checkout", "-q", "-b", "tmp"]);
    repo.commit("t.txt", "tmp");
    repo.git(&["update-ref", "refs/pull/1/head", "HEAD"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["branch", "-q", "-D", "tmp"]);

    let commits = GitCli::new(repo.path()).log(&LogOptions::default()).unwrap();

    assert_eq!(commits.len(), 4);
    assert!(commits.iter().all(|c| c.summary != "tmp"));
}

#[test]
fn commits_carry_a_formatted_local_date() {
    let repo = merged_repo("date");
    let commits = GitCli::new(repo.path()).log(&LogOptions::default()).unwrap();
    // Like `7 Oct 2026 09:10`: day, month name, year, 24-hour time.
    let parts: Vec<&str> = commits[0].date.split_whitespace().collect();
    assert_eq!(parts.len(), 4, "got {:?}", commits[0].date);
    assert!(parts[0].parse::<u32>().is_ok());
    assert!(parts[2].parse::<u32>().is_ok_and(|year| year >= 2024));
    assert!(parts[3].contains(':'));
}

#[test]
fn current_branch_is_the_branch_head_is_on_and_none_when_detached() {
    let repo = merged_repo("branch");
    let git = GitCli::new(repo.path());
    assert_eq!(git.current_branch().unwrap().as_deref(), Some("main"));

    repo.git(&["checkout", "-q", "--detach"]);
    assert_eq!(git.current_branch().unwrap(), None);
}

#[test]
fn changed_files_counts_modified_staged_untracked_and_renamed_once() {
    let repo = merged_repo("status");
    let git = GitCli::new(repo.path());
    assert_eq!(git.changed_files().unwrap(), 0);

    std::fs::write(repo.path().join("a.txt"), "edited").unwrap(); // modified
    std::fs::write(repo.path().join("new.txt"), "x").unwrap(); // untracked
    std::fs::write(repo.path().join("staged.txt"), "x").unwrap();
    repo.git(&["add", "staged.txt"]); // staged
    assert_eq!(git.changed_files().unwrap(), 3);

    repo.git(&["mv", "b.txt", "renamed.txt"]); // a rename is one change, not two
    assert_eq!(git.changed_files().unwrap(), 4);
}

#[test]
fn reading_a_repo_does_not_take_the_index_lock() {
    let repo = merged_repo("lock");
    std::fs::write(repo.path().join("a.txt"), "edited").unwrap();
    let git = GitCli::new(repo.path());
    git.changed_files().unwrap();
    assert!(!repo.path().join(".git/index.lock").exists());
}

fn head_id(repo: &TempRepo) -> String {
    let out = Command::new("git").arg("-C").arg(repo.path()).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn find<'a>(changes: &'a [gitgui_core::FileChange], path: &str) -> &'a gitgui_core::FileChange {
    changes.iter().find(|c| c.path == path).unwrap_or_else(|| panic!("no change for {path} in {changes:?}"))
}

#[test]
fn commit_files_reports_status_counts_renames_and_binaries() {
    use gitgui_core::FileStatus::*;
    let repo = TempRepo::new("files");
    std::fs::write(repo.path().join("keep.txt"), "1\n2\n3\n").unwrap();
    std::fs::write(repo.path().join("gone.txt"), "bye\n").unwrap();
    std::fs::write(repo.path().join("old.txt"), "alpha\nbeta\ngamma\ndelta\nepsilon\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "base"]);

    std::fs::write(repo.path().join("keep.txt"), "1\ntwo\n3\n4\n").unwrap(); // +2 -1
    repo.git(&["rm", "-q", "gone.txt"]);
    repo.git(&["mv", "old.txt", "new.txt"]);
    std::fs::write(repo.path().join("added.txt"), "x\ny\n").unwrap();
    std::fs::write(repo.path().join("pic.bin"), [0u8, 159, 146, 150, 0, 1]).unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "changes"]);

    let git = GitCli::new(repo.path());
    let changes = git.commit_files(&head_id(&repo)).unwrap();
    assert_eq!(changes.len(), 5, "{changes:?}");

    let keep = find(&changes, "keep.txt");
    assert_eq!((keep.status, keep.additions, keep.deletions), (Modified, Some(2), Some(1)));
    let gone = find(&changes, "gone.txt");
    assert_eq!((gone.status, gone.additions, gone.deletions), (Deleted, Some(0), Some(1)));
    let renamed = find(&changes, "new.txt");
    assert_eq!((renamed.status, renamed.old_path.as_deref()), (Renamed, Some("old.txt")));
    let added = find(&changes, "added.txt");
    assert_eq!((added.status, added.additions), (Added, Some(2)));
    let binary = find(&changes, "pic.bin");
    assert_eq!((binary.additions, binary.deletions), (None, None));
}

#[test]
fn a_root_commit_adds_everything_and_a_merge_is_compared_with_its_first_parent() {
    use gitgui_core::FileStatus::Added;
    let repo = merged_repo("rootmerge");
    let git = GitCli::new(repo.path());
    let commits = git.log(&LogOptions::default()).unwrap();

    let root = commits.iter().find(|c| c.summary == "a").unwrap();
    let changes = git.commit_files(&root.id).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].path.as_str(), changes[0].status), ("a.txt", Added));

    // m merges `feature` (which added c.txt) into main (which added b.txt): against main, c.txt is new.
    let merge = commits.iter().find(|c| c.summary == "m").unwrap();
    let changes = git.commit_files(&merge.id).unwrap();
    assert_eq!(changes.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["c.txt"]);
}

#[test]
fn file_diff_returns_hunks_with_numbered_lines() {
    let repo = TempRepo::new("diff");
    std::fs::write(repo.path().join("f.txt"), "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "base"]);
    std::fs::write(repo.path().join("f.txt"), "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\neight\nNINE\nten\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "edit"]);

    let git = GitCli::new(repo.path());
    let id = head_id(&repo);
    let change = git.commit_files(&id).unwrap().remove(0);
    let diff = git.file_diff(&id, &change, 1).unwrap();

    assert!(!diff.binary);
    assert_eq!(diff.hunks.len(), 2, "with one line of context, edits six lines apart are separate hunks");
    let removed = &diff.hunks[0].lines.iter().find(|l| l.kind == gitgui_core::LineKind::Removed).unwrap();
    assert_eq!((removed.text.as_str(), removed.old_no, removed.new_no), ("three", Some(3), None));
    let added = &diff.hunks[1].lines.iter().find(|l| l.kind == gitgui_core::LineKind::Added).unwrap();
    assert_eq!((added.text.as_str(), added.old_no, added.new_no), ("NINE", None, Some(9)));
}

#[test]
fn file_diff_follows_a_rename_and_flags_a_binary_file() {
    let repo = TempRepo::new("diffrename");
    std::fs::write(repo.path().join("old.txt"), "a\nb\nc\nd\ne\nf\ng\nh\n").unwrap();
    std::fs::write(repo.path().join("pic.bin"), [0u8, 1, 2]).unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "base"]);
    repo.git(&["mv", "old.txt", "new.txt"]);
    std::fs::write(repo.path().join("new.txt"), "a\nb\nc\nd\ne\nf\ng\nH\n").unwrap();
    std::fs::write(repo.path().join("pic.bin"), [0u8, 9, 9, 9]).unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "rename and edit"]);

    let git = GitCli::new(repo.path());
    let id = head_id(&repo);
    let changes = git.commit_files(&id).unwrap();

    let renamed = find(&changes, "new.txt");
    let diff = git.file_diff(&id, renamed, 3).unwrap();
    assert_eq!(diff.hunks.len(), 1, "a rename with a small edit is one small diff, not a whole new file");
    assert_eq!(diff.line_count(), 3 + 1 + 1, "three lines of context, the removed line, and the added line");

    let binary = find(&changes, "pic.bin");
    assert!(git.file_diff(&id, binary, 3).unwrap().binary);
}

#[test]
fn commit_detail_has_the_whole_message_and_both_identities() {
    let repo = TempRepo::new("detail");
    repo.commit("a.txt", "subject\n\nbody line");
    let git = GitCli::new(repo.path());
    let detail = git.commit_detail(&head_id(&repo)).unwrap();
    assert_eq!(detail.author, "Ada");
    assert_eq!(detail.committer_email, "ada@example.com");
    assert!(detail.message.starts_with("subject"));
    assert!(detail.message.ends_with("body line"));
    assert!(detail.parents.is_empty());
    assert!(!detail.date.is_empty());
}

#[test]
fn an_option_shaped_revision_never_reaches_git() {
    let repo = TempRepo::new("inject");
    repo.commit("a.txt", "a");
    let git = GitCli::new(repo.path());
    assert!(matches!(git.commit_detail("--output=/tmp/gitgui-pwned"), Err(Error::Parse(_))));
    assert!(matches!(git.commit_files("-p"), Err(Error::Parse(_))));
    assert!(!Path::new("/tmp/gitgui-pwned").exists());
}

#[test]
fn toplevel_finds_the_repo_root_from_a_subfolder_and_refuses_a_plain_folder() {
    let repo = merged_repo("toplevel");
    std::fs::create_dir_all(repo.path().join("deep/er")).unwrap();
    let root = GitCli::new(repo.path().join("deep/er")).toplevel().unwrap();
    assert_eq!(std::fs::canonicalize(root).unwrap(), std::fs::canonicalize(repo.path()).unwrap());

    let plain = std::env::temp_dir().join(format!("gitgui-test-{}-plain", std::process::id()));
    std::fs::create_dir_all(&plain).unwrap();
    let result = GitCli::new(&plain).toplevel();
    let _ = std::fs::remove_dir_all(&plain);
    assert!(matches!(result, Err(Error::Git { .. })), "got {result:?}");
}

// ---- squash and other merges -------------------------------------------------------------------

fn rev(repo: &TempRepo, name: &str) -> String {
    let out = Command::new("git").arg("-C").arg(repo.path()).args(["rev-parse", name]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn tip(repo: &TempRepo, name: &str) -> BranchTip {
    BranchTip { name: name.to_owned(), id: rev(repo, name) }
}

fn write(repo: &TempRepo, file: &str, text: &str) {
    std::fs::write(repo.path().join(file), text).unwrap();
}

fn numbered(lines: usize) -> String {
    (1..=lines).map(|n| format!("line {n}\n")).collect()
}

/// `app.txt` has 20 numbered lines; `main` has one commit.
fn trunk() -> TempRepo {
    let repo = TempRepo::new(&format!("squash-{}", std::thread::current().name().unwrap_or("t").replace("::", "-")));
    write(&repo, "app.txt", &numbered(20));
    write(&repo, "other.txt", "other\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "base"]);
    repo
}

fn edit_line(repo: &TempRepo, n: usize, text: &str) {
    let content = std::fs::read_to_string(repo.path().join("app.txt")).unwrap();
    let edited: String = content.lines().enumerate().map(|(i, l)| format!("{}\n", if i + 1 == n { text } else { l })).collect();
    write(repo, "app.txt", &edited);
}

fn clues(repo: &TempRepo, branch: &str, targets: &[&str]) -> Vec<gitgui_core::MergeClue> {
    let targets: Vec<BranchTip> = targets.iter().map(|t| tip(repo, t)).collect();
    GitCli::new(repo.path()).merge_clues(&[tip(repo, branch)], &targets).unwrap().clues
}

#[test]
fn a_squash_merge_is_recognized_by_its_identical_changes() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    edit_line(&repo, 3, "THREE");
    repo.git(&["commit", "-q", "-am", "feat: change three"]);
    edit_line(&repo, 12, "TWELVE");
    repo.git(&["commit", "-q", "-am", "fix: change twelve"]);
    repo.git(&["checkout", "-q", "main"]);
    write(&repo, "other.txt", "trunk moved on\n");
    repo.git(&["commit", "-q", "-am", "chore: unrelated work"]);
    repo.git(&["merge", "-q", "--squash", "feature"]);
    repo.git(&["commit", "-q", "-m", "feat: change things (#7)"]);

    let found = clues(&repo, "feature", &["main"]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].evidence, Evidence::SamePatch);
    assert_eq!(found[0].commit.as_deref(), Some(rev(&repo, "main").as_str()), "points at the squash commit");
    assert_eq!((found[0].branch.as_str(), found[0].into.as_str(), found[0].pr), ("feature", "main", Some(7)));
    assert!(!found[0].evidence.is_probable());
}

#[test]
fn a_squash_after_the_trunk_edited_nearby_lines_is_found_by_the_messages_it_repeats() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    edit_line(&repo, 10, "TEN");
    repo.git(&["commit", "-q", "-am", "feat: edit line ten for the booking"]);
    write(&repo, "b.txt", "b\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "feat: add the b file for the booking"]);
    repo.git(&["checkout", "-q", "main"]);
    edit_line(&repo, 13, "THIRTEEN, edited on the trunk");
    repo.git(&["commit", "-q", "-am", "chore: trunk edits a nearby line"]);
    repo.git(&["merge", "-q", "--squash", "feature"]);
    repo.git(&[
        "commit", "-q", "-m", "feat: booking (#9)", "-m",
        "* feat: edit line ten for the booking\n* feat: add the b file for the booking",
    ]);

    let found = clues(&repo, "feature", &["main"]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].evidence, Evidence::Messages, "the patch no longer matches: its context changed");
    assert_eq!(found[0].pr, Some(9));
    assert!(found[0].evidence.is_probable());
}

#[test]
fn a_squash_is_found_by_the_pull_request_number_both_sides_name() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    edit_line(&repo, 10, "TEN");
    repo.git(&["commit", "-q", "-am", "fix: address PR #42 review comments"]);
    repo.git(&["checkout", "-q", "main"]);
    edit_line(&repo, 13, "THIRTEEN, edited on the trunk");
    repo.git(&["commit", "-q", "-am", "chore: trunk edits a nearby line"]);
    repo.git(&["merge", "-q", "--squash", "feature"]);
    repo.git(&["commit", "-q", "-m", "feat: a different title entirely (#42)"]);

    let found = clues(&repo, "feature", &["main"]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((found[0].evidence, found[0].pr), (Evidence::PullRequest, Some(42)));
}

#[test]
fn ordinary_and_fast_forward_merges_are_contained() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "merged"]);
    edit_line(&repo, 2, "TWO");
    repo.git(&["commit", "-q", "-am", "feat: two"]);
    repo.git(&["checkout", "-q", "main"]);
    write(&repo, "other.txt", "moved\n");
    repo.git(&["commit", "-q", "-am", "chore: move"]);
    repo.git(&["merge", "-q", "--no-ff", "-m", "Merge merged", "merged"]);

    // A branch fast-forwarded into the trunk, which then moved on.
    repo.git(&["checkout", "-q", "-b", "ff", "main"]);
    edit_line(&repo, 5, "FIVE");
    repo.git(&["commit", "-q", "-am", "feat: five"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["merge", "-q", "--ff-only", "ff"]);
    write(&repo, "other.txt", "after the fast-forward\n");
    repo.git(&["commit", "-q", "-am", "chore: after"]);

    let found = GitCli::new(repo.path())
        .merge_clues(&[tip(&repo, "merged"), tip(&repo, "ff")], &[tip(&repo, "main")])
        .unwrap()
        .clues;
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|c| c.evidence == Evidence::Contained && c.commit.is_none()));
}

#[test]
fn an_unmerged_branch_and_a_branch_on_the_trunk_tip_have_no_clue() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "wip"]);
    edit_line(&repo, 4, "FOUR");
    repo.git(&["commit", "-q", "-am", "wip: four"]);
    repo.git(&["checkout", "-q", "-b", "fresh", "main"]);
    repo.git(&["checkout", "-q", "main"]);
    write(&repo, "other.txt", "trunk\n");
    repo.git(&["commit", "-q", "-am", "chore: trunk"]);

    assert!(clues(&repo, "wip", &["main"]).is_empty());
    // A branch at the trunk's tip, or named like the target, is not "merged into" it.
    repo.git(&["branch", "same-as-main"]);
    assert!(clues(&repo, "same-as-main", &["main"]).is_empty());
    assert!(GitCli::new(repo.path()).merge_clues(&[tip(&repo, "main")], &[tip(&repo, "main")]).unwrap().clues.is_empty());
}

#[test]
fn the_first_target_that_contains_the_branch_wins_and_unrelated_histories_are_skipped() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "release"]);
    write(&repo, "other.txt", "release only\n");
    repo.git(&["commit", "-q", "-am", "chore: release-only change"]);
    repo.git(&["checkout", "-q", "-b", "feature", "main"]);
    edit_line(&repo, 3, "THREE");
    repo.git(&["commit", "-q", "-am", "feat: change three here"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["merge", "-q", "--squash", "feature"]);
    repo.git(&["commit", "-q", "-m", "feat: change three here (#3)"]);

    // `release` never got the change; `main` did. Targets are tried in order.
    let found = clues(&repo, "feature", &["release", "main"]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].into, "main");

    // A branch with no common history with the target is not an error, just no clue.
    repo.git(&["checkout", "-q", "--orphan", "island"]);
    repo.git(&["rm", "-rfq", "."]);
    write(&repo, "island.txt", "alone\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "island"]);
    assert!(clues(&repo, "island", &["main"]).is_empty());
}

#[test]
fn an_option_shaped_branch_id_never_reaches_git() {
    let repo = trunk();
    let bad = BranchTip { name: "x".into(), id: "--output=/tmp/gitgui-pwned2".into() };
    let result = GitCli::new(repo.path()).merge_clues(&[bad], &[tip(&repo, "main")]);
    assert!(matches!(result, Err(Error::Parse(_))));
    assert!(!Path::new("/tmp/gitgui-pwned2").exists());
}

// ---- changing the repository ---------------------------------------------------------------------

fn branch_exists(repo: &TempRepo, name: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")])
        .status()
        .unwrap()
        .success()
}

fn status_is_clean(repo: &TempRepo) -> bool {
    let out = Command::new("git").arg("-C").arg(repo.path()).args(["status", "--porcelain"]).output().unwrap();
    out.stdout.is_empty()
}

/// Two branches that both change line 5 of `app.txt`, so merging one into the other conflicts.
fn conflicting() -> TempRepo {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "other"]);
    edit_line(&repo, 5, "FIVE from other");
    repo.git(&["commit", "-q", "-am", "feat: five from other"]);
    repo.git(&["checkout", "-q", "main"]);
    edit_line(&repo, 5, "FIVE from main");
    repo.git(&["commit", "-q", "-am", "feat: five from main"]);
    repo
}

#[test]
fn checkout_switches_branches_detaches_and_follows_a_remote_branch() {
    let repo = trunk();
    repo.git(&["branch", "feature"]);
    let git = GitCli::new(repo.path());

    git.checkout(&CheckoutTarget::Branch("feature".into())).unwrap();
    assert_eq!(git.current_branch().unwrap().as_deref(), Some("feature"));

    git.checkout(&CheckoutTarget::Detached(rev(&repo, "main"))).unwrap();
    assert_eq!(git.current_branch().unwrap(), None);

    // A remote branch with no local copy becomes a local branch that tracks it.
    let remote = std::env::temp_dir().join(format!("gitgui-test-{}-remote-co", std::process::id()));
    let _ = std::fs::remove_dir_all(&remote);
    repo.git(&["init", "-q", "--bare", remote.to_str().unwrap()]);
    repo.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    repo.git(&["push", "-q", "origin", "feature"]);
    repo.git(&["fetch", "-q", "origin"]);
    repo.git(&["branch", "-q", "-D", "feature"]);
    git.checkout(&CheckoutTarget::RemoteBranch("origin/feature".into())).unwrap();
    assert_eq!(git.current_branch().unwrap().as_deref(), Some("feature"));
    let _ = std::fs::remove_dir_all(&remote);
}

#[test]
fn checkout_refuses_to_overwrite_local_changes_and_says_why() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    edit_line(&repo, 3, "THREE on feature");
    repo.git(&["commit", "-q", "-am", "feat: three"]);
    repo.git(&["checkout", "-q", "main"]);
    edit_line(&repo, 3, "uncommitted work on main");

    let result = GitCli::new(repo.path()).checkout(&CheckoutTarget::Branch("feature".into()));
    assert!(matches!(result, Err(Error::Git { .. })), "{result:?}");
    let text = std::fs::read_to_string(repo.path().join("app.txt")).unwrap();
    assert!(text.contains("uncommitted work on main"), "the local edit is still there");
}

#[test]
fn branches_can_be_created_renamed_and_deleted_and_bad_names_are_refused() {
    let repo = trunk();
    let git = GitCli::new(repo.path());

    git.create_branch("feat/new", None, false).unwrap();
    assert!(branch_exists(&repo, "feat/new"));
    git.create_branch("at-base", Some(&rev(&repo, "main")), true).unwrap();
    assert_eq!(git.current_branch().unwrap().as_deref(), Some("at-base"));

    git.rename_branch("feat/new", "feat/renamed").unwrap();
    assert!(branch_exists(&repo, "feat/renamed") && !branch_exists(&repo, "feat/new"));

    for bad in ["-D", "has space", "a..b", "ends.lock", "", "x~1"] {
        assert!(git.create_branch(bad, None, false).is_err(), "{bad:?} should be refused");
    }
    assert!(git.rename_branch("feat/renamed", "bad name").is_err());
    assert!(branch_exists(&repo, "feat/renamed"), "a refused rename changes nothing");

    git.checkout(&CheckoutTarget::Branch("main".into())).unwrap();
    git.delete_branch("feat/renamed", false).unwrap();
    assert!(!branch_exists(&repo, "feat/renamed"));
}

#[test]
fn an_unmerged_branch_needs_force_to_delete_and_the_current_branch_cannot_be_deleted() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "wip"]);
    edit_line(&repo, 4, "FOUR");
    repo.git(&["commit", "-q", "-am", "wip: four"]);
    repo.git(&["checkout", "-q", "main"]);
    let git = GitCli::new(repo.path());

    assert!(git.delete_branch("wip", false).is_err(), "git will not drop unmerged work unasked");
    assert!(branch_exists(&repo, "wip"));
    git.delete_branch("wip", true).unwrap();
    assert!(!branch_exists(&repo, "wip"));

    assert!(git.delete_branch("main", true).is_err(), "even forced, the branch you are on stays");
    assert!(branch_exists(&repo, "main"));
}

#[test]
fn a_squash_merged_branch_is_deleted_with_force_once_the_clue_says_it_is_safe() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    edit_line(&repo, 3, "THREE");
    repo.git(&["commit", "-q", "-am", "feat: change three"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["merge", "-q", "--squash", "feature"]);
    repo.git(&["commit", "-q", "-m", "feat: change three (#5)"]);

    let git = GitCli::new(repo.path());
    assert!(git.delete_branch("feature", false).is_err(), "git cannot see a squash merge");
    let found = git.merge_clues(&[tip(&repo, "feature")], &[tip(&repo, "main")]).unwrap().clues;
    assert_eq!(found.len(), 1);
    git.delete_branch("feature", true).unwrap();
}

#[test]
fn tags_are_made_at_a_commit_and_a_bad_tag_name_is_refused() {
    let repo = trunk();
    let git = GitCli::new(repo.path());
    git.create_tag("v1.0", &rev(&repo, "main")).unwrap();
    assert_eq!(rev(&repo, "v1.0^{commit}"), rev(&repo, "main"));
    assert!(git.create_tag("v1.0", &rev(&repo, "main")).is_err(), "an existing tag is not moved");
    assert!(git.create_tag("bad tag", &rev(&repo, "main")).is_err());
    assert!(git.create_tag("a..b", &rev(&repo, "main")).is_err());
}

#[test]
fn a_merge_that_can_proceed_does_and_a_conflicting_one_stops_and_can_be_aborted() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    write(&repo, "b.txt", "b\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "feat: add b"]);
    repo.git(&["checkout", "-q", "main"]);
    let git = GitCli::new(repo.path());
    assert!(matches!(git.merge("feature").unwrap(), Outcome::Done(_)));
    assert!(repo.path().join("b.txt").exists());

    let repo = conflicting();
    let git = GitCli::new(repo.path());
    let outcome = git.merge("other").unwrap();
    assert_eq!(outcome, Outcome::Conflicts { operation: Operation::Merge, files: 1 });
    assert_eq!(git.in_progress(), Some(Operation::Merge));
    assert!(!status_is_clean(&repo));

    git.abort(Operation::Merge).unwrap();
    assert_eq!(git.in_progress(), None);
    assert!(status_is_clean(&repo), "aborting puts everything back");
    assert!(std::fs::read_to_string(repo.path().join("app.txt")).unwrap().contains("FIVE from main"));
}

#[test]
fn a_rebase_moves_the_branch_and_a_conflicting_one_can_be_aborted() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    write(&repo, "f.txt", "f\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "feat: f"]);
    repo.git(&["checkout", "-q", "main"]);
    write(&repo, "other.txt", "trunk moved\n");
    repo.git(&["commit", "-q", "-am", "chore: trunk"]);
    repo.git(&["checkout", "-q", "feature"]);
    let git = GitCli::new(repo.path());
    assert!(matches!(git.rebase("main").unwrap(), Outcome::Done(_)));
    assert_eq!(rev(&repo, "feature~1"), rev(&repo, "main"), "feature now sits on the trunk's tip");

    let repo = conflicting();
    repo.git(&["checkout", "-q", "other"]);
    let before = rev(&repo, "other");
    let git = GitCli::new(repo.path());
    assert_eq!(git.rebase("main").unwrap(), Outcome::Conflicts { operation: Operation::Rebase, files: 1 });
    assert_eq!(git.in_progress(), Some(Operation::Rebase));
    git.abort(Operation::Rebase).unwrap();
    assert_eq!(git.in_progress(), None);
    assert_eq!(rev(&repo, "other"), before, "the branch is exactly where it was");
}

#[test]
fn cherry_pick_applies_a_commit_stops_on_conflict_and_refuses_a_merge_commit() {
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "feature"]);
    write(&repo, "c.txt", "c\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "feat: c"]);
    let pick = rev(&repo, "feature");
    repo.git(&["checkout", "-q", "main"]);
    let git = GitCli::new(repo.path());
    assert!(matches!(git.cherry_pick(&pick).unwrap(), Outcome::Done(_)));
    assert!(repo.path().join("c.txt").exists());

    let repo = conflicting();
    let pick = rev(&repo, "other");
    let git = GitCli::new(repo.path());
    assert_eq!(git.cherry_pick(&pick).unwrap(), Outcome::Conflicts { operation: Operation::CherryPick, files: 1 });
    git.abort(Operation::CherryPick).unwrap();
    assert!(status_is_clean(&repo));

}

#[test]
fn a_merge_commit_cannot_be_cherry_picked() {
    let repo = merged_repo("pickmerge");
    let merge = rev(&repo, "main");
    repo.git(&["checkout", "-q", "-b", "elsewhere", &format!("{merge}~1")]);
    let result = GitCli::new(repo.path()).cherry_pick(&merge);
    assert!(matches!(result, Err(Error::Parse(_))), "{result:?}");
}

#[test]
fn push_sets_an_upstream_the_first_time_pushes_new_commits_and_never_forces() {
    let repo = trunk();
    let remote = std::env::temp_dir().join(format!("gitgui-test-{}-remote-push", std::process::id()));
    let _ = std::fs::remove_dir_all(&remote);
    repo.git(&["init", "-q", "--bare", remote.to_str().unwrap()]);
    let git = GitCli::new(repo.path());

    // No remote yet: a clear error, nothing pushed.
    assert!(matches!(git.push_branch("main"), Err(Error::Parse(_))));

    repo.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    git.push_branch("main").unwrap();
    let upstream = Command::new("git").arg("-C").arg(repo.path()).args(["config", "branch.main.remote"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&upstream.stdout).trim(), "origin", "the first push set the upstream");

    write(&repo, "other.txt", "more\n");
    repo.git(&["commit", "-q", "-am", "chore: more"]);
    git.push_branch("main").unwrap();

    // Our history now diverges from the remote's: we drop its last commit and make a different one.
    repo.git(&["reset", "-q", "--hard", "HEAD~1"]);
    write(&repo, "mine.txt", "mine\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "mine"]);
    let result = git.push_branch("main");
    assert!(matches!(result, Err(Error::Git { .. })), "the push is rejected, not forced: {result:?}");
    let remote_head = Command::new("git").arg("-C").arg(&remote).args(["log", "-1", "--format=%s", "main"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&remote_head.stdout).trim(), "chore: more", "the remote's history was not overwritten");

    let _ = std::fs::remove_dir_all(&remote);
}

/// Another person's clone of `remote`, in its own folder, ready to commit and push from.
fn clone_of(remote: &Path, name: &str) -> TempRepo {
    let dir = std::env::temp_dir().join(format!("gitgui-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let status = Command::new("git")
        .args(["clone", "-q", remote.to_str().unwrap(), dir.to_str().unwrap()])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    TempRepo(dir)
}

#[test]
fn pull_rebase_replays_local_commits_on_the_remotes_and_stops_cleanly_on_conflicts() {
    let repo = trunk();
    let remote = std::env::temp_dir().join(format!("gitgui-test-{}-remote-pull", std::process::id()));
    let _ = std::fs::remove_dir_all(&remote);
    repo.git(&["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    repo.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    let git = GitCli::new(repo.path());
    git.push_branch("main").unwrap();

    // A colleague pushes a commit; we also made one that is not pushed.
    let other = clone_of(&remote, "pull-other");
    other.commit("theirs.txt", "theirs");
    other.git(&["push", "-q", "origin", "main"]);
    repo.commit("mine.txt", "mine");

    let outcome = git.pull_rebase().unwrap();
    assert!(matches!(outcome, Outcome::Done(_)), "{outcome:?}");
    let log = Command::new("git").arg("-C").arg(repo.path()).args(["log", "--format=%s", "-3"]).output().unwrap();
    let log = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(log.starts_with("mine\ntheirs\n"), "our commit is on top of theirs: {log:?}");
    let parents = Command::new("git").arg("-C").arg(repo.path()).args(["rev-list", "--merges", "--count", "HEAD"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&parents.stdout).trim(), "0", "no merge commit was made");

    // Both sides change the same file: the rebase stops, and abort puts it back.
    other.commit("clash.txt", "their version");
    other.git(&["push", "-q", "origin", "main"]);
    repo.commit("clash.txt", "my version");
    let before = Command::new("git").arg("-C").arg(repo.path()).args(["rev-parse", "HEAD"]).output().unwrap().stdout;
    let outcome = git.pull_rebase().unwrap();
    assert!(matches!(outcome, Outcome::Conflicts { operation: Operation::Rebase, files: 1 }), "{outcome:?}");
    git.abort(Operation::Rebase).unwrap();
    let after = Command::new("git").arg("-C").arg(repo.path()).args(["rev-parse", "HEAD"]).output().unwrap().stdout;
    assert_eq!(before, after, "abort restored our commit");

    // Uncommitted changes: git refuses and says why; nothing is lost.
    std::fs::write(repo.path().join("mine.txt"), "edited").unwrap();
    let result = git.pull_rebase();
    assert!(matches!(result, Err(Error::Git { .. })) || matches!(result, Ok(Outcome::Done(_))), "{result:?}");
    assert_eq!(std::fs::read_to_string(repo.path().join("mine.txt")).unwrap(), "edited");

    let _ = std::fs::remove_dir_all(&remote);
}

#[test]
fn option_shaped_names_never_reach_git_for_any_operation() {
    let repo = trunk();
    let git = GitCli::new(repo.path());
    assert!(matches!(git.checkout(&CheckoutTarget::Branch("--orphan=x".into())), Err(Error::Parse(_))));
    assert!(matches!(git.merge("--abort"), Err(Error::Parse(_))));
    assert!(matches!(git.rebase("-i"), Err(Error::Parse(_))));
    assert!(matches!(git.cherry_pick("--quit"), Err(Error::Parse(_))));
    assert!(matches!(git.push_branch("--force"), Err(Error::Parse(_))));
    assert!(matches!(git.delete_branch("-r", true), Err(Error::Parse(_))));
    assert!(matches!(git.create_tag("-d", "HEAD"), Err(Error::Parse(_))));
}

#[test]
fn many_branches_are_checked_together_and_each_gets_its_own_answer() {
    let repo = trunk();
    // Six squash-merged branches, each touching its own file, and four that were never merged.
    for n in 0..6 {
        repo.git(&["checkout", "-q", "-b", &format!("done-{n}"), "main"]);
        write(&repo, &format!("done{n}.txt"), &format!("{n}\n"));
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", &format!("feat: finished piece {n} of the work")]);
        repo.git(&["checkout", "-q", "main"]);
        repo.git(&["merge", "-q", "--squash", &format!("done-{n}")]);
        repo.git(&["commit", "-q", "-m", &format!("feat: piece {n} (#{})", 100 + n)]);
    }
    for n in 0..4 {
        repo.git(&["checkout", "-q", "-b", &format!("open-{n}"), "main"]);
        write(&repo, &format!("open{n}.txt"), "x\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", &format!("wip: open work {n} that nobody merged")]);
        repo.git(&["checkout", "-q", "main"]);
    }
    let branches: Vec<BranchTip> =
        (0..6).map(|n| format!("done-{n}")).chain((0..4).map(|n| format!("open-{n}"))).map(|name| tip(&repo, &name)).collect();
    let scan = GitCli::new(repo.path()).merge_clues(&branches, &[tip(&repo, "main")]).unwrap();

    assert_eq!(scan.unchecked, 0);
    assert_eq!(scan.clues.len(), 6, "{:?}", scan.clues);
    // In the order the branches were given, each with the squash commit that carries it.
    for (n, clue) in scan.clues.iter().enumerate() {
        assert_eq!(clue.branch, format!("done-{n}"));
        assert_eq!((clue.evidence, clue.pr), (Evidence::SamePatch, Some(100 + n as u32)));
    }
}

/// Three squash-merged branches and two open ones, all off `main`.
fn squashed_and_open() -> (TempRepo, Vec<String>) {
    let repo = trunk();
    for n in 0..3 {
        repo.git(&["checkout", "-q", "-b", &format!("done-{n}"), "main"]);
        write(&repo, &format!("done{n}.txt"), &format!("{n}\n"));
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", &format!("feat: finished piece {n} of the work")]);
        repo.git(&["checkout", "-q", "main"]);
        repo.git(&["merge", "-q", "--squash", &format!("done-{n}")]);
        repo.git(&["commit", "-q", "-m", &format!("feat: piece {n} (#{})", 100 + n)]);
    }
    for n in 0..2 {
        repo.git(&["checkout", "-q", "-b", &format!("open-{n}"), "main"]);
        write(&repo, &format!("open{n}.txt"), "x\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", &format!("wip: open work {n} that nobody merged")]);
        repo.git(&["checkout", "-q", "main"]);
    }
    let names = (0..3).map(|n| format!("done-{n}")).chain((0..2).map(|n| format!("open-{n}"))).collect();
    (repo, names)
}

#[test]
fn a_scan_of_branches_that_have_not_moved_asks_git_nothing_and_a_moved_branch_is_checked_alone() {
    use std::sync::atomic::AtomicBool;
    let (repo, names) = squashed_and_open();
    let git = GitCli::new(repo.path());
    let tips = |repo: &TempRepo| -> Vec<BranchTip> { names.iter().map(|name| tip(repo, name)).collect() };
    let targets = [tip(&repo, "main"), BranchTip { name: "origin/main".into(), id: rev(&repo, "main") }];
    let cache = gitgui_core::ScanCache::default();
    let go = AtomicBool::new(false);
    assert!(cache.lookup(&tips(&repo), &targets).is_none(), "nothing is known before the first scan");

    let first = git.merge_clues_cached(&tips(&repo), &targets, &cache, &go).unwrap();
    assert_eq!(first, git.merge_clues(&tips(&repo), &targets).unwrap(), "the same answer as without a cache");
    assert_eq!(first.clues.len(), 3);
    assert_eq!((cache.pairs_checked(), cache.targets_scanned()), (5, 1), "each branch once, against one trunk (two names)");

    // Refreshing with nothing moved: the answer is known without running git at all.
    for _ in 0..50 {
        assert_eq!(cache.lookup(&tips(&repo), &targets).as_ref(), Some(&first));
        assert_eq!(git.merge_clues_cached(&tips(&repo), &targets, &cache, &go).unwrap(), first);
    }
    assert_eq!((cache.pairs_checked(), cache.targets_scanned()), (5, 1), "50 more scans asked git nothing");
    assert_eq!(cache.len(), 5, "one answer per branch is kept, however often it scans");

    // One branch moves: only it is checked again, and the trunk is not read again.
    repo.git(&["checkout", "-q", "open-0"]);
    write(&repo, "open0.txt", "more\n");
    repo.git(&["commit", "-q", "-am", "wip: more open work on the first one"]);
    repo.git(&["checkout", "-q", "main"]);
    assert!(cache.lookup(&tips(&repo), &targets).is_none());
    let again = git.merge_clues_cached(&tips(&repo), &targets, &cache, &go).unwrap();
    assert_eq!(again.clues.len(), 3);
    assert_eq!((cache.pairs_checked(), cache.targets_scanned()), (6, 1));
    assert_eq!(cache.len(), 5, "the moved branch's old answer was dropped");
}

#[test]
fn a_cancelled_scan_stops_without_asking_git_and_says_what_it_left_unchecked() {
    use std::sync::atomic::AtomicBool;
    let (repo, names) = squashed_and_open();
    let branches: Vec<BranchTip> = names.iter().map(|name| tip(&repo, name)).collect();
    let cache = gitgui_core::ScanCache::default();
    let scan = GitCli::new(repo.path()).merge_clues_cached(&branches, &[tip(&repo, "main")], &cache, &AtomicBool::new(true)).unwrap();
    assert_eq!((scan.clues.len(), scan.unchecked), (0, 5));
    assert_eq!(cache.pairs_checked(), 0);
    assert!(cache.lookup(&branches, &[tip(&repo, "main")]).is_none(), "nothing unchecked is taken as an answer");
}

#[test]
fn reading_the_log_again_parses_nothing_when_git_prints_the_same() {
    let (repo, _) = squashed_and_open();
    let git = GitCli::new(repo.path());
    let options = LogOptions { max_count: Some(20_000), skip: 0 };
    let (first, commits) = git.log_if_changed(&options, None).unwrap();
    assert_eq!(commits.as_deref(), Some(git.log(&options).unwrap().as_slice()));
    assert_eq!(git.log_if_changed(&options, Some(first)).unwrap(), (first, None), "nothing moved: nothing parsed");

    // Anything that changes what the graph shows changes the fingerprint: a commit, a branch, a stash.
    repo.git(&["branch", "another"]);
    let (second, commits) = git.log_if_changed(&options, Some(first)).unwrap();
    assert_ne!(second, first);
    assert!(commits.unwrap().iter().any(|c| c.refs.iter().any(|r| r.name == "another")));
    write(&repo, "app.txt", "stashed\n");
    repo.git(&["stash", "-q"]);
    let (third, commits) = git.log_if_changed(&options, Some(second)).unwrap();
    assert_ne!(third, second);
    assert!(commits.unwrap().iter().any(|c| c.stash.is_some()));
}

#[test]
fn a_squash_of_a_very_large_change_is_still_recognized() {
    // The diffs are streamed from one git process into the next; they never sit in memory here.
    let repo = trunk();
    repo.git(&["checkout", "-q", "-b", "big"]);
    write(&repo, "big.txt", &numbered(200_000));
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "chore: add a very large generated file"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["merge", "-q", "--squash", "big"]);
    repo.git(&["commit", "-q", "-m", "chore: the large file (#9)"]);
    let found = clues(&repo, "big", &["main"]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((found[0].evidence, found[0].pr), (Evidence::SamePatch, Some(9)));
}

#[test]
fn the_working_tree_stages_unstages_diffs_and_commits() {
    let repo = merged_repo("worktree");
    let git = GitCli::new(repo.path());
    std::fs::write(repo.path().join("a.txt"), "a\nmore\n").unwrap();
    std::fs::write(repo.path().join("new.txt"), "one\ntwo\n").unwrap();
    std::fs::remove_file(repo.path().join("c.txt")).unwrap();

    let show = |git: &GitCli| -> Vec<(String, bool, bool)> {
        git.work_status().unwrap().into_iter().map(|f| (f.change.path, f.staged, f.untracked)).collect()
    };
    assert_eq!(
        show(&git),
        [("a.txt".into(), false, false), ("c.txt".into(), false, false), ("new.txt".into(), false, true)]
    );
    let files = git.work_status().unwrap();
    assert_eq!((files[2].change.additions, files[2].change.deletions), (Some(2), Some(0)), "a new file's lines");

    // A new file diffs against nothing; an edit against the index.
    let new = git.work_diff(&files[2], 3).unwrap();
    assert_eq!(new.hunks[0].lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["one", "two"]);
    let edit = git.work_diff(&files[0], 3).unwrap();
    assert!(edit.hunks[0].lines.iter().any(|l| l.text == "more"));
    assert_eq!(git.work_sides(&files[0]), (Some(b"a".to_vec()), Some(b"a\nmore\n".to_vec())));

    git.stage(&["a.txt".into(), "c.txt".into(), "new.txt".into()]).unwrap();
    assert_eq!(
        show(&git),
        [("a.txt".into(), true, false), ("c.txt".into(), true, false), ("new.txt".into(), true, false)]
    );
    let staged = git.work_status().unwrap();
    assert!(git.work_diff(&staged[0], 3).unwrap().hunks[0].lines.iter().any(|l| l.text == "more"));
    assert_eq!(git.work_sides(&staged[1]).1, None, "a staged deletion has no after");

    git.unstage(&["new.txt".into()]).unwrap();
    assert_eq!(show(&git)[2], ("new.txt".into(), false, true));

    assert!(git.commit_staged("  ").is_err(), "a message is needed");
    git.commit_staged("chore: tidy").unwrap();
    assert_eq!(show(&git), [("new.txt".into(), false, true)], "only what was not staged is left");
    let log = git.log(&LogOptions::default()).unwrap();
    assert_eq!(log[0].summary, "chore: tidy");
}

#[test]
fn unstaging_works_before_the_first_commit() {
    let repo = TempRepo::new("worktree-empty");
    let git = GitCli::new(repo.path());
    std::fs::write(repo.path().join("first.txt"), "hi\n").unwrap();
    git.stage(&["first.txt".into()]).unwrap();
    assert!(git.work_status().unwrap()[0].staged);
    git.unstage(&["first.txt".into()]).unwrap();
    assert!(git.work_status().unwrap()[0].untracked);
}
