//! Who is who: one person often commits under several names and emails (a laptop's git config, the
//! GitHub web interface, an old address). They are found together here, so the person shows with
//! one picture, one set of initials and one color.
//!
//! Two identities are taken to be one person when:
//! - they are the same GitHub account (`123+user@users.noreply.github.com`, `user@users.noreply.github.com`);
//! - one committed the other's work, and committed no one else's: someone rebasing or amending their
//!   own commits under another identity. Someone who commits many people's work (a maintainer
//!   rebasing pull requests) is left alone, and so are services such as GitHub's own web merges.
//!
//! A `.mailmap` in the repository has already been applied by git when the log was read.

use std::collections::{HashMap, HashSet};

use crate::model::Commit;

/// One person, by the identity they use most.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// The name they author most commits under.
    pub name: String,
    /// The email they author most commits under: it picks their color.
    pub email: String,
    /// The email their picture is found by: a GitHub address when they have one.
    pub avatar_email: String,
}

/// Every person in a history, by any of their emails.
#[derive(Clone, Debug, Default)]
pub struct People {
    by_email: HashMap<String, usize>,
    people: Vec<Person>,
}

impl People {
    /// Everyone in the history with every email they commit under (lowercase), in no particular order.
    pub fn everyone(&self) -> Vec<(&Person, Vec<&str>)> {
        let mut emails: Vec<Vec<&str>> = vec![Vec::new(); self.people.len()];
        for (email, &ix) in &self.by_email {
            emails[ix].push(email.as_str());
        }
        self.people.iter().zip(emails).collect()
    }

    /// The person behind `email`, if they appear in the history.
    pub fn of(&self, email: &str) -> Option<&Person> {
        self.by_email.get(&key(email)).map(|&i| &self.people[i])
    }
}

fn key(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

/// The GitHub account of a no-reply address: `65760336+chanphiromsok@…` → `chanphiromsok`.
fn github_user(email: &str) -> Option<String> {
    let local = email.strip_suffix("@users.noreply.github.com")?;
    Some(local.rsplit('+').next()?.to_owned()).filter(|user| !user.is_empty())
}

/// Commits made by a service rather than a person.
fn is_service(email: &str) -> bool {
    matches!(email, "noreply@github.com" | "noreply@gitlab.com" | "") || email.ends_with("[bot]@users.noreply.github.com")
}

struct Sets(Vec<usize>);

impl Sets {
    fn find(&mut self, i: usize) -> usize {
        let mut root = i;
        while self.0[root] != root {
            root = self.0[root];
        }
        let mut at = i;
        while self.0[at] != root {
            (self.0[at], at) = (root, self.0[at]);
        }
        root
    }

    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[b] = a;
        }
    }
}

/// Finds the people in `commits`.
pub fn people(commits: &[Commit]) -> People {
    let mut emails: Vec<String> = Vec::new();
    // How often each identity authored a commit, under which name.
    let mut authored: HashMap<usize, HashMap<String, usize>> = HashMap::new();
    // Whose work each committer committed, other than their own.
    let mut committed_for: HashMap<usize, HashSet<usize>> = HashMap::new();
    {
        let mut ids: HashMap<String, usize> = HashMap::new();
        let mut id = |email: &str| -> usize {
            let email = key(email);
            let next = ids.len();
            *ids.entry(email.clone()).or_insert_with(|| {
                emails.push(email);
                next
            })
        };
        for commit in commits {
            let author = id(&commit.email);
            *authored.entry(author).or_default().entry(commit.author.clone()).or_default() += 1;
            let committer_email = key(&commit.committer_email);
            if !is_service(&committer_email) {
                let committer = id(&committer_email);
                if committer != author {
                    committed_for.entry(committer).or_default().insert(author);
                }
            }
        }
    }

    let mut sets = Sets((0..emails.len()).collect());
    let mut by_user: HashMap<String, usize> = HashMap::new();
    for (i, email) in emails.iter().enumerate() {
        if let Some(user) = github_user(email) {
            let first = *by_user.entry(user).or_insert(i);
            sets.join(first, i);
        }
    }
    for (committer, authors) in &committed_for {
        if let [author] = authors.iter().collect::<Vec<_>>()[..] {
            sets.join(*committer, *author);
        }
    }

    // Each group's person: their most used name and email, and a GitHub address for the picture.
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..emails.len() {
        groups.entry(sets.find(i)).or_default().push(i);
    }
    let mut people = People::default();
    for members in groups.values() {
        let count = |i: &usize| authored.get(i).map_or(0, |names| names.values().sum::<usize>());
        let main = *members.iter().max_by_key(|i| (count(i), std::cmp::Reverse(**i))).expect("a group has members");
        let mut names: HashMap<&str, usize> = HashMap::new();
        for i in members {
            for (name, n) in authored.get(i).into_iter().flatten() {
                *names.entry(name.as_str()).or_default() += n;
            }
        }
        let name = names.into_iter().max_by_key(|(name, n)| (*n, std::cmp::Reverse(*name))).map(|(name, _)| name.to_owned());
        let github = |numbered: bool| {
            members.iter().map(|&i| &emails[i]).find(|e| {
                github_user(e).is_some() && (!numbered || e.split('+').next().is_some_and(|id| id.chars().all(|c| c.is_ascii_digit())) && e.contains('+'))
            })
        };
        let avatar_email = github(true).or_else(|| github(false)).unwrap_or(&emails[main]).clone();
        let index = people.people.len();
        people.people.push(Person { name: name.unwrap_or_else(|| emails[main].clone()), email: emails[main].clone(), avatar_email });
        for &i in members {
            people.by_email.insert(emails[i].clone(), index);
        }
    }
    people
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(author: (&str, &str), committer: (&str, &str)) -> Commit {
        Commit {
            id: String::new(),
            parents: Vec::new(),
            author: author.0.into(),
            email: author.1.into(),
            committer: committer.0.into(),
            committer_email: committer.1.into(),
            time: 0,
            date: String::new(),
            summary: String::new(),
            refs: Vec::new(),
            stash: None,
        }
    }

    const ROM: (&str, &str) = ("Rom", "chanphiromsok2109@gmail.com");
    const SOK: (&str, &str) = ("Chanphirom Sok", "65760336+chanphiromsok@users.noreply.github.com");
    const SHORT: (&str, &str) = ("chanphiromsok", "chanphiromsok@users.noreply.github.com");
    const GITHUB: (&str, &str) = ("GitHub", "noreply@github.com");

    #[test]
    fn one_person_under_three_identities_is_found_as_one() {
        let mut history = vec![commit(ROM, ROM); 5];
        history.push(commit(SOK, GITHUB)); // a merge on GitHub: says nothing about Rom
        history.push(commit(SOK, ROM)); // Rom rebased Chanphirom Sok's commit: the same person
        history.push(commit(SHORT, SHORT)); // the same GitHub account
        let people = people(&history);
        let me = people.of(ROM.1).unwrap();
        assert_eq!(people.of(SOK.1), Some(me));
        assert_eq!(people.of("CHANPHIROMSOK@users.noreply.github.com"), Some(me), "case does not matter");
        assert_eq!(me.name, "Rom", "the name used most");
        assert_eq!(me.email, ROM.1);
        assert_eq!(me.avatar_email, SOK.1, "the GitHub address with an account number finds the picture");
        assert_ne!(people.of(GITHUB.1), Some(me), "GitHub's web merges are not anyone");
    }

    #[test]
    fn a_maintainer_who_commits_many_peoples_work_stays_themselves() {
        let ada = ("Ada", "ada@example.com");
        let bob = ("Bob", "bob@example.com");
        let max = ("Max", "max@example.com");
        let history = [commit(ada, max), commit(bob, max), commit(max, max)];
        let people = people(&history);
        assert_ne!(people.of(ada.1), people.of(max.1));
        assert_ne!(people.of(bob.1), people.of(ada.1));
        assert_eq!(people.of(max.1).unwrap().name, "Max");
    }

    #[test]
    fn strangers_and_bots_stay_apart() {
        let bot = ("dependabot[bot]", "49699333+dependabot[bot]@users.noreply.github.com");
        let ada = ("Ada", "ada@example.com");
        let history = [commit(bot, bot), commit(ada, ada), commit(ada, GITHUB)];
        let people = people(&history);
        assert_ne!(people.of(bot.1), people.of(ada.1));
        assert!(people.of("someone@else.com").is_none());
    }
}
