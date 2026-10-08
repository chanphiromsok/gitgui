//! Cloning a repository from an address (https, ssh, git) into a folder.
//!
//! An address is checked before git sees it: `git clone` accepts some forms that run programs (`ext::sh -c …`)
//! or that start with a dash and would be read as an option, so only plain web, ssh, git and file addresses
//! go through, and git is told the same through `GIT_ALLOW_PROTOCOL`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::backend::Error;

fn bad(message: impl Into<String>) -> Error {
    Error::Parse(message.into())
}

/// Whether `host` looks like a host name (letters, digits, dots, dashes), not an option or a path.
fn is_host(host: &str) -> bool {
    host.len() >= 2 && !host.starts_with(['-', '.']) && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `Ok` for an address git may be asked to clone: `https://…`, `http://…`, `ssh://…`, `git://…`, `file://…`,
/// or ssh's short form `[user@]host:owner/repo.git`.
pub fn check_url(url: &str) -> Result<(), Error> {
    let url = url.trim();
    if url.is_empty() {
        return Err(bad("Type or paste the address of a repository."));
    }
    if url.starts_with('-') || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad("That is not a repository address: it has spaces or starts with a dash."));
    }
    if let Some((scheme, rest)) = url.split_once("://") {
        return match scheme.to_ascii_lowercase().as_str() {
            "https" | "http" | "ssh" | "git" if rest.split(['/', ':']).next().is_some_and(|h| is_host(h.rsplit('@').next().unwrap_or(h))) => Ok(()),
            "file" if rest.starts_with('/') => Ok(()),
            _ => Err(bad("Use an https://, ssh:// or git@host:owner/repo address.")),
        };
    }
    // The short form: [user@]host:path. A leading `host::` or an empty path is how `ext::` runs programs.
    let after_user = url.rsplit_once('@').map_or(url, |(user, rest)| if user.contains(':') || user.contains('/') { url } else { rest });
    match after_user.split_once(':') {
        Some((host, path)) if is_host(host) && !path.is_empty() && !path.starts_with(':') => Ok(()),
        _ => Err(bad("Use an https://, ssh:// or git@host:owner/repo address.")),
    }
}

/// The folder name a clone of `url` gets: its last path part without `.git`, as git would choose.
pub fn repo_name(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let last = url.rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    let ok = !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '~'));
    ok.then(|| name.to_owned())
}

/// Clones `url` into `parent/name` and returns that folder. Git never asks on a terminal: a private
/// repository needs credentials its helper (or an ssh key in the agent) already has. A folder that
/// already exists is not touched.
pub fn clone(url: &str, parent: &Path, name: &str) -> Result<PathBuf, Error> {
    check_url(url)?;
    if name.is_empty() || name.starts_with(['-', '.']) || name.contains(['/', '\\']) {
        return Err(bad(format!("{name:?} is not a usable folder name.")));
    }
    let dest = parent.join(name);
    if dest.exists() {
        return Err(bad(format!("{} already exists. Open it with Open Folder…, or pick another folder.", dest.display())));
    }
    std::fs::create_dir_all(parent).map_err(Error::Spawn)?;
    let mut command = Command::new("git");
    crate::process::windowless(&mut command);
    command
        .args(["-c", "protocol.ext.allow=never", "clone", "--", url.trim()])
        .arg(&dest)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_ALLOW_PROTOCOL", "https:http:ssh:git:file")
        .stdin(Stdio::null());
    // Fail at once, rather than wait for a password or a yes on a terminal that is not there.
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let output = command.output().map_err(Error::Spawn)?;
    if output.status.success() {
        Ok(dest)
    } else {
        // Git removes what it started; make sure nothing half-made is left behind.
        if dest.join(".git").exists() && !dest.join(".git").join("HEAD").exists() {
            let _ = std::fs::remove_dir_all(&dest);
        }
        Err(Error::Git { status: output.status.code(), stderr: String::from_utf8_lossy(&output.stderr).into_owned() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_ssh_git_and_short_addresses_pass() {
        for ok in [
            "https://github.com/owner/repo.git",
            "https://user@bitbucket.org/owner/repo",
            "http://git.local:8080/team/app.git",
            "ssh://git@host:2222/owner/repo.git",
            "git://example.com/repo.git",
            "git@github.com:owner/repo.git",
            "host.example.com:team/repo",
            "file:///tmp/some/repo",
            "  https://github.com/owner/repo  ",
        ] {
            assert!(check_url(ok).is_ok(), "{ok:?} should pass");
        }
    }

    #[test]
    fn addresses_that_run_programs_or_look_like_options_are_refused() {
        for bad in [
            "",
            "ext::sh -c touch /tmp/x",
            "ext::sh",
            "--upload-pack=touch /tmp/x",
            "-u",
            "https://github.com/owner/repo --config=x",
            "fd::17/repo",
            "/just/a/path",
            "C:\\\\repos\\\\app",
            "ftp://example.com/repo",
            "https://",
            "git@:repo",
        ] {
            assert!(check_url(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn the_folder_name_is_the_last_part_without_dot_git() {
        assert_eq!(repo_name("https://github.com/owner/repo.git").as_deref(), Some("repo"));
        assert_eq!(repo_name("git@github.com:owner/my-app.git").as_deref(), Some("my-app"));
        assert_eq!(repo_name("https://host/owner/repo/").as_deref(), Some("repo"));
        assert_eq!(repo_name("ssh://git@host:2222/team/app").as_deref(), Some("app"));
        assert_eq!(repo_name("host:repo.git").as_deref(), Some("repo"));
        assert_eq!(repo_name("https://host/owner/.hidden"), None);
        assert_eq!(repo_name(""), None);
    }
}
