use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::process::{Command as Process, Stdio};

use crate::diff::{self, FileDiff};
use crate::filter::Query;
use crate::model::{Commit, CommitDetail, FileChange, FileStatus, Ref, RefKind};
use crate::squash::{BranchTip, Evidence, MergeClue, MergeScan, repeats_subjects, shared_pr, subject_pr};

#[derive(Debug)]
pub enum Error {
    /// `git` could not be started.
    Spawn(std::io::Error),
    /// `git` ran and failed; carries its stderr.
    Git { status: Option<i32>, stderr: String },
    /// Output that did not have the shape we asked for.
    Parse(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Spawn(err) => write!(f, "could not run git: {err}"),
            Error::Git { status, stderr } => match status {
                Some(code) => write!(f, "git exited with {code}: {}", stderr.trim()),
                None => write!(f, "git was killed: {}", stderr.trim()),
            },
            Error::Parse(what) => write!(f, "unexpected git output: {what}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug, Default)]
pub struct LogOptions {
    /// Stop after this many commits.
    pub max_count: Option<usize>,
    /// Skip this many commits first, for paging.
    pub skip: usize,
}

/// What the UI needs from a repository. Reads and writes go through this trait, so the
/// implementation can move from the `git` binary to gitoxide piece by piece.
pub trait Backend {
    /// Commits reachable from HEAD, branches, remote branches, tags and stashes, newest first,
    /// parents after children. Each stash is one commit; its internal index commits are left out.
    fn log(&self, options: &LogOptions) -> Result<Vec<Commit>, Error>;

    /// The branch HEAD is on, or `None` when HEAD is detached or has no commits yet.
    fn current_branch(&self) -> Result<Option<String>, Error>;

    /// How many files have uncommitted changes, untracked files included.
    fn changed_files(&self) -> Result<usize, Error>;

    /// The full record of one commit.
    fn commit_detail(&self, id: &str) -> Result<CommitDetail, Error>;

    /// Files a commit changed against its first parent (against nothing, for a root commit).
    fn commit_files(&self, id: &str) -> Result<Vec<FileChange>, Error>;

    /// What a commit did to one file, with `context` unchanged lines around each hunk.
    fn file_diff(&self, id: &str, file: &FileChange, context: u32) -> Result<FileDiff, Error>;

    /// Which of `branches` have already been merged into one of `targets` (tried in order), including
    /// by a squash or rebase merge that leaves no link in the history. Branches with no sign of a
    /// merge are left out. Gives up after a few seconds and returns what it has found.
    fn merge_clues(&self, branches: &[BranchTip], targets: &[BranchTip]) -> Result<MergeScan, Error>;
}

/// Backend that runs the user's own `git`, so their config, hooks and credential helpers apply.
pub struct GitCli {
    root: PathBuf,
    /// Run git at a lower CPU priority, for work nobody is waiting on (the merge scan).
    low_priority: bool,
}

impl GitCli {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), low_priority: false }
    }

    /// The same repository, with git run at a lower CPU priority so background work gives way to
    /// the window and to the user's own programs.
    pub fn in_background(&self) -> Self {
        Self { root: self.root.clone(), low_priority: true }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The top folder of the repository containing this path, or an error when it is not inside one.
    pub fn toplevel(&self) -> Result<PathBuf, Error> {
        let out = self.run(&["rev-parse", "--show-toplevel"])?;
        let path = String::from_utf8_lossy(&out).trim_end_matches(['\n', '\r']).to_owned();
        if path.is_empty() {
            return Err(Error::Parse("empty repository path".into()));
        }
        Ok(PathBuf::from(path))
    }

    pub(crate) fn run(&self, args: &[&str]) -> Result<Vec<u8>, Error> {
        let output = self.command(args).output().map_err(Error::Spawn)?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(Error::Git {
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        }
    }

    pub(crate) fn command(&self, args: &[&str]) -> Process {
        let mut process = Process::new("git");
        // A viewer must not take `index.lock`; it would make the user's own `git add` fail.
        process.arg("--no-optional-locks").arg("-C").arg(&self.root).args(args);
        if self.low_priority {
            lower_priority(&mut process);
        }
        process
    }

    /// Runs `first`, feeds its output straight into `second`, and returns what `second` printed. The
    /// output of `first` never passes through this process, so a huge `git log -p` costs no memory here.
    fn pipeline(&self, first: &[&str], second: &[&str]) -> Result<Vec<u8>, Error> {
        let mut producer = self.command(first);
        producer.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut producer = producer.spawn().map_err(Error::Spawn)?;
        let between = producer.stdout.take().expect("stdout was piped");
        let mut consumer = self.command(second);
        consumer.stdin(Stdio::from(between)).stdout(Stdio::piped()).stderr(Stdio::piped());
        let consumer = match consumer.spawn() {
            Ok(consumer) => consumer,
            Err(err) => {
                let _ = producer.kill();
                let _ = producer.wait();
                return Err(Error::Spawn(err));
            }
        };
        let output = consumer.wait_with_output().map_err(Error::Spawn)?;
        let status = producer.wait().map_err(Error::Spawn)?;
        if !status.success() {
            return Err(Error::Git { status: status.code(), stderr: String::new() });
        }
        if !output.status.success() {
            return Err(Error::Git { status: output.status.code(), stderr: String::from_utf8_lossy(&output.stderr).into_owned() });
        }
        Ok(output.stdout)
    }

    /// Where `origin` points, else the first remote; `None` with no remotes.
    pub fn remote_url(&self) -> Result<Option<String>, Error> {
        let names = String::from_utf8_lossy(&self.run(&["remote"])?).lines().map(str::to_owned).collect::<Vec<_>>();
        let Some(name) = names.iter().find(|n| *n == "origin").or(names.first()) else { return Ok(None) };
        let url = String::from_utf8_lossy(&self.run(&["remote", "get-url", name])?).trim().to_owned();
        Ok((!url.is_empty()).then_some(url))
    }

    /// The text of `path` as it was at `rev` (`abc123`, `abc123^1`); `None` when the file is not
    /// there or is not UTF-8 text.
    pub fn file_at(&self, rev: &str, path: &str) -> Result<Option<String>, Error> {
        Ok(self.file_bytes_at(rev, path)?.and_then(|bytes| String::from_utf8(bytes).ok()))
    }

    /// The bytes of `path` as it was at `rev`; `None` when the file is not there.
    pub fn file_bytes_at(&self, rev: &str, path: &str) -> Result<Option<Vec<u8>>, Error> {
        let spec = format!("{rev}:{path}");
        match self.run(&["cat-file", "blob", &spec]) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Error::Git { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Ids of the commits a `path:` or `code:` query finds, across every branch, remote and tag the
    /// graph shows. Other queries find nothing here; they are matched against the loaded commits.
    pub fn search(&self, query: &Query) -> Result<HashSet<String>, Error> {
        let pickaxe;
        let mut args: Vec<&str> = vec!["log", "--format=%H", "--branches", "--remotes", "--tags"];
        if self.has_head() {
            args.push("HEAD");
        }
        match query {
            Query::Path(path) => args.extend(["--", path.as_str()]),
            Query::Code(code) => {
                pickaxe = format!("-S{code}");
                args.insert(1, &pickaxe);
            }
            Query::None | Query::Text(_) => return Ok(HashSet::new()),
        }
        let out = self.run(&args)?;
        Ok(String::from_utf8_lossy(&out).lines().map(str::to_owned).collect())
    }

    /// `stash@{n}` commit ids, newest first.
    fn stash_ids(&self) -> Result<Vec<String>, Error> {
        let out = self.run(&["stash", "list", "--format=%H"])?;
        Ok(String::from_utf8_lossy(&out).lines().map(str::to_owned).collect())
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        self.command(&["merge-base", "--is-ancestor", ancestor, descendant])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn merge_base(&self, a: &str, b: &str) -> Option<String> {
        let out = self.run(&["merge-base", a, b]).ok()?;
        let id = String::from_utf8_lossy(&out).trim().to_owned();
        (!id.is_empty()).then_some(id)
    }

    /// `(id, subject, body)` of up to `max` commits in `range`, newest first.
    fn messages(&self, range: &str, max: usize) -> Option<Vec<(String, String, String)>> {
        let limit = format!("--max-count={max}");
        let out = self.run(&["log", "--format=%H%x1f%s%x1f%b%x1e", &limit, range]).ok()?;
        let text = String::from_utf8_lossy(&out);
        Some(
            text.split(RECORD_END)
                .map(|record| record.trim_start_matches('\n'))
                .filter(|record| !record.trim().is_empty())
                .filter_map(|record| {
                    let mut fields = record.splitn(3, FIELD);
                    Some((fields.next()?.to_owned(), fields.next()?.to_owned(), fields.next().unwrap_or("").to_owned()))
                })
                .collect(),
        )
    }

    /// The patch id of everything `tip` changed since `base`, or `None` when it changed nothing.
    fn branch_patch_id(&self, base: &str, tip: &str) -> Option<String> {
        // An empty diff gives no patch id at all.
        let out = self.pipeline(&["diff", "--no-color", "--no-ext-diff", base, tip], &["patch-id", "--stable"]).ok()?;
        String::from_utf8_lossy(&out).split_whitespace().next().map(str::to_owned)
    }

    /// The patch id of each of the target's newest commits. Done once per target, not once per branch.
    fn scan_target(&self, target: &str) -> Option<TargetScan> {
        let limit = format!("--max-count={SCAN_COMMITS}");
        let out = self
            .pipeline(
                &["log", "-p", "--no-merges", "--no-color", "--no-ext-diff", "--no-decorate", &limit, target],
                &["patch-id", "--stable"],
            )
            .ok()?;
        let patches = String::from_utf8_lossy(&out)
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                Some((parts.next()?.to_owned(), parts.next()?.to_owned()))
            })
            .collect();
        Some(TargetScan { patches })
    }

    fn squash_clue(&self, branch: &BranchTip, target: &BranchTip, base: &str, scan: Option<&TargetScan>) -> Option<MergeClue> {
        let own = self.messages(&format!("{base}..{}", branch.id), 200)?;
        if own.is_empty() {
            return None;
        }
        let clue = |evidence: Evidence, commit: &str, subject: Option<&str>, pr: Option<u32>| MergeClue {
            branch: branch.name.clone(),
            into: target.name.clone(),
            commit: Some(commit.to_owned()),
            evidence,
            pr: pr.or_else(|| subject.and_then(subject_pr)),
        };

        // 1. The same changes, all in one commit that came after the branch left the trunk.
        if let (Some(scan), Some(id)) = (scan, self.branch_patch_id(base, &branch.id))
            && let Some(commit) = scan.patches.get(&id)
            && !self.is_ancestor(commit, base)
        {
            let subject = self.messages(&format!("{commit}^!"), 1).and_then(|m| m.into_iter().next()).map(|(_, s, _)| s);
            return Some(clue(Evidence::SamePatch, commit, subject.as_deref(), None));
        }

        // What the trunk did since the fork, for the two guesses from messages.
        let theirs = self.messages(&format!("{base}..{}", target.id), 300)?;
        // 2. A commit that names the pull request the branch's commits name.
        let texts: Vec<String> = own.iter().map(|(_, subject, body)| format!("{subject}\n{body}")).collect();
        if let Some((id, pr)) = theirs.iter().find_map(|(id, subject, _)| Some((id, shared_pr(&texts, subject)?))) {
            return Some(clue(Evidence::PullRequest, id, None, Some(pr)));
        }
        // 3. A commit whose message repeats the branch's commit messages.
        let subjects: Vec<String> = own.iter().map(|(_, subject, _)| subject.clone()).collect();
        let found = theirs.iter().find(|(_, subject, body)| repeats_subjects(&subjects, &format!("{subject}\n{body}")));
        found.map(|(id, subject, _)| clue(Evidence::Messages, id, Some(subject), None))
    }

    /// What a commit is compared against: its first parent, or the empty tree for a root commit.
    fn base_of(&self, id: &str) -> Result<String, Error> {
        check_rev(id)?;
        let first_parent = format!("{id}^1");
        let output = self
            .command(&["rev-parse", "--verify", "--quiet", &first_parent])
            .output()
            .map_err(Error::Spawn)?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
        }
        // The empty tree's id depends on the repo's hash algorithm, so ask git for it.
        let mut empty = self.command(&["hash-object", "-t", "tree", "--stdin"]);
        empty.stdin(Stdio::null());
        let output = empty.output().map_err(Error::Spawn)?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// The stashes, and what `git log` prints for the graph.
    fn log_output(&self, options: &LogOptions) -> Result<(Vec<String>, Vec<u8>), Error> {
        // Asking for the stashes first also fails fast when the folder is not a repository.
        let stashes = self.stash_ids()?;

        let max = options.max_count.map(|max| format!("--max-count={max}"));
        let skip = (options.skip > 0).then(|| format!("--skip={}", options.skip));
        let mut args: Vec<&str> = vec!["log", "--date-order", "--decorate=full", DATE_FORMAT, LOG_FORMAT];
        args.extend(max.as_deref());
        args.extend(skip.as_deref());
        // Explicit tips, not `--all`: that would also walk `refs/stash`, notes, and any `refs/pull/*`.
        args.extend(["--branches", "--remotes", "--tags"]);
        if self.has_head() {
            args.push("HEAD");
        }
        args.extend(stashes.iter().map(String::as_str));
        let stdout = self.run(&args)?;
        Ok((stashes, stdout))
    }

    /// [`Backend::log`], with a fingerprint of what git printed. When it is `unchanged` (the fingerprint
    /// of an earlier read), the commits are not parsed again and `None` comes back in their place:
    /// the commits read then are still exactly right. Parsing tens of thousands of commits makes
    /// hundreds of thousands of small allocations, so a refresh that finds nothing new skips that.
    pub fn log_if_changed(&self, options: &LogOptions, unchanged: Option<u64>) -> Result<(u64, Option<Vec<Commit>>), Error> {
        use std::hash::{Hash, Hasher};
        let (stashes, stdout) = self.log_output(options)?;
        let mut hasher = std::hash::DefaultHasher::new();
        (&stashes, &stdout).hash(&mut hasher);
        let fingerprint = hasher.finish();
        if unchanged == Some(fingerprint) {
            return Ok((fingerprint, None));
        }
        Ok((fingerprint, Some(commits_of(&stashes, &stdout)?)))
    }

    pub(crate) fn has_head(&self) -> bool {
        self.command(&["rev-parse", "--verify", "--quiet", "HEAD"])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// [`Backend::merge_clues`], keeping what it learns in `cache` and asking git only about the branch
    /// and trunk pairs the cache has no answer for. Git runs at a lower priority, on at most half the
    /// cores. Setting `cancel` stops it soon after (it finishes the git command it is in); what it has
    /// worked out by then is kept in the cache, so the scan that replaces it does not repeat it.
    pub fn merge_clues_cached(
        &self,
        branches: &[BranchTip],
        targets: &[BranchTip],
        cache: &ScanCache,
        cancel: &AtomicBool,
    ) -> Result<MergeScan, Error> {
        for tip in branches.iter().chain(targets) {
            check_rev(&tip.id)?;
        }
        let git = self.in_background();
        let targets = unique_targets(targets);
        let deadline = Instant::now() + MERGE_SCAN_BUDGET;
        let next = AtomicUsize::new(0);
        let found: Mutex<Vec<(usize, MergeClue)>> = Mutex::new(Vec::new());
        let unchecked = AtomicUsize::new(0);

        let check_pair = |branch: &BranchTip, target: &BranchTip| -> Option<MergeClue> {
            if git.is_ancestor(&branch.id, &target.id) {
                return Some(MergeClue {
                    branch: branch.name.clone(),
                    into: target.name.clone(),
                    commit: None,
                    evidence: Evidence::Contained,
                    pr: None,
                });
            }
            let base = git.merge_base(&branch.id, &target.id)?;
            let slot = cache.target_slot(&target.id);
            let scan = slot.get_or_init(|| {
                cache.targets_scanned.fetch_add(1, Ordering::Relaxed);
                git.scan_target(&target.id).map(Arc::new)
            });
            git.squash_clue(branch, target, &base, scan.as_deref())
        };
        let check = |branch: &BranchTip| -> Option<Option<MergeClue>> {
            resolve(branch, &targets, |key| {
                if let Some(known) = cache.pair(key) {
                    return Some(known);
                }
                if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                    return None;
                }
                let target = targets.iter().find(|t| t.name == key.target && t.id == key.target_id)?;
                let answer = check_pair(branch, target);
                cache.pairs_checked.fetch_add(1, Ordering::Relaxed);
                cache.keep(key.clone(), answer.clone());
                Some(answer)
            })
        };

        std::thread::scope(|scope| {
            for _ in 0..scan_threads().min(branches.len()).max(1) {
                scope.spawn(|| {
                    loop {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        let Some(branch) = branches.get(at) else { break };
                        match check(branch) {
                            Some(Some(clue)) => found.lock().expect("no panic while holding the lock").push((at, clue)),
                            Some(None) => {}
                            None => {
                                unchecked.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
        });

        // A cancelled scan's inputs are already out of date: they say nothing about what to keep.
        if !cancel.load(Ordering::Relaxed) {
            cache.prune(branches, &targets);
        }
        let mut found = found.into_inner().expect("no panic while holding the lock");
        found.sort_by_key(|(at, _)| *at);
        Ok(MergeScan { clues: found.into_iter().map(|(_, clue)| clue).collect(), unchecked: unchecked.into_inner() })
    }
}

/// Makes `process` run at a lower CPU priority (nice 10), so it gives way to the window and to other
/// programs. Nothing changes where that is not possible.
#[cfg(unix)]
fn lower_priority(process: &mut Process) {
    use std::os::unix::process::CommandExt;
    unsafe extern "C" {
        fn setpriority(which: i32, who: u32, prio: i32) -> i32;
    }
    const PRIO_PROCESS: i32 = 0;
    // SAFETY: runs in the child between fork and exec, and only makes one system call, which is
    // async-signal-safe; its result is ignored, so a refusal changes nothing.
    unsafe {
        process.pre_exec(|| {
            setpriority(PRIO_PROCESS, 0, 10);
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn lower_priority(_process: &mut Process) {}

/// How long `merge_clues` may work, how many of a trunk's newest commits it fingerprints, and how many
/// branches it checks at once at most.
const MERGE_SCAN_BUDGET: Duration = Duration::from_secs(25);
const SCAN_COMMITS: usize = 500;
const MOST_SCAN_THREADS: usize = 6;

/// How many branches the scan checks at once: half the cores (each check runs git processes one after
/// another), so a scan never takes the whole machine.
fn scan_threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(1, MOST_SCAN_THREADS)
}

/// The patch id of each recent commit on a trunk, newest first, mapped to the commit.
struct TargetScan {
    patches: HashMap<String, String>,
}

/// One branch checked against one trunk, each at the commit it pointed at. What git says about the pair
/// cannot change while neither moves, so the answer is kept.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PairKey {
    branch: String,
    branch_id: String,
    target: String,
    target_id: String,
}

impl PairKey {
    fn of(branch: &BranchTip, target: &BranchTip) -> Self {
        Self { branch: branch.name.clone(), branch_id: branch.id.clone(), target: target.name.clone(), target_id: target.id.clone() }
    }
}

type TargetSlot = Arc<OnceLock<Option<Arc<TargetScan>>>>;

/// What earlier merge scans of one repository found, so the next scan only asks git about branches and
/// trunks that have moved since. Refreshing, or any operation that reads the repository again, then
/// costs nothing when the branches are where they were.
///
/// It holds an answer per branch and trunk pair plus each trunk's fingerprints, and after each
/// complete scan only what that scan was about, so it stays small.
#[derive(Default)]
pub struct ScanCache {
    pairs: Mutex<HashMap<PairKey, Option<MergeClue>>>,
    /// Each trunk's fingerprints, by commit. Shared by scans running at the same time, so a scan that
    /// replaces a cancelled one waits for the trunk being read instead of reading it a second time.
    targets: Mutex<HashMap<String, TargetSlot>>,
    /// Pairs worked out with git, and trunks fingerprinted, over this cache's life.
    pairs_checked: AtomicUsize,
    targets_scanned: AtomicUsize,
}

impl ScanCache {
    /// How many branch and trunk pairs have been asked of git, over this cache's life.
    pub fn pairs_checked(&self) -> usize {
        self.pairs_checked.load(Ordering::Relaxed)
    }

    /// How many trunks have been fingerprinted (`git log -p`), over this cache's life.
    pub fn targets_scanned(&self) -> usize {
        self.targets_scanned.load(Ordering::Relaxed)
    }

    /// How many answers are kept.
    pub fn len(&self) -> usize {
        self.pairs.lock().map_or(0, |pairs| pairs.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The whole scan of `branches` against `targets`, when every answer it needs is already known.
    pub fn lookup(&self, branches: &[BranchTip], targets: &[BranchTip]) -> Option<MergeScan> {
        let targets = unique_targets(targets);
        let pairs = self.pairs.lock().ok()?;
        let mut clues = Vec::new();
        for branch in branches {
            if let Some(clue) = resolve(branch, &targets, |key| pairs.get(key).cloned())? {
                clues.push(clue);
            }
        }
        Some(MergeScan { clues, unchecked: 0 })
    }

    fn pair(&self, key: &PairKey) -> Option<Option<MergeClue>> {
        self.pairs.lock().ok()?.get(key).cloned()
    }

    fn keep(&self, key: PairKey, answer: Option<MergeClue>) {
        if let Ok(mut pairs) = self.pairs.lock() {
            pairs.insert(key, answer);
        }
    }

    fn target_slot(&self, id: &str) -> TargetSlot {
        let mut targets = self.targets.lock().expect("no panic while holding the lock");
        targets.entry(id.to_owned()).or_default().clone()
    }

    /// Forgets everything that is not about these branches and trunks.
    fn prune(&self, branches: &[BranchTip], targets: &[&BranchTip]) {
        let branches: HashSet<(&str, &str)> = branches.iter().map(|b| (b.name.as_str(), b.id.as_str())).collect();
        let tips: HashSet<(&str, &str)> = targets.iter().map(|t| (t.name.as_str(), t.id.as_str())).collect();
        if let Ok(mut pairs) = self.pairs.lock() {
            pairs.retain(|key, _| {
                branches.contains(&(key.branch.as_str(), key.branch_id.as_str()))
                    && tips.contains(&(key.target.as_str(), key.target_id.as_str()))
            });
        }
        let ids: HashSet<&str> = targets.iter().map(|t| t.id.as_str()).collect();
        if let Ok(mut scans) = self.targets.lock() {
            scans.retain(|id, _| ids.contains(id.as_str()));
        }
    }
}

/// Two names for one commit (a branch and its remote) are one target.
fn unique_targets(targets: &[BranchTip]) -> Vec<&BranchTip> {
    let mut seen = HashSet::new();
    targets.iter().filter(|t| seen.insert(t.id.as_str())).collect()
}

/// The answer for one branch: the first trunk, in order, that it was merged into. `answer` gives what is
/// known about one pair, or `None` when that pair could not be checked; the whole answer is then `None`.
fn resolve(
    branch: &BranchTip,
    targets: &[&BranchTip],
    mut answer: impl FnMut(&PairKey) -> Option<Option<MergeClue>>,
) -> Option<Option<MergeClue>> {
    for target in targets {
        if branch.id == target.id || branch.name == target.name {
            continue;
        }
        if let Some(clue) = answer(&PairKey::of(branch, target))? {
            return Some(Some(clue));
        }
    }
    Some(None)
}

// Records start with 0x1e and fields are split by 0x1f, so subjects and names can hold anything else.
const RECORD: char = '\x1e';
const FIELD: char = '\x1f';
const RECORD_END: char = '\x1e';
// Names and emails through `.mailmap` (%aN, %cN…), so a repository can say which identities are one person.
const LOG_FORMAT: &str = "--format=%x1e%H%x1f%P%x1f%aN%x1f%aE%x1f%at%x1f%ad%x1f%D%x1f%cN%x1f%cE%x1f%s";
const DATE_FORMAT: &str = "--date=format-local:%e %b %Y %H:%M";
const LONG_DATE_FORMAT: &str = "--date=format-local:%a %b %e %Y %H:%M:%S %z";
const DETAIL_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%cn%x1f%ce%x1f%ad%x1f%B";

impl Backend for GitCli {
    fn log(&self, options: &LogOptions) -> Result<Vec<Commit>, Error> {
        let (stashes, stdout) = self.log_output(options)?;
        commits_of(&stashes, &stdout)
    }

    fn current_branch(&self) -> Result<Option<String>, Error> {
        let output = self.command(&["symbolic-ref", "--short", "--quiet", "HEAD"]).output().map_err(Error::Spawn)?;
        // Exit 1 is how `--quiet` says "HEAD is detached", which is an answer, not a failure.
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|name| !name.is_empty()))
    }

    fn changed_files(&self) -> Result<usize, Error> {
        let out = self.run(&["status", "--porcelain=v1", "--untracked-files=all", "-z"])?;
        // `-z` separates entries with NUL; a rename adds a second NUL-terminated path for its origin.
        let mut count = 0;
        let mut entries = out.split(|byte| *byte == 0).filter(|entry| !entry.is_empty());
        while let Some(entry) = entries.next() {
            count += 1;
            if matches!(entry.first(), Some(b'R' | b'C')) {
                entries.next();
            }
        }
        Ok(count)
    }

    fn commit_detail(&self, id: &str) -> Result<CommitDetail, Error> {
        check_rev(id)?;
        let out = self.run(&["show", "-s", "--no-color", DETAIL_FORMAT, LONG_DATE_FORMAT, id])?;
        parse_detail(&String::from_utf8_lossy(&out))
    }

    fn commit_files(&self, id: &str) -> Result<Vec<FileChange>, Error> {
        let base = self.base_of(id)?;
        let common = ["--no-ext-diff", "--no-color", "-z", "-M"];
        let names = self.run(&[&["diff", "--name-status"], &common[..], &[base.as_str(), id]].concat())?;
        let counts = self.run(&[&["diff", "--numstat"], &common[..], &[base.as_str(), id]].concat())?;
        Ok(parse_changes(&names, &counts))
    }

    fn merge_clues(&self, branches: &[BranchTip], targets: &[BranchTip]) -> Result<MergeScan, Error> {
        self.merge_clues_cached(branches, targets, &ScanCache::default(), &AtomicBool::new(false))
    }

    fn file_diff(&self, id: &str, file: &FileChange, context: u32) -> Result<FileDiff, Error> {
        let base = self.base_of(id)?;
        let context = format!("-U{context}");
        let mut args = vec!["diff", "--no-color", "--no-ext-diff", "-M", context.as_str(), base.as_str(), id, "--"];
        // Naming both paths lets git see a rename instead of a deletion and an addition.
        args.extend(file.old_path.as_deref());
        args.push(&file.path);
        let out = self.run(&args)?;
        Ok(diff::parse(&String::from_utf8_lossy(&out)))
    }
}

fn parse_detail(output: &str) -> Result<CommitDetail, Error> {
    let fields: Vec<&str> = output.splitn(8, FIELD).collect();
    let [id, parents, author, author_email, committer, committer_email, date, message] = fields[..] else {
        return Err(Error::Parse(format!("expected 8 fields, got {}", fields.len())));
    };
    Ok(CommitDetail {
        id: id.trim().to_owned(),
        parents: parents.split_whitespace().map(str::to_owned).collect(),
        author: author.to_owned(),
        author_email: author_email.to_owned(),
        committer: committer.to_owned(),
        committer_email: committer_email.to_owned(),
        date: date.trim().to_owned(),
        message: message.trim_end().to_owned(),
    })
}

/// Joins `git diff --name-status -z` with `--numstat -z`. Both name the new path last, so it is the key.
pub(crate) fn parse_changes(name_status: &[u8], numstat: &[u8]) -> Vec<FileChange> {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();

    // numstat: `adds\tdels\tpath\0`, or `adds\tdels\t\0old\0new\0` for a rename. Binary files say `-`.
    let mut counts: HashMap<String, (Option<u32>, Option<u32>)> = HashMap::new();
    let mut tokens = numstat.split(|byte| *byte == 0).filter(|token| !token.is_empty());
    while let Some(token) = tokens.next() {
        let token = text(token);
        let mut parts = token.splitn(3, '\t');
        let (Some(adds), Some(dels), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let path = if path.is_empty() {
            let _old = tokens.next();
            tokens.next().map(text).unwrap_or_default()
        } else {
            path.to_owned()
        };
        counts.insert(path, (adds.parse().ok(), dels.parse().ok()));
    }

    let mut changes = Vec::new();
    let mut tokens = name_status.split(|byte| *byte == 0).filter(|token| !token.is_empty());
    while let Some(status) = tokens.next() {
        let status = text(status);
        let (kind, two_paths) = match status.chars().next() {
            Some('A') => (FileStatus::Added, false),
            Some('M') => (FileStatus::Modified, false),
            Some('D') => (FileStatus::Deleted, false),
            Some('T') => (FileStatus::TypeChanged, false),
            Some('R') => (FileStatus::Renamed, true),
            Some('C') => (FileStatus::Copied, true),
            _ => (FileStatus::Modified, false),
        };
        let first = tokens.next().map(text).unwrap_or_default();
        let (path, old_path) = if two_paths {
            (tokens.next().map(text).unwrap_or_default(), Some(first))
        } else {
            (first, None)
        };
        let (additions, deletions) = counts.get(&path).copied().unwrap_or((None, None));
        changes.push(FileChange { path, old_path, status: kind, additions, deletions });
    }
    changes
}

/// Rejects ids that git would read as options. Ids come from our own UI, so this is a backstop.
fn check_rev(id: &str) -> Result<(), Error> {
    if id.is_empty() || id.starts_with('-') || id.contains(char::is_whitespace) {
        return Err(Error::Parse(format!("not a revision: {id:?}")));
    }
    Ok(())
}

/// A stash commit has the commit it was made on as its first parent and the index (and untracked
/// files) as extra parents. Show each stash as one commit hanging off its base.
fn fold_stashes(commits: &mut Vec<Commit>, stashes: &[String]) {
    let mut internal: HashSet<String> = HashSet::new();
    for commit in commits.iter_mut() {
        let Some(n) = stashes.iter().position(|id| *id == commit.id) else { continue };
        internal.extend(commit.parents.drain(1..));
        commit.stash = Some(n);
        commit.refs.push(Ref { name: format!("stash@{{{n}}}"), kind: RefKind::Stash });
    }
    commits.retain(|commit| !internal.contains(&commit.id));
}

/// The commits in `git log` output, each stash folded into one commit.
fn commits_of(stashes: &[String], stdout: &[u8]) -> Result<Vec<Commit>, Error> {
    let mut commits = parse_log(&String::from_utf8_lossy(stdout))?;
    fold_stashes(&mut commits, stashes);
    Ok(commits)
}

pub(crate) fn parse_log(output: &str) -> Result<Vec<Commit>, Error> {
    output
        .split(RECORD)
        .filter(|record| !record.trim().is_empty())
        .map(parse_record)
        .collect()
}

fn parse_record(record: &str) -> Result<Commit, Error> {
    let fields: Vec<&str> = record.trim_end_matches('\n').splitn(10, FIELD).collect();
    let [id, parents, author, email, time, date, refs, committer, committer_email, summary] = fields[..] else {
        return Err(Error::Parse(format!("expected 10 fields, got {}", fields.len())));
    };
    Ok(Commit {
        id: id.to_owned(),
        parents: parents.split_whitespace().map(str::to_owned).collect(),
        author: author.to_owned(),
        email: email.to_owned(),
        committer: committer.to_owned(),
        committer_email: committer_email.to_owned(),
        time: time.parse().map_err(|_| Error::Parse(format!("bad time {time:?}")))?,
        date: date.trim().to_owned(),
        summary: summary.to_owned(),
        refs: parse_refs(refs),
        stash: None,
    })
}

/// Parses `%D` under `--decorate=full`: `HEAD -> refs/heads/main, refs/remotes/origin/main, tag: refs/tags/v1`.
fn parse_refs(decorations: &str) -> Vec<Ref> {
    let mut refs = Vec::new();
    for item in decorations.split(", ").map(str::trim).filter(|item| !item.is_empty()) {
        if let Some(target) = item.strip_prefix("HEAD -> ") {
            refs.push(Ref { name: "HEAD".into(), kind: RefKind::Head });
            refs.extend(local_or_other(target));
        } else if item == "HEAD" {
            refs.push(Ref { name: "HEAD".into(), kind: RefKind::Head });
        } else if let Some(tag) = item.strip_prefix("tag: ") {
            refs.push(Ref {
                name: tag.strip_prefix("refs/tags/").unwrap_or(tag).to_owned(),
                kind: RefKind::Tag,
            });
        } else {
            refs.extend(local_or_other(item));
        }
    }
    refs
}

fn local_or_other(full: &str) -> Option<Ref> {
    if let Some(name) = full.strip_prefix("refs/heads/") {
        Some(Ref { name: name.to_owned(), kind: RefKind::LocalBranch })
    } else if let Some(name) = full.strip_prefix("refs/remotes/") {
        // `origin/HEAD` only says which branch the remote shows by default; it is not a branch.
        (!name.ends_with("/HEAD")).then(|| Ref { name: name.to_owned(), kind: RefKind::RemoteBranch })
    } else if let Some(name) = full.strip_prefix("refs/tags/") {
        Some(Ref { name: name.to_owned(), kind: RefKind::Tag })
    } else {
        // refs/stash is shown as `stash@{n}` instead; notes and pull refs stay hidden.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(refs: &[Ref]) -> Vec<(&str, RefKind)> {
        refs.iter().map(|r| (r.name.as_str(), r.kind)).collect()
    }

    #[test]
    fn parses_refs_under_full_decoration() {
        let refs = parse_refs(
            "HEAD -> refs/heads/main, refs/remotes/origin/main, tag: refs/tags/v1.0, refs/heads/feature/x",
        );
        assert_eq!(
            kinds(&refs),
            [
                ("HEAD", RefKind::Head),
                ("main", RefKind::LocalBranch),
                ("origin/main", RefKind::RemoteBranch),
                ("v1.0", RefKind::Tag),
                ("feature/x", RefKind::LocalBranch),
            ]
        );
    }

    #[test]
    fn a_detached_head_is_just_head() {
        let refs = parse_refs("HEAD");
        assert_eq!(refs, [Ref { name: "HEAD".into(), kind: RefKind::Head }]);
    }

    #[test]
    fn the_remote_default_pointer_and_unknown_refs_are_hidden() {
        let refs = parse_refs("refs/remotes/origin/HEAD, refs/stash, refs/pull/42/head, refs/notes/commits");
        assert!(refs.is_empty());
    }

    #[test]
    fn parses_a_record_with_two_parents_and_a_separator_free_subject() {
        let out = format!(
            "{RECORD}abc123{FIELD}p1 p2{FIELD}Ada{FIELD}ada@example.com{FIELD}1700000000{FIELD} 6 Oct 2026 15:22{FIELD}HEAD -> refs/heads/main{FIELD}Bob{FIELD}bob@example.com{FIELD}Merge: it | works\n"
        );
        let commits = parse_log(&out).unwrap();
        assert_eq!(commits.len(), 1);
        let c = &commits[0];
        assert_eq!(c.parents, ["p1", "p2"]);
        assert!(c.is_merge());
        assert_eq!(c.time, 1_700_000_000);
        assert_eq!(c.date, "6 Oct 2026 15:22", "git pads the day with a space; we trim it");
        assert_eq!(c.summary, "Merge: it | works");
        assert_eq!((c.committer.as_str(), c.committer_email.as_str()), ("Bob", "bob@example.com"));
        assert_eq!(c.short_id(), "abc123");
    }

    #[test]
    fn a_root_commit_has_no_parents_and_no_refs() {
        let out = format!("{RECORD}abc{FIELD}{FIELD}A{FIELD}a@b{FIELD}1{FIELD}d{FIELD}{FIELD}A{FIELD}a@b{FIELD}init\n");
        let c = &parse_log(&out).unwrap()[0];
        assert!(c.parents.is_empty());
        assert!(c.refs.is_empty());
    }

    #[test]
    fn a_short_record_is_an_error_not_a_panic() {
        let out = format!("{RECORD}abc{FIELD}only two");
        assert!(matches!(parse_log(&out), Err(Error::Parse(_))));
    }

    fn commit(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: "A".into(),
            email: "a@b".into(),
            time: 0,
            date: String::new(),
            summary: id.into(),
            refs: Vec::new(),
            stash: None,
            committer: String::new(),
            committer_email: String::new(),
        }
    }

    #[test]
    fn a_stash_becomes_one_commit_on_its_base_and_its_index_commit_disappears() {
        let mut commits = vec![commit("stash", &["base", "index"]), commit("index", &["base"]), commit("base", &[])];
        fold_stashes(&mut commits, &["stash".to_owned()]);
        let ids: Vec<&str> = commits.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["stash", "base"]);
        assert_eq!(commits[0].parents, ["base"]);
        assert_eq!(commits[0].stash, Some(0));
        assert_eq!(kinds(&commits[0].refs), [("stash@{0}", RefKind::Stash)]);
    }

    #[test]
    fn an_untracked_files_commit_is_folded_away_too() {
        let mut commits = vec![commit("stash", &["base", "index", "untracked"]), commit("untracked", &[]), commit("index", &["base"]), commit("base", &[])];
        fold_stashes(&mut commits, &["stash".to_owned()]);
        let ids: Vec<&str> = commits.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["stash", "base"]);
    }

    #[test]
    fn older_stashes_are_numbered_by_their_position() {
        let mut commits = vec![commit("s0", &["base", "i0"]), commit("s1", &["base", "i1"]), commit("base", &[])];
        fold_stashes(&mut commits, &["s0".to_owned(), "s1".to_owned()]);
        assert_eq!(commits[1].stash, Some(1));
        assert_eq!(kinds(&commits[1].refs), [("stash@{1}", RefKind::Stash)]);
    }

    #[test]
    fn joins_name_status_with_numstat_by_new_path() {
        let names = b"M\0src/a.rs\0A\0new.rs\0R087\0old.rs\0renamed.rs\0D\0gone.rs\0A\0img.png\0";
        let counts = b"3\t1\tsrc/a.rs\010\t0\tnew.rs\02\t2\t\0old.rs\0renamed.rs\00\t5\tgone.rs\0-\t-\timg.png\0";
        let changes = parse_changes(names, counts);

        let got: Vec<(&str, Option<&str>, FileStatus, Option<u32>, Option<u32>)> = changes
            .iter()
            .map(|c| (c.path.as_str(), c.old_path.as_deref(), c.status, c.additions, c.deletions))
            .collect();
        assert_eq!(
            got,
            [
                ("src/a.rs", None, FileStatus::Modified, Some(3), Some(1)),
                ("new.rs", None, FileStatus::Added, Some(10), Some(0)),
                ("renamed.rs", Some("old.rs"), FileStatus::Renamed, Some(2), Some(2)),
                ("gone.rs", None, FileStatus::Deleted, Some(0), Some(5)),
                ("img.png", None, FileStatus::Added, None, None),
            ]
        );
    }

    #[test]
    fn no_changes_is_an_empty_list() {
        assert!(parse_changes(b"", b"").is_empty());
    }

    #[test]
    fn parses_a_commit_detail_with_a_multi_line_message() {
        let out = format!(
            "abc{FIELD}p1 p2{FIELD}Ada{FIELD}ada@x.io{FIELD}GitHub{FIELD}noreply@github.com{FIELD}Tue Oct  6 2026 15:22:06 +0700{FIELD}Subject line\n\nBody line one\nBody two\n"
        );
        let detail = parse_detail(&out).unwrap();
        assert_eq!(detail.parents, ["p1", "p2"]);
        assert_eq!(detail.committer, "GitHub");
        assert_eq!(detail.date, "Tue Oct  6 2026 15:22:06 +0700");
        assert_eq!(detail.message, "Subject line\n\nBody line one\nBody two");
    }

    #[test]
    fn revisions_that_look_like_options_are_refused() {
        assert!(check_rev("--output=/tmp/x").is_err());
        assert!(check_rev("").is_err());
        assert!(check_rev("a b").is_err());
        assert!(check_rev("abc123").is_ok());
        assert!(check_rev("HEAD~2").is_ok());
    }
}
