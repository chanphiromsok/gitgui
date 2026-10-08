//! The token GitHub hands over after the device flow, and how it is kept fresh.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transport::{Method, Request, Transport};
use crate::Error;

/// A user access token for the GitHub App. It expires after eight hours; the refresh token lasts six months.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub access: String,
    pub refresh: Option<String>,
    /// Seconds since 1970 when `access` stops working; `None` for a token that does not expire.
    pub expires_at: Option<u64>,
    pub refresh_expires_at: Option<u64>,
}

/// A token is a secret: it never shows in a log or a panic message.
impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token").field("expires_at", &self.expires_at).finish_non_exhaustive()
    }
}

/// Refresh a little before the end, so a request does not go out with a token that dies on the way.
const MARGIN: u64 = 120;

impl Token {
    /// From GitHub's answer to the device flow or to a refresh; `now` is when the answer came.
    pub fn from_json(answer: &Value, now: u64) -> Option<Token> {
        let access = answer.get("access_token")?.as_str()?.to_owned();
        let seconds = |name: &str| answer.get(name).and_then(Value::as_u64);
        Some(Token {
            access,
            refresh: answer.get("refresh_token").and_then(Value::as_str).map(str::to_owned),
            expires_at: seconds("expires_in").map(|s| now + s),
            refresh_expires_at: seconds("refresh_token_expires_in").map(|s| now + s),
        })
    }

    /// The access token is over, or nearly.
    pub fn needs_refresh(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|end| now + MARGIN >= end)
    }

    /// There is a refresh token that is still good.
    pub fn can_refresh(&self, now: u64) -> bool {
        self.refresh.is_some() && self.refresh_expires_at.is_none_or(|end| now < end)
    }
}

/// Trades the refresh token for a new pair. GitHub makes the old refresh token useless once it has been used, so the
/// caller must keep the answer. A device-flow token needs no client secret to be refreshed.
pub fn refresh(transport: &dyn Transport, client_id: &str, token: &Token, now: u64) -> Result<Token, Error> {
    let Some(refresh) = token.refresh.as_deref().filter(|_| token.can_refresh(now)) else { return Err(Error::SignedOut) };
    let request = Request {
        method: Method::Post,
        url: "https://github.com/login/oauth/access_token".into(),
        headers: vec![("Accept".into(), "application/json".into()), ("User-Agent".into(), "gitgui".into())],
        form: vec![
            ("client_id".into(), client_id.into()),
            ("grant_type".into(), "refresh_token".into()),
            ("refresh_token".into(), refresh.into()),
        ],
    };
    let response = transport.send(&request)?;
    let answer: Value = serde_json::from_str(&response.body).map_err(|err| Error::Parse(err.to_string()))?;
    // `bad_refresh_token` and the like: the sign-in cannot be saved, so it is over.
    Token::from_json(&answer, now).ok_or(Error::SignedOut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_reads_its_times_from_the_answer() {
        let answer = serde_json::json!({
            "access_token": "ghu_abc", "expires_in": 28800, "refresh_token": "ghr_def", "refresh_token_expires_in": 15897600, "token_type": "bearer"
        });
        let token = Token::from_json(&answer, 1_000).unwrap();
        assert_eq!((token.expires_at, token.refresh_expires_at), (Some(29_800), Some(15_898_600)));
        assert!(!token.needs_refresh(1_000) && token.needs_refresh(29_700), "refreshed two minutes early");
        assert!(token.can_refresh(1_000) && !token.can_refresh(16_000_000));
        assert!(Token::from_json(&serde_json::json!({"error": "bad_verification_code"}), 0).is_none());
    }

    #[test]
    fn a_token_never_shows_itself() {
        let token = Token { access: "ghu_secret".into(), refresh: Some("ghr_secret".into()), expires_at: None, refresh_expires_at: None };
        let shown = format!("{token:?}");
        assert!(!shown.contains("secret"), "{shown}");
    }

    struct Answer(&'static str);
    impl Transport for Answer {
        fn send(&self, _: &Request) -> Result<crate::Response, Error> {
            Ok(crate::Response { status: 200, body: self.0.to_owned(), ..Default::default() })
        }
    }

    #[test]
    fn a_refresh_gives_a_new_pair_and_a_dead_refresh_token_ends_the_sign_in() {
        let old = Token { access: "old".into(), refresh: Some("r1".into()), expires_at: Some(10), refresh_expires_at: Some(100_000) };
        let ok = Answer(r#"{"access_token":"new","expires_in":28800,"refresh_token":"r2","refresh_token_expires_in":15897600}"#);
        let fresh = refresh(&ok, "id", &old, 50).unwrap();
        assert_eq!((fresh.access.as_str(), fresh.refresh.as_deref()), ("new", Some("r2")));
        let dead = Answer(r#"{"error":"bad_refresh_token"}"#);
        assert_eq!(refresh(&dead, "id", &old, 50), Err(Error::SignedOut));
        // A refresh token past its end is not even tried.
        assert_eq!(refresh(&ok, "id", &old, 200_000), Err(Error::SignedOut));
    }
}
