//! Where a repository lives on the web, from its remote's URL: so a pull request or commit can be
//! opened in the browser.

/// The kind of site a remote is on; each spells its pull request links its own way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    GitHub,
    GitLab,
    Bitbucket,
}

/// A repository's home page and the site it is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebRemote {
    /// `https://github.com/owner/repo`, with no trailing slash or `.git`.
    pub base: String,
    pub host: Host,
}

impl WebRemote {
    /// The page of pull request (merge request, on GitLab) `number`.
    pub fn pull_request(&self, number: u32) -> String {
        match self.host {
            Host::GitHub => format!("{}/pull/{number}", self.base),
            Host::GitLab => format!("{}/-/merge_requests/{number}", self.base),
            Host::Bitbucket => format!("{}/pull-requests/{number}", self.base),
        }
    }

    pub fn commit(&self, id: &str) -> String {
        match self.host {
            Host::GitHub => format!("{}/commit/{id}", self.base),
            Host::GitLab => format!("{}/-/commit/{id}", self.base),
            Host::Bitbucket => format!("{}/commits/{id}", self.base),
        }
    }

    /// What to call a pull request on this site.
    pub fn pull_request_name(&self) -> &'static str {
        match self.host {
            Host::GitLab => "Merge Request",
            Host::GitHub | Host::Bitbucket => "Pull Request",
        }
    }
}

/// The web home of a remote URL in any of git's forms: `git@github.com:owner/repo.git`,
/// `ssh://git@host:2222/owner/repo.git`, `https://user@bitbucket.org/owner/repo`. `None` for a site
/// this does not know, or a local path.
pub fn web_remote(url: &str) -> Option<WebRemote> {
    let url = url.trim();
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let rest = rest.split_once('@').filter(|(user, _)| !user.contains('/')).map_or(rest, |(_, r)| r);
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next()?, path)
    } else {
        // scp-like: [user@]host:owner/repo
        let rest = url.split_once('@').map_or(url, |(_, r)| r);
        let (host, path) = rest.split_once(':')?;
        if host.contains('/') {
            return None;
        }
        (host, path)
    };
    let host = host.to_ascii_lowercase();
    let kind = if host.contains("github") {
        Host::GitHub
    } else if host.contains("gitlab") {
        Host::GitLab
    } else if host.contains("bitbucket") {
        Host::Bitbucket
    } else {
        return None;
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    // An SSH host name for GitHub is `github.com`, also behind aliases like `ssh.github.com`.
    let web_host = match (kind, host.as_str()) {
        (Host::GitHub, "ssh.github.com") => "github.com",
        (Host::GitLab, "altssh.gitlab.com") => "gitlab.com",
        (Host::Bitbucket, "altssh.bitbucket.org") => "bitbucket.org",
        _ => host.as_str(),
    };
    (path.contains('/') && !path.is_empty()).then(|| WebRemote { base: format!("https://{web_host}/{path}"), host: kind })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(url: &str) -> Option<String> {
        web_remote(url).map(|w| w.base)
    }

    #[test]
    fn every_form_of_remote_url_gives_the_web_home() {
        let home = Some("https://github.com/bstnt/customer".to_owned());
        assert_eq!(base("git@github.com:bstnt/customer.git"), home);
        assert_eq!(base("https://github.com/bstnt/customer.git"), home);
        assert_eq!(base("https://github.com/bstnt/customer"), home);
        assert_eq!(base("https://token@github.com/bstnt/customer/"), home);
        assert_eq!(base("ssh://git@ssh.github.com:443/bstnt/customer.git"), home);
        assert_eq!(base("git@gitlab.com:group/sub/project.git"), Some("https://gitlab.com/group/sub/project".into()));
        assert_eq!(base("https://rom@bitbucket.org/team/repo.git"), Some("https://bitbucket.org/team/repo".into()));
        assert_eq!(base("git@github.mycorp.com:team/app.git"), Some("https://github.mycorp.com/team/app".into()));
        assert_eq!(base("/Users/me/repos/thing"), None);
        assert_eq!(base("git@example.com:a/b.git"), None);
    }

    #[test]
    fn each_site_spells_its_links_its_own_way() {
        let gh = web_remote("git@github.com:o/r.git").unwrap();
        assert_eq!(gh.pull_request(42), "https://github.com/o/r/pull/42");
        assert_eq!(gh.commit("abc"), "https://github.com/o/r/commit/abc");
        let gl = web_remote("git@gitlab.com:o/r.git").unwrap();
        assert_eq!(gl.pull_request(7), "https://gitlab.com/o/r/-/merge_requests/7");
        assert_eq!(gl.pull_request_name(), "Merge Request");
        let bb = web_remote("git@bitbucket.org:o/r.git").unwrap();
        assert_eq!(bb.pull_request(3), "https://bitbucket.org/o/r/pull-requests/3");
    }
}
