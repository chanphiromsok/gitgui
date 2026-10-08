//! Signing in to GitHub and reading what a repository's page lists: its pull requests and issues.
//!
//! The sign-in is GitHub's *device flow* for a GitHub App: the program shows a short code, the person types it at
//! `github.com/login/device` and approves there, and the program is handed a token. No password passes through
//! here and no secret is built in; only the app's public client id. The token is short-lived (8 hours) and comes
//! with a refresh token, and what it can read is what the app was installed on and given: pull requests and
//! issues, read-only. Nothing in this crate writes to GitHub.
//!
//! Everything that touches the network goes through [`Transport`], so the rest runs on canned answers in tests.

pub mod api;
pub mod device;
pub mod model;
pub mod token;
pub mod transport;
pub mod vault;

use std::fmt;

pub use api::Api;
pub use device::{DeviceCode, Poll};
pub use model::{Installation, Issue, Pull, PullState};
pub use token::Token;
pub use transport::{Fixture, Http, Method, Request, Response, Transport};

/// The public client id of the GitHub App that gitgui signs in with. It is not a secret.
pub const CLIENT_ID: &str = "Iv23li31SzwbW248deun";

/// The name of the GitHub App in its web address (`github.com/apps/<name>`), for the "Install on GitHub" button.
/// Empty until it is filled in; the button then opens the page where a person's installed apps are listed.
pub const APP_SLUG: &str = "";

/// Where to install the GitHub App on an account or organization.
pub fn install_url() -> String {
    if APP_SLUG.is_empty() {
        "https://github.com/settings/installations".to_owned()
    } else {
        format!("https://github.com/apps/{APP_SLUG}/installations/new")
    }
}

/// What can go wrong, in words a person can be told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// GitHub could not be reached.
    Network(String),
    /// GitHub answered with something that is not what was asked for.
    Parse(String),
    /// The token is no longer good (expired or taken back): sign in again.
    SignedOut,
    /// The person said no on github.com.
    Denied,
    /// The code was not entered in time.
    Expired,
    /// The app cannot see this repository: it is not installed there, or the repository is not the person's to see.
    NoAccess,
    /// Too many requests; GitHub says when to come back (seconds since 1970).
    RateLimited { resets_at: Option<u64> },
    /// Any other answer GitHub gave.
    Other { status: u16, message: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Network(why) => write!(f, "Couldn't reach GitHub: {why}"),
            Error::Parse(what) => write!(f, "GitHub's answer wasn't what was expected: {what}"),
            Error::SignedOut => write!(f, "Your GitHub sign-in has ended. Sign in again."),
            Error::Denied => write!(f, "GitHub sign-in was refused."),
            Error::Expired => write!(f, "The code ran out before it was entered. Start again."),
            Error::NoAccess => write!(f, "gitgui can't see this repository: the GitHub app isn't installed on it."),
            Error::RateLimited { .. } => write!(f, "GitHub says too many requests were made. Try again in a while."),
            Error::Other { status, message } => write!(f, "GitHub said {status}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// Seconds since 1970, now.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
