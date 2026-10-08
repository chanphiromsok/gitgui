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
    /// The branch HEAD is on, the branch it was cut from, and their remote copies: where the work stands
    /// against the trunk and the remote, without the other branches around it.
    #[default]
    Focus,
    /// Every branch, remote ones included, with tags and stashes.
    All,
    /// Local branches, and the remote copies that point where they do.
    Local,
    /// Only the history of the commit HEAD is on.
    Current,
    /// Only the branches named in the focus, whether or not HEAD is on one: one branch picked out of the graph
    /// with the branch it was cut from. Not a choice of the filter bar; the graph asks for it.
    Only,
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
    filter_with_focus(commits, scope, hidden, stashes, &[])
}

/// `filter_commits`, where `Scope::Focus` also shows the branches named in `focus` (`release/1.0.0`: the local
/// branch and any remote copy of it, `origin/release/1.0.0`) along with the branch HEAD is on.
pub fn filter_with_focus(commits: &[Commit], scope: Scope, hidden: &HashSet<String>, stashes: bool, focus: &[String]) -> Vec<Commit> {
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

    let in_focus = |r: &crate::model::Ref| match r.kind {
        RefKind::LocalBranch => focus.contains(&r.name),
        RefKind::RemoteBranch => focus.contains(&r.name) || focus.contains(&tail(&r.name)),
        _ => false,
    };
    let is_tip = |commit: &Commit| {
        commit.refs.iter().any(|r| match scope {
            Scope::Focus => r.kind == RefKind::Head || in_focus(r),
            Scope::Only => in_focus(r),
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

// ---- author and date --------------------------------------------------------------------------

/// A calendar day: year, month, day. Tuples order the way days do.
pub type Day = (i32, u32, u32);

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm), and back.
fn days_from_civil((y, m, d): Day) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> Day {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m, d)
}

/// The day `n` days after `day` (before, when negative).
pub fn add_days(day: Day, n: i64) -> Day {
    civil_from_days(days_from_civil(day) + n)
}

/// The day a moment falls on, `utc_offset` seconds east of UTC.
pub fn day_of(epoch_seconds: i64, utc_offset: i64) -> Day {
    civil_from_days((epoch_seconds + utc_offset).div_euclid(86_400))
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// `2026-10-05` as a day; `2026-10` as its first day (and `month_end` its last).
fn parse_day(text: &str) -> Option<Day> {
    let mut parts = text.split('-');
    // Fixed widths, so a date half-typed (`2026-1`) is not taken for January while its month is still being written.
    fn field(part: Option<&str>, width: usize) -> Option<&str> {
        part.filter(|p| p.len() == width && p.bytes().all(|b| b.is_ascii_digit()))
    }
    let y: i32 = field(parts.next(), 4)?.parse().ok().filter(|y| (1970..=9999).contains(y))?;
    let m: u32 = field(parts.next(), 2)?.parse().ok().filter(|m| (1..=12).contains(m))?;
    let d: u32 = match parts.next() {
        Some(d) => field(Some(d), 2)?.parse().ok().filter(|d| (1..=days_in_month(y, m)).contains(d))?,
        None => 1,
    };
    parts.next().is_none().then_some((y, m, d))
}

/// The last day an incomplete date covers: `2026-10` → 31 October; a full date is itself.
fn parse_day_end(text: &str) -> Option<Day> {
    let (y, m, d) = parse_day(text)?;
    Some(if text.matches('-').count() == 1 { (y, m, days_in_month(y, m)) } else { (y, m, d) })
}

/// The day a commit was made, from its `date` ("7 Oct 2026 15:32", in the user's time zone).
pub fn commit_day(commit: &Commit) -> Option<Day> {
    let mut words = commit.date.split_whitespace();
    let d: u32 = words.next()?.parse().ok()?;
    let month = words.next()?;
    let y: i32 = words.next()?.parse().ok()?;
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let m = MONTHS.iter().position(|name| month.to_ascii_lowercase().starts_with(name))? as u32 + 1;
    Some((y, m, d))
}

/// Narrowing the graph to one author or a stretch of days, from `author:` and `date:` words in the search.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Constraints {
    /// A commit matches when any of these is its email (whole, ignoring case) or is part of its author name
    /// or email. Empty: anyone.
    pub authors: Vec<String>,
    /// First and last day, both included. `None`: no limit on that side.
    pub since: Option<Day>,
    pub until: Option<Day>,
}

impl Constraints {
    pub fn is_empty(&self) -> bool {
        self.authors.is_empty() && self.since.is_none() && self.until.is_none()
    }

    pub fn matches(&self, commit: &Commit) -> bool {
        if !self.authors.is_empty() {
            let (name, email) = (commit.author.to_lowercase(), commit.email.to_lowercase());
            let by_author = self.authors.iter().any(|a| email == *a || name.contains(a.as_str()) || email.contains(a.as_str()));
            if !by_author {
                return false;
            }
        }
        if self.since.is_some() || self.until.is_some() {
            let Some(day) = commit_day(commit) else { return false };
            if self.since.is_some_and(|since| day < since) || self.until.is_some_and(|until| day > until) {
                return false;
            }
        }
        true
    }
}

/// The words of a search, split on spaces, except inside double quotes (`author:"Ada Lovelace"`).
fn words(text: &str) -> Vec<String> {
    let (mut out, mut word, mut quoted) = (Vec::new(), String::new(), false);
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// A `date:` value: `today`, `yesterday`, `week`, `month`, `7d` (the last 7 days), `2026-10-05`, `2026-10`, or
/// a range `2026-10-01..2026-10-07` with either end left open.
fn parse_date_spec(spec: &str, today: Day) -> Option<(Option<Day>, Option<Day>)> {
    let spec = spec.to_ascii_lowercase();
    match spec.as_str() {
        "today" => return Some((Some(today), Some(today))),
        "yesterday" => {
            let day = add_days(today, -1);
            return Some((Some(day), Some(day)));
        }
        "week" => return Some((Some(add_days(today, -6)), Some(today))),
        "month" => return Some((Some((today.0, today.1, 1)), Some(today))),
        _ => {}
    }
    if let Some(n) = spec.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()).filter(|n| (1..=36_500).contains(n)) {
        return Some((Some(add_days(today, 1 - n)), Some(today)));
    }
    if let Some((from, to)) = spec.split_once("..") {
        let from = if from.is_empty() { None } else { Some(parse_day(from)?) };
        let to = if to.is_empty() { None } else { Some(parse_day_end(to)?) };
        return Some((from, to));
    }
    Some((Some(parse_day(&spec)?), Some(parse_day_end(&spec)?)))
}

/// Takes the `author:`, `date:`, `since:` and `until:` words out of a search. What is left is returned with
/// the constraints; a word that does not make sense (a half-typed date) is dropped and constrains nothing.
pub fn parse_constraints(text: &str, today: Day) -> (Constraints, String) {
    let mut constraints = Constraints::default();
    let mut rest: Vec<String> = Vec::new();
    for word in words(text) {
        let lower = word.to_ascii_lowercase();
        let value = |key: &str| lower.strip_prefix(key).map(|_| word[key.len()..].trim().to_owned());
        if let Some(author) = value("author:") {
            if !author.is_empty() {
                constraints.authors.push(author.to_lowercase());
            }
        } else if let Some(spec) = value("date:") {
            if let Some((since, until)) = parse_date_spec(&spec, today) {
                (constraints.since, constraints.until) = (since, until);
            }
        } else if let Some(day) = value("since:") {
            constraints.since = parse_day(&day).or(constraints.since);
        } else if let Some(day) = value("until:") {
            constraints.until = parse_day_end(&day).or(constraints.until);
        } else if word.contains('"') || word.contains(' ') {
            rest.push(format!("\"{word}\""));
        } else {
            rest.push(word);
        }
    }
    (constraints, rest.join(" "))
}

/// The value of the first `key:` word in a search, as typed.
pub fn term(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    words(text)
        .into_iter()
        .find(|word| word.to_ascii_lowercase().starts_with(&prefix))
        .map(|word| word[prefix.len()..].to_owned())
        .filter(|value| !value.is_empty())
}

/// The search with every `key:` word replaced by `key:value` (or removed when `value` is `None`).
pub fn with_term(text: &str, key: &str, value: Option<&str>) -> String {
    let prefix = format!("{key}:");
    let mut kept: Vec<String> = words(text)
        .into_iter()
        .filter(|word| !word.to_ascii_lowercase().starts_with(&prefix))
        .map(|word| if word.contains(' ') { format!("\"{word}\"") } else { word })
        .collect();
    if let Some(value) = value {
        kept.push(if value.contains(' ') { format!("{prefix}\"{value}\"") } else { format!("{prefix}{value}") });
    }
    kept.join(" ")
}

/// The commits that pass `keep`, with each one's parents moved down to the nearest commits that also pass,
/// the way `git log` simplifies a filtered history: what is left stays one connected graph, with lines
/// running past the commits that were left out.
pub fn narrow(commits: &[Commit], keep: impl Fn(&Commit) -> bool) -> Vec<Commit> {
    /// A long dropped stretch can lead to very many kept ancestors; this many are enough to draw the lines.
    const MOST: usize = 32;
    let kept: Vec<bool> = commits.iter().map(&keep).collect();
    let index: std::collections::HashMap<&str, usize> = commits.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();
    // The nearest kept commit(s) at or below each commit. Parents come after their children, so go from the end.
    let mut nearest: Vec<Vec<usize>> = vec![Vec::new(); commits.len()];
    let below = |commit: &Commit, nearest: &[Vec<usize>]| -> Vec<usize> {
        let mut found: Vec<usize> = Vec::new();
        for parent in commit.parents.iter().filter_map(|p| index.get(p.as_str())) {
            for &ix in &nearest[*parent] {
                if !found.contains(&ix) && found.len() < MOST {
                    found.push(ix);
                }
            }
        }
        found
    };
    for ix in (0..commits.len()).rev() {
        nearest[ix] = if kept[ix] { vec![ix] } else { below(&commits[ix], &nearest) };
    }
    commits
        .iter()
        .enumerate()
        .filter(|(ix, _)| kept[*ix])
        .map(|(_, commit)| {
            let mut commit = commit.clone();
            commit.parents = below(&commit, &nearest).into_iter().map(|ix| commits[ix].id.clone()).collect();
            commit
        })
        .collect()
}

/// The merge commits that only bring a trunk into a feature branch to keep it current ("Merge branch 'release/1.0.0'
/// into feat/x"): the merge itself is not on a trunk's own line, but the commit it merges in is. A pull request
/// merge is the other way round (it lands on the trunk's line) and a pull of a feature branch merges another
/// feature line, so neither counts. A trunk is a branch ranked like `main`, `develop`, `release/…` or `staging`.
pub fn sync_merges(commits: &[Commit]) -> HashSet<String> {
    let by_id: std::collections::HashMap<&str, &Commit> = commits.iter().map(|c| (c.id.as_str(), c)).collect();
    // Every commit on the first-parent line of a trunk: the line the trunk's own commits and merges are on.
    let mut line: HashSet<&str> = HashSet::new();
    for tip in commits.iter().filter(|c| crate::lineage::commit_rank(c) >= 50) {
        let mut at = Some(tip);
        while let Some(commit) = at {
            if !line.insert(commit.id.as_str()) {
                break;
            }
            at = commit.parents.first().and_then(|p| by_id.get(p.as_str()).copied());
        }
    }
    commits
        .iter()
        .filter(|c| c.is_merge() && crate::squash::subject_pr(&c.summary).is_none())
        .filter(|c| !line.contains(c.id.as_str()) && c.parents.get(1).is_some_and(|merged| line.contains(merged.as_str())))
        .map(|c| c.id.clone())
        .collect()
}

/// `commits` without those in `drop`: whoever had one as a parent has its first parent instead (what the merge
/// only brought in is on the trunk's line anyway). Meant for merges that are not worth a row; the branches and
/// tags a dropped commit carries are the caller's to keep it for.
pub fn without_commits(commits: &[Commit], drop: &HashSet<String>) -> Vec<Commit> {
    if drop.is_empty() {
        return commits.to_vec();
    }
    let by_id: std::collections::HashMap<&str, &Commit> = commits.iter().map(|c| (c.id.as_str(), c)).collect();
    let past = |id: &str| -> Option<String> {
        let mut at = id.to_owned();
        while drop.contains(&at) {
            at = by_id.get(at.as_str())?.parents.first()?.clone();
        }
        Some(at)
    };
    commits
        .iter()
        .filter(|c| !drop.contains(&c.id))
        .map(|commit| {
            let mut commit = commit.clone();
            let mut parents: Vec<String> = Vec::new();
            for parent in commit.parents.iter().filter_map(|p| past(p)) {
                if !parents.contains(&parent) {
                    parents.push(parent);
                }
            }
            commit.parents = parents;
            commit
        })
        .collect()
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

    /// release: t2 (and origin/release); feat (HEAD): f1 -> t1, its remote copy a commit ahead at f2; other: o1 -> t1.
    fn moved_on() -> Vec<Commit> {
        vec![
            commit("t2", &["t1"], &[("release", RefKind::LocalBranch), ("origin/release", RefKind::RemoteBranch)]),
            commit("o1", &["t1"], &[("other", RefKind::LocalBranch)]),
            commit("f2", &["f1"], &[("origin/feat", RefKind::RemoteBranch)]),
            commit("f1", &["t1"], &[("HEAD", RefKind::Head), ("feat", RefKind::LocalBranch)]),
            commit("t1", &["t0"], &[]),
            commit("t0", &[], &[]),
        ]
    }

    #[test]
    fn focus_is_the_branch_its_base_and_their_remote_copies_and_nothing_else() {
        let focus = ["release".to_owned(), "feat".to_owned()];
        let shown = filter_with_focus(&moved_on(), Scope::Focus, &HashSet::new(), true, &focus);
        assert_eq!(ids(&shown), ["t2", "f2", "f1", "t1", "t0"], "the base moved on, the remote is ahead, the other branch is left out");
        // Without a base named, it is the branch HEAD is on and what it comes from.
        let alone = filter_with_focus(&moved_on(), Scope::Focus, &HashSet::new(), true, &[]);
        assert_eq!(ids(&alone), ["f1", "t1", "t0"]);
    }

    /// release/1.0.0: rel2 (a PR merge of feat) -> rel1 -> rel0. feat: f3 -> s1 (merges rel1 in) -> f2 -> f1 -> rel0.
    fn with_a_sync_merge() -> Vec<Commit> {
        let mut commits = vec![
            commit("rel2", &["rel1", "f3"], &[("release/1.0.0", RefKind::LocalBranch)]),
            commit("f3", &["s1"], &[("feat", RefKind::LocalBranch)]),
            commit("s1", &["f2", "rel1"], &[]),
            commit("f2", &["f1"], &[]),
            commit("rel1", &["rel0"], &[]),
            commit("f1", &["rel0"], &[]),
            commit("rel0", &[], &[]),
        ];
        commits[0].summary = "Merge pull request #5 from o/feat".into();
        commits[2].summary = "Merge branch 'release/1.0.0' into feat".into();
        commits
    }

    #[test]
    fn a_merge_that_only_brings_the_trunk_in_is_a_sync_merge_and_a_pull_request_merge_is_not() {
        let found = sync_merges(&with_a_sync_merge());
        assert_eq!(found, HashSet::from(["s1".to_owned()]));
    }

    #[test]
    fn dropping_a_sync_merge_joins_its_children_to_its_first_parent() {
        let commits = with_a_sync_merge();
        let shown = without_commits(&commits, &sync_merges(&commits));
        assert_eq!(ids(&shown), ["rel2", "f3", "f2", "rel1", "f1", "rel0"]);
        assert_eq!(shown[1].parents, ["f2"], "f3 now comes straight after f2, not after the merge");
        assert_eq!(shown[0].parents, ["rel1", "f3"], "the pull request merge is untouched");
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

    // ---- author and date

    fn by(id: &str, parents: &[&str], author: &str, email: &str, date: &str) -> Commit {
        Commit { author: author.into(), email: email.into(), date: date.into(), ..commit(id, parents, &[]) }
    }

    const TODAY: Day = (2026, 10, 7);

    #[test]
    fn day_arithmetic_crosses_months_years_and_leap_days() {
        assert_eq!(add_days((2026, 10, 7), -6), (2026, 10, 1));
        assert_eq!(add_days((2026, 10, 1), -1), (2026, 9, 30));
        assert_eq!(add_days((2026, 1, 1), -1), (2025, 12, 31));
        assert_eq!(add_days((2024, 2, 28), 1), (2024, 2, 29));
        assert_eq!(add_days((2025, 2, 28), 1), (2025, 3, 1));
        assert_eq!(day_of(0, 0), (1970, 1, 1));
        assert_eq!(day_of(1_700_000_000, 0), (2023, 11, 14));
        assert_eq!(day_of(1_700_000_000, 7 * 3600), (2023, 11, 15), "past midnight in UTC+7");
    }

    #[test]
    fn a_commit_date_reads_back_as_a_day() {
        assert_eq!(commit_day(&by("a", &[], "A", "a@x", "7 Oct 2026 15:32")), Some((2026, 10, 7)));
        assert_eq!(commit_day(&by("a", &[], "A", "a@x", "21 Feb 2025 09:05")), Some((2025, 2, 21)));
        assert_eq!(commit_day(&by("a", &[], "A", "a@x", "")), None);
        assert_eq!(commit_day(&by("a", &[], "A", "a@x", "7 Smarch 2026 15:32")), None);
    }

    #[test]
    fn search_words_for_author_and_date_are_taken_out_and_the_rest_is_kept() {
        let (c, rest) = parse_constraints("fix author:ada date:2026-10-01..2026-10-05 login", TODAY);
        assert_eq!(c.authors, ["ada"]);
        assert_eq!((c.since, c.until), (Some((2026, 10, 1)), Some((2026, 10, 5))));
        assert_eq!(rest, "fix login");

        let (c, rest) = parse_constraints("author:\"Ada Lovelace\" author:bob@x.io", TODAY);
        assert_eq!(c.authors, ["ada lovelace", "bob@x.io"]);
        assert_eq!(rest, "");

        // Open ends, a whole month, relative words, and half-typed words that constrain nothing.
        assert_eq!(parse_constraints("date:2026-10-03..", TODAY).0.since, Some((2026, 10, 3)));
        assert_eq!(parse_constraints("date:2026-10-03..", TODAY).0.until, None);
        assert_eq!(parse_constraints("date:..2026-10-03", TODAY).0.until, Some((2026, 10, 3)));
        let month = parse_constraints("date:2026-02", TODAY).0;
        assert_eq!((month.since, month.until), (Some((2026, 2, 1)), Some((2026, 2, 28))));
        let (today, yesterday) = (parse_constraints("date:today", TODAY).0, parse_constraints("date:yesterday", TODAY).0);
        assert_eq!((today.since, today.until), (Some(TODAY), Some(TODAY)));
        assert_eq!(yesterday.since, Some((2026, 10, 6)));
        let week = parse_constraints("date:7d", TODAY).0;
        assert_eq!((week.since, week.until), (Some((2026, 10, 1)), Some(TODAY)));
        assert_eq!(parse_constraints("date:month", TODAY).0.since, Some((2026, 10, 1)));
        for half_typed in ["date:", "date:2026-1", "date:2026-13-01", "date:2026-02-30", "date:soon", "author:", "since:x"] {
            let (c, rest) = parse_constraints(half_typed, TODAY);
            assert!(c.is_empty(), "{half_typed:?} constrains nothing");
            assert_eq!(rest, "", "and is not left in the text");
        }
        assert_eq!(parse_constraints("path:src date:today", TODAY).1, "path:src");
    }

    #[test]
    fn constraints_match_by_author_name_or_email_and_by_day() {
        let ada = by("a", &[], "Ada Lovelace", "Ada@Example.com", "5 Oct 2026 10:00");
        let bob = by("b", &[], "Bob", "bob@example.com", "20 Sep 2026 10:00");
        let nodate = by("c", &[], "Ada", "ada@example.com", "");
        let only = |text: &str| parse_constraints(text, TODAY).0;
        assert!(only("author:lovelace").matches(&ada) && !only("author:lovelace").matches(&bob));
        assert!(only("author:ada@example.com").matches(&ada), "an email matches whole, whatever its case");
        assert!(only("author:ada author:bob").matches(&bob), "any of them");
        assert!(only("date:week").matches(&ada) && !only("date:week").matches(&bob));
        assert!(only("date:2026-09").matches(&bob) && !only("date:2026-09").matches(&ada));
        assert!(only("since:2026-10-05 until:2026-10-05").matches(&ada), "both ends are included");
        assert!(!only("date:2026-10").matches(&nodate), "a commit with no date is not in a range");
        assert!(only("").matches(&nodate), "no constraint, no filter");
        assert!(only("author:ada date:month").matches(&ada) && !only("author:bob date:month").matches(&ada));
    }

    #[test]
    fn replacing_a_search_word_keeps_the_rest_of_the_search() {
        assert_eq!(with_term("fix author:ada", "author", Some("bob")), "fix author:bob");
        assert_eq!(with_term("fix author:ada date:7d", "author", None), "fix date:7d");
        assert_eq!(with_term("", "date", Some("today")), "date:today");
        assert_eq!(with_term("login", "author", Some("Ada Lovelace")), "login author:\"Ada Lovelace\"");
        assert_eq!(with_term("AUTHOR:ada x", "author", Some("bob")), "x author:bob", "any case of the key");
        assert_eq!(term("x author:\"Ada Lovelace\" date:7d", "author").as_deref(), Some("Ada Lovelace"));
        assert_eq!(term("x date:7d", "date").as_deref(), Some("7d"));
        assert_eq!(term("x date:", "date"), None);
        assert_eq!(term("x", "author"), None);
    }

    #[test]
    fn narrowing_to_some_commits_keeps_the_graph_connected() {
        // Newest first:  m (merge of f2 and r1) ← f2 ← f1 ← base;  r1 ← base.
        let log = vec![
            by("m", &["r1", "f2"], "Ada", "ada@x", "7 Oct 2026 10:00"),
            by("f2", &["f1"], "Bob", "bob@x", "6 Oct 2026 10:00"),
            by("r1", &["base"], "Ada", "ada@x", "5 Oct 2026 10:00"),
            by("f1", &["base"], "Bob", "bob@x", "4 Oct 2026 10:00"),
            by("base", &[], "Ada", "ada@x", "1 Oct 2026 10:00"),
        ];
        let ada = parse_constraints("author:ada", TODAY).0;
        let shown = narrow(&log, |c| ada.matches(c));
        assert_eq!(ids(&shown), ["m", "r1", "base"]);
        // The merge's second parent (f2, Bob's) is gone; what it led to (base, through f1) is the nearest of Ada's.
        let parents = |id: &str| shown.iter().find(|c| c.id == id).unwrap().parents.clone();
        assert_eq!(parents("m"), ["r1", "base"]);
        assert_eq!(parents("r1"), ["base"]);
        assert!(parents("base").is_empty());
        // Every parent that is left is a commit that is shown, so the layout has nothing dangling.
        for commit in &shown {
            assert!(commit.parents.iter().all(|p| shown.iter().any(|c| &c.id == p)), "{commit:?}");
        }
        // Nothing passes: nothing is shown. Everything passes: nothing changes.
        assert!(narrow(&log, |_| false).is_empty());
        assert_eq!(narrow(&log, |_| true), log);
    }
}
