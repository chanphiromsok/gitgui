//! Reading GitHub's REST API with a user token. Reads only.

use crate::model::{Installation, Issue, Pull, parse_installations, parse_issues, parse_pulls};
use crate::transport::{Method, Request, Response, Transport};
use crate::Error;

pub struct Api<'a> {
    transport: &'a dyn Transport,
    token: &'a str,
}

/// The most items asked for at once; a longer list is cut here (GitHub's own page limit).
const PAGE: u32 = 100;
/// How many closed or merged ones come along with the open ones.
const CLOSED: u32 = 25;

impl<'a> Api<'a> {
    pub fn new(transport: &'a dyn Transport, token: &'a str) -> Self {
        Self { transport, token }
    }

    fn get(&self, path: &str) -> Result<Response, Error> {
        let request = Request {
            method: Method::Get,
            url: format!("https://api.github.com{path}"),
            headers: vec![
                ("Authorization".into(), format!("Bearer {}", self.token)),
                ("Accept".into(), "application/vnd.github+json".into()),
                ("X-GitHub-Api-Version".into(), "2022-11-28".into()),
                ("User-Agent".into(), "gitgui".into()),
            ],
            form: Vec::new(),
        };
        let response = self.transport.send(&request)?;
        match response.status {
            200..=299 => Ok(response),
            401 => Err(Error::SignedOut),
            403 if response.rate_remaining == Some(0) => Err(Error::RateLimited { resets_at: response.rate_reset }),
            // A private repository the app cannot see looks the same as one that is not there.
            403 | 404 => Err(Error::NoAccess),
            status => {
                let message = serde_json::from_str::<serde_json::Value>(&response.body)
                    .ok()
                    .and_then(|v| v.get("message").and_then(|m| m.as_str().map(str::to_owned)))
                    .unwrap_or_default();
                Err(Error::Other { status, message })
            }
        }
    }

    /// The login of the person who is signed in.
    pub fn user(&self) -> Result<String, Error> {
        let body = self.get("/user")?.body;
        serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("login").and_then(|l| l.as_str().map(str::to_owned)))
            .ok_or_else(|| Error::Parse("no login".into()))
    }

    /// Open pull requests, then the latest closed and merged ones, newest change first.
    pub fn pulls(&self, owner: &str, repo: &str) -> Result<Vec<Pull>, Error> {
        let full = format!("{owner}/{repo}");
        let mut all = parse_pulls(&self.get(&format!("/repos/{full}/pulls?state=open&sort=updated&direction=desc&per_page={PAGE}"))?.body, &full)?;
        all.extend(parse_pulls(&self.get(&format!("/repos/{full}/pulls?state=closed&sort=updated&direction=desc&per_page={CLOSED}"))?.body, &full)?);
        Ok(all)
    }

    /// Open issues, then the latest closed ones.
    pub fn issues(&self, owner: &str, repo: &str) -> Result<Vec<Issue>, Error> {
        let full = format!("{owner}/{repo}");
        let mut all = parse_issues(&self.get(&format!("/repos/{full}/issues?state=open&sort=updated&direction=desc&per_page={PAGE}"))?.body)?;
        all.extend(parse_issues(&self.get(&format!("/repos/{full}/issues?state=closed&sort=updated&direction=desc&per_page={CLOSED}"))?.body)?);
        Ok(all)
    }

    /// Where the app is installed for this person.
    pub fn installations(&self) -> Result<Vec<Installation>, Error> {
        parse_installations(&self.get("/user/installations?per_page=100")?.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Answers each request with the next canned response and remembers what was asked.
    struct Script {
        answers: RefCell<Vec<Response>>,
        asked: RefCell<Vec<Request>>,
    }

    impl Transport for Script {
        fn send(&self, request: &Request) -> Result<Response, Error> {
            self.asked.borrow_mut().push(request.clone());
            Ok(self.answers.borrow_mut().remove(0))
        }
    }

    fn ok(body: &str) -> Response {
        Response { status: 200, body: body.to_owned(), ..Default::default() }
    }

    fn script(answers: Vec<Response>) -> Script {
        Script { answers: RefCell::new(answers), asked: RefCell::new(Vec::new()) }
    }

    #[test]
    fn a_read_sends_the_token_and_nothing_but_gets() {
        let transport = script(vec![ok(r#"{"login":"rom"}"#)]);
        assert_eq!(Api::new(&transport, "ghu_t").user().unwrap(), "rom");
        let asked = transport.asked.borrow();
        assert_eq!(asked[0].method, Method::Get);
        assert!(asked[0].headers.iter().any(|(n, v)| n == "Authorization" && v == "Bearer ghu_t"));
        assert_eq!(asked[0].url, "https://api.github.com/user");
    }

    #[test]
    fn pull_requests_come_as_open_ones_then_closed_ones() {
        let open = r#"[{"number":1,"state":"open","title":"a","user":{"login":"x"},"head":{"ref":"h"},"base":{"ref":"main"}}]"#;
        let closed = r#"[{"number":2,"state":"closed","title":"b","user":{"login":"x"},"head":{"ref":"h2"},"base":{"ref":"main"},"merged_at":"2026-01-01T00:00:00Z"}]"#;
        let transport = script(vec![ok(open), ok(closed)]);
        let pulls = Api::new(&transport, "t").pulls("o", "r").unwrap();
        assert_eq!(pulls.iter().map(|p| p.number).collect::<Vec<_>>(), [1, 2]);
        let asked = transport.asked.borrow();
        assert!(asked[0].url.contains("/repos/o/r/pulls?state=open") && asked[1].url.contains("state=closed"));
    }

    #[test]
    fn what_github_refuses_is_told_apart() {
        let refuse = |status: u16, remaining: Option<u64>| {
            let transport = script(vec![Response { status, body: "{}".into(), rate_remaining: remaining, rate_reset: Some(99) }]);
            Api::new(&transport, "t").user().unwrap_err()
        };
        assert_eq!(refuse(401, None), Error::SignedOut);
        assert_eq!(refuse(404, None), Error::NoAccess);
        assert_eq!(refuse(403, Some(0)), Error::RateLimited { resets_at: Some(99) });
        assert_eq!(refuse(403, Some(10)), Error::NoAccess);
        assert!(matches!(refuse(500, None), Error::Other { status: 500, .. }));
    }
}
