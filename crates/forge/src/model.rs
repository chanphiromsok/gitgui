//! What a repository's page lists, read from GitHub's JSON.

use serde_json::Value;

use crate::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pull {
    pub number: u32,
    pub title: String,
    pub state: PullState,
    pub author: String,
    /// The branch it comes from, and the one it goes into.
    pub head: String,
    pub base: String,
    /// The branch is in this repository (not a fork), so it can be checked out from the remote.
    pub same_repo: bool,
    /// When it last changed, seconds since 1970.
    pub updated: u64,
    pub url: String,
    pub body: String,
    pub labels: Vec<String>,
    /// People whose review is asked for.
    pub reviewers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub number: u32,
    pub title: String,
    pub open: bool,
    pub author: String,
    pub updated: u64,
    pub url: String,
    pub body: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub comments: u32,
}

/// Where the GitHub App is installed for the person: an account or organization, and which repositories of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installation {
    pub account: String,
    pub app_slug: String,
    /// Every repository of the account, or only chosen ones.
    pub all_repositories: bool,
}

/// `2026-10-07T09:20:00Z` as seconds since 1970; `None` for anything else.
pub fn iso_to_unix(text: &str) -> Option<u64> {
    let (date, time) = text.split_once('T')?;
    let mut d = date.split('-').map(|part| part.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.trim_end_matches('Z').split(':').map(|part| part.split('.').next().and_then(|p| p.parse::<i64>().ok()));
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next().flatten().unwrap_or(0));
    // Howard Hinnant's days-from-civil.
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3_600 + mm * 60 + ss).ok()
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_owned()
}

fn names(v: &Value, key: &str, field: &str) -> Vec<String> {
    v.get(key).and_then(Value::as_array).map(|list| list.iter().filter_map(|item| item.get(field).and_then(Value::as_str)).map(str::to_owned).collect()).unwrap_or_default()
}

fn login(v: &Value, key: &str) -> String {
    v.get(key).and_then(|user| user.get("login")).and_then(Value::as_str).unwrap_or("ghost").to_owned()
}

fn list(body: &str) -> Result<Vec<Value>, Error> {
    match serde_json::from_str::<Value>(body).map_err(|err| Error::Parse(err.to_string()))? {
        Value::Array(items) => Ok(items),
        other => Err(Error::Parse(format!("a list was expected, got {}", other.to_string().chars().take(80).collect::<String>()))),
    }
}

/// The pull requests of GitHub's `/pulls` answer. `repo` is the `owner/name` they were asked of, to tell a branch of
/// this repository from a fork's.
pub fn parse_pulls(body: &str, repo: &str) -> Result<Vec<Pull>, Error> {
    Ok(list(body)?
        .iter()
        .filter_map(|item| {
            let number = u32::try_from(item.get("number")?.as_u64()?).ok()?;
            let merged = item.get("merged_at").is_some_and(|v| !v.is_null());
            let closed = item.get("state").and_then(Value::as_str) == Some("closed");
            let draft = item.get("draft").and_then(Value::as_bool).unwrap_or(false);
            let state = match (merged, closed, draft) {
                (true, _, _) => PullState::Merged,
                (_, true, _) => PullState::Closed,
                (_, _, true) => PullState::Draft,
                _ => PullState::Open,
            };
            let head = item.get("head").unwrap_or(&Value::Null);
            let base = item.get("base").unwrap_or(&Value::Null);
            let head_repo = head.get("repo").and_then(|r| r.get("full_name")).and_then(Value::as_str);
            Some(Pull {
                number,
                title: text(item, "title"),
                state,
                author: login(item, "user"),
                head: text(head, "ref"),
                base: text(base, "ref"),
                same_repo: head_repo.is_some_and(|name| name.eq_ignore_ascii_case(repo)),
                updated: item.get("updated_at").and_then(Value::as_str).and_then(iso_to_unix).unwrap_or(0),
                url: text(item, "html_url"),
                body: text(item, "body"),
                labels: names(item, "labels", "name"),
                reviewers: names(item, "requested_reviewers", "login"),
            })
        })
        .collect())
}

/// The issues of GitHub's `/issues` answer, which also lists pull requests; those are left out.
pub fn parse_issues(body: &str) -> Result<Vec<Issue>, Error> {
    Ok(list(body)?
        .iter()
        .filter(|item| item.get("pull_request").is_none())
        .filter_map(|item| {
            Some(Issue {
                number: u32::try_from(item.get("number")?.as_u64()?).ok()?,
                title: text(item, "title"),
                open: item.get("state").and_then(Value::as_str) != Some("closed"),
                author: login(item, "user"),
                updated: item.get("updated_at").and_then(Value::as_str).and_then(iso_to_unix).unwrap_or(0),
                url: text(item, "html_url"),
                body: text(item, "body"),
                labels: names(item, "labels", "name"),
                assignees: names(item, "assignees", "login"),
                comments: item.get("comments").and_then(Value::as_u64).unwrap_or(0) as u32,
            })
        })
        .collect())
}

/// The installations of GitHub's `/user/installations` answer.
pub fn parse_installations(body: &str) -> Result<Vec<Installation>, Error> {
    let answer: Value = serde_json::from_str(body).map_err(|err| Error::Parse(err.to_string()))?;
    Ok(answer
        .get("installations")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| Installation {
                    account: login(item, "account"),
                    app_slug: text(item, "app_slug"),
                    all_repositories: item.get("repository_selection").and_then(Value::as_str) == Some("all"),
                })
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_from_github_is_read_to_the_second() {
        assert_eq!(iso_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_to_unix("2026-10-07T09:20:05Z"), Some(1_791_364_805));
        assert_eq!(iso_to_unix("2024-02-29T23:59:59.123Z"), Some(1_709_251_199));
        assert_eq!(iso_to_unix("yesterday"), None);
    }

    const PULLS: &str = r#"[
      {"number":58,"state":"open","draft":false,"title":"chore: remove unused libs","user":{"login":"rom"},"html_url":"https://github.com/o/r/pull/58",
       "head":{"ref":"chore/remove-unused-libs","repo":{"full_name":"o/r"}},"base":{"ref":"release/1.0.0"},"updated_at":"2026-10-08T01:00:00Z",
       "body":"Drops three libraries.","labels":[{"name":"cleanup"}],"requested_reviewers":[{"login":"kim"}],"merged_at":null},
      {"number":49,"state":"open","draft":true,"title":"feat: pick-up hooks","user":{"login":"kim"},"html_url":"u",
       "head":{"ref":"feat/pickup","repo":{"full_name":"someone/r"}},"base":{"ref":"main"},"updated_at":"2026-10-04T10:00:00Z","merged_at":null},
      {"number":40,"state":"closed","draft":false,"title":"done","user":null,"html_url":"u","head":{"ref":"x","repo":null},"base":{"ref":"main"},
       "updated_at":"2026-10-01T10:00:00Z","merged_at":"2026-10-01T10:00:00Z"}
    ]"#;

    #[test]
    fn pull_requests_are_read_with_their_state_and_whether_the_branch_is_here() {
        let pulls = parse_pulls(PULLS, "o/r").unwrap();
        assert_eq!(pulls.len(), 3);
        assert_eq!((pulls[0].state, pulls[0].same_repo, pulls[0].reviewers.as_slice()), (PullState::Open, true, ["kim".to_owned()].as_slice()));
        assert_eq!((pulls[1].state, pulls[1].same_repo), (PullState::Draft, false), "a fork's branch");
        assert_eq!((pulls[2].state, pulls[2].author.as_str(), pulls[2].same_repo), (PullState::Merged, "ghost", false), "a deleted user and fork");
        assert!(parse_pulls("{\"message\":\"Not Found\"}", "o/r").is_err());
    }

    #[test]
    fn issues_leave_out_the_pull_requests_that_github_lists_with_them() {
        let body = r#"[
          {"number":101,"state":"open","title":"driver reporting v2","user":{"login":"rom"},"html_url":"u","labels":[{"name":"feature"}],"assignees":[{"login":"kim"}],"comments":3,"updated_at":"2026-10-07T00:00:00Z"},
          {"number":58,"state":"open","title":"a pull request","user":{"login":"rom"},"html_url":"u","pull_request":{"url":"x"},"updated_at":"2026-10-07T00:00:00Z"}
        ]"#;
        let issues = parse_issues(body).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!((issues[0].number, issues[0].comments, issues[0].open), (101, 3, true));
    }

    #[test]
    fn installations_say_where_the_app_is() {
        let body = r#"{"total_count":1,"installations":[{"app_slug":"gitgui-x","repository_selection":"selected","account":{"login":"acme"}}]}"#;
        assert_eq!(parse_installations(body).unwrap(), [Installation { account: "acme".into(), app_slug: "gitgui-x".into(), all_repositories: false }]);
    }
}
