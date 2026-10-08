//! The network, behind a trait.

use std::fmt;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use crate::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// One request. A header called `Authorization` is never printed.
#[derive(Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// For a POST: the fields of the form.
    pub form: Vec<(String, String)>,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("Request").field("method", &self.method).field("url", &self.url).field("headers", &headers).finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
    /// GitHub's `x-ratelimit-remaining` and `x-ratelimit-reset`, when it sent them.
    pub rate_remaining: Option<u64>,
    pub rate_reset: Option<u64>,
}

pub trait Transport {
    fn send(&self, request: &Request) -> Result<Response, Error>;
}

/// The real thing: HTTPS with a timeout, and an error status is an answer, not a failure.
pub struct Http;

fn form_encode(fields: &[(String, String)]) -> String {
    fn escape(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for byte in text.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        out
    }
    fields.iter().map(|(name, value)| format!("{}={}", escape(name), escape(value))).collect::<Vec<_>>().join("&")
}

impl Transport for Http {
    fn send(&self, request: &Request) -> Result<Response, Error> {
        static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
        let agent = AGENT.get_or_init(|| {
            ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).http_status_as_error(false).build().into()
        });
        let result = match request.method {
            Method::Get => {
                let mut builder = agent.get(&request.url);
                for (name, value) in &request.headers {
                    builder = builder.header(name.as_str(), value.as_str());
                }
                builder.call()
            }
            Method::Post => {
                let mut builder = agent.post(&request.url);
                for (name, value) in &request.headers {
                    builder = builder.header(name.as_str(), value.as_str());
                }
                builder.header("Content-Type", "application/x-www-form-urlencoded").send(form_encode(&request.form))
            }
        };
        let mut response = result.map_err(|err| Error::Network(err.to_string()))?;
        let number = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
        let (rate_remaining, rate_reset) = (number("x-ratelimit-remaining"), number("x-ratelimit-reset"));
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_string().map_err(|err| Error::Network(err.to_string()))?;
        Ok(Response { status, body, rate_remaining, rate_reset })
    }
}

/// Answers from files in a folder, for looking at the interface without signing in and for tests:
/// `user.json`, `pulls.json`, `pulls_closed.json`, `issues.json`, `issues_closed.json`, `installations.json`.
pub struct Fixture(pub PathBuf);

impl Transport for Fixture {
    fn send(&self, request: &Request) -> Result<Response, Error> {
        let url = request.url.as_str();
        let closed = url.contains("state=closed");
        let file = if url.contains("/user/installations") {
            "installations.json"
        } else if url.contains("/pulls") {
            if closed { "pulls_closed.json" } else { "pulls.json" }
        } else if url.contains("/issues") {
            if closed { "issues_closed.json" } else { "issues.json" }
        } else if url.ends_with("/user") {
            "user.json"
        } else {
            return Ok(Response { status: 404, body: "{\"message\":\"Not Found\"}".into(), ..Response::default() });
        };
        let empty = if file == "user.json" { "{\"login\":\"you\"}" } else if file == "installations.json" { "{\"installations\":[]}" } else { "[]" };
        let body = std::fs::read_to_string(self.0.join(file)).unwrap_or_else(|_| empty.to_owned());
        Ok(Response { status: 200, body, ..Response::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_values_are_escaped() {
        let fields = [("client_id".to_owned(), "Iv23".to_owned()), ("grant_type".to_owned(), "urn:ietf:params:oauth:grant-type:device_code".to_owned())];
        assert_eq!(form_encode(&fields), "client_id=Iv23&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code");
    }

    #[test]
    fn a_request_never_prints_its_authorization() {
        let request = Request {
            method: Method::Get,
            url: "https://api.github.com/user".into(),
            headers: vec![("Authorization".into(), "Bearer ghu_secret".into())],
            form: Vec::new(),
        };
        assert!(!format!("{request:?}").contains("ghu_secret"));
    }
}
