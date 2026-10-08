//! GitHub's device flow: ask for a code, show it, and wait for the person to approve it on github.com.

use std::fmt;

use serde_json::Value;

use crate::token::Token;
use crate::transport::{Method, Request, Transport};
use crate::Error;

/// What GitHub gives for the person to type, and for the program to wait on.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceCode {
    /// The key the program polls with; not shown to anyone.
    pub device_code: String,
    /// What the person types, like `6B82-EDC6`.
    pub user_code: String,
    /// Where to type it: `https://github.com/login/device`.
    pub verification_uri: String,
    /// Seconds the code is good for.
    pub expires_in: u64,
    /// Seconds to wait between asks.
    pub interval: u64,
}

impl fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceCode").field("user_code", &self.user_code).finish_non_exhaustive()
    }
}

/// How an ask for the token went.
#[derive(Debug, PartialEq, Eq)]
pub enum Poll {
    /// Not approved yet: ask again after the interval.
    Pending,
    /// Asked too soon: wait this many seconds between asks from now on.
    SlowDown { interval: u64 },
    Done(Token),
}

fn headers() -> Vec<(String, String)> {
    vec![("Accept".into(), "application/json".into()), ("User-Agent".into(), "gitgui".into())]
}

fn json(body: &str) -> Result<Value, Error> {
    serde_json::from_str(body).map_err(|err| Error::Parse(err.to_string()))
}

/// Asks GitHub for a code to show.
pub fn request_code(transport: &dyn Transport, client_id: &str) -> Result<DeviceCode, Error> {
    let request = Request {
        method: Method::Post,
        url: "https://github.com/login/device/code".into(),
        headers: headers(),
        form: vec![("client_id".into(), client_id.into())],
    };
    let response = transport.send(&request)?;
    let answer = json(&response.body)?;
    if let Some(error) = answer.get("error").and_then(Value::as_str) {
        return Err(Error::Other { status: response.status, message: error.to_owned() });
    }
    let text = |name: &str| answer.get(name).and_then(Value::as_str).map(str::to_owned);
    let number = |name: &str| answer.get(name).and_then(Value::as_u64);
    Ok(DeviceCode {
        device_code: text("device_code").ok_or_else(|| Error::Parse("no device_code".into()))?,
        user_code: text("user_code").ok_or_else(|| Error::Parse("no user_code".into()))?,
        verification_uri: text("verification_uri").unwrap_or_else(|| "https://github.com/login/device".into()),
        expires_in: number("expires_in").unwrap_or(900),
        interval: number("interval").unwrap_or(5).max(1),
    })
}

/// Asks once whether the code has been approved. `now` is when the answer arrives (for the token's end time).
pub fn poll(transport: &dyn Transport, client_id: &str, code: &DeviceCode, now: u64) -> Result<Poll, Error> {
    let request = Request {
        method: Method::Post,
        url: "https://github.com/login/oauth/access_token".into(),
        headers: headers(),
        form: vec![
            ("client_id".into(), client_id.into()),
            ("device_code".into(), code.device_code.clone()),
            ("grant_type".into(), "urn:ietf:params:oauth:grant-type:device_code".into()),
        ],
    };
    let answer = json(&transport.send(&request)?.body)?;
    if let Some(token) = Token::from_json(&answer, now) {
        return Ok(Poll::Done(token));
    }
    match answer.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown { interval: answer.get("interval").and_then(Value::as_u64).unwrap_or(code.interval + 5) }),
        Some("expired_token") => Err(Error::Expired),
        Some("access_denied") => Err(Error::Denied),
        Some(other) => Err(Error::Other { status: 200, message: other.to_owned() }),
        None => Err(Error::Parse("neither a token nor an error".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Response;

    struct Answer(&'static str);
    impl Transport for Answer {
        fn send(&self, _: &Request) -> Result<Response, Error> {
            Ok(Response { status: 200, body: self.0.to_owned(), ..Default::default() })
        }
    }

    fn code() -> DeviceCode {
        DeviceCode { device_code: "dc".into(), user_code: "AAAA-BBBB".into(), verification_uri: "https://github.com/login/device".into(), expires_in: 900, interval: 5 }
    }

    #[test]
    fn the_code_to_type_is_read_from_githubs_answer() {
        let answer = Answer(r#"{"device_code":"3584d83530557fdd1f46af8289938c8ef79f9dc5","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#);
        let code = request_code(&answer, "id").unwrap();
        assert_eq!((code.user_code.as_str(), code.expires_in, code.interval), ("WDJB-MJHT", 900, 5));
        assert!(!format!("{code:?}").contains("3584d835"), "the polling key is not printed");
        assert!(matches!(request_code(&Answer(r#"{"error":"device_flow_disabled"}"#), "id"), Err(Error::Other { .. })));
    }

    #[test]
    fn polling_tells_pending_slow_down_done_denied_and_expired_apart() {
        let poll_with = |body: &'static str| poll(&Answer(body), "id", &code(), 100);
        assert_eq!(poll_with(r#"{"error":"authorization_pending"}"#), Ok(Poll::Pending));
        assert_eq!(poll_with(r#"{"error":"slow_down","interval":10}"#), Ok(Poll::SlowDown { interval: 10 }));
        assert_eq!(poll_with(r#"{"error":"access_denied"}"#), Err(Error::Denied));
        assert_eq!(poll_with(r#"{"error":"expired_token"}"#), Err(Error::Expired));
        let Ok(Poll::Done(token)) = poll_with(r#"{"access_token":"ghu_x","expires_in":28800,"refresh_token":"ghr_y","refresh_token_expires_in":15897600}"#) else {
            panic!("a token")
        };
        assert_eq!(token.expires_at, Some(28_900));
    }
}
