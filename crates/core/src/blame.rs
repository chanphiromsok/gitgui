//! Who last changed each line of a file: `git blame`, read into a table of commits and a commit for every line.
//!
//! The text format git prints for scripts (`--line-porcelain`) repeats a commit's details on every line, so the
//! details are kept once, and a file of ten thousand lines costs ten thousand small numbers, not ten thousand
//! copies of an author's name.

use std::collections::HashMap;

use crate::backend::{Error, GitCli};

/// What git says about the commit that last changed a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameInfo {
    /// The commit's id; all zeros for a line that is not committed yet.
    pub commit: String,
    pub author: String,
    pub email: String,
    /// Seconds since the epoch.
    pub time: i64,
    pub summary: String,
}

impl BlameInfo {
    /// The line is in the working tree and in no commit.
    pub fn uncommitted(&self) -> bool {
        self.commit.bytes().all(|b| b == b'0')
    }
}

/// The blame of a whole file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blame {
    infos: Vec<BlameInfo>,
    /// For each line (from the first), an index into `infos`.
    lines: Vec<u32>,
}

impl Blame {
    /// The commit behind line `number`, counting from 1.
    pub fn line(&self, number: u32) -> Option<&BlameInfo> {
        let at = usize::try_from(number).ok()?.checked_sub(1)?;
        self.infos.get(*self.lines.get(at)? as usize)
    }

    /// How many lines the file has.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Reads what `git blame --line-porcelain` printed.
    pub fn parse(porcelain: &str) -> Blame {
        let mut blame = Blame::default();
        let mut seen: HashMap<&str, u32> = HashMap::new();
        // The entry being read: its commit, its line, and what the lines below it say.
        let mut commit = "";
        let mut line = 0usize;
        let mut info = BlameInfo { commit: String::new(), author: String::new(), email: String::new(), time: 0, summary: String::new() };
        for text in porcelain.lines() {
            if text.starts_with('\t') {
                // The line itself ends its entry.
                let index = match seen.get(commit) {
                    Some(&index) => index,
                    None => {
                        let index = blame.infos.len() as u32;
                        blame.infos.push(std::mem::replace(
                            &mut info,
                            BlameInfo { commit: String::new(), author: String::new(), email: String::new(), time: 0, summary: String::new() },
                        ));
                        seen.insert(commit, index);
                        index
                    }
                };
                if line >= 1 {
                    if blame.lines.len() < line {
                        blame.lines.resize(line, index);
                    }
                    blame.lines[line - 1] = index;
                }
                continue;
            }
            if let Some((word, rest)) = text.split_once(' ') {
                if is_header(word, rest) {
                    commit = word;
                    // "<commit> <line in the original> <line in the file> [<lines in this group>]".
                    line = rest.split(' ').nth(1).and_then(|n| n.parse().ok()).unwrap_or(0);
                    info.commit = word.to_owned();
                    continue;
                }
                match word {
                    "author" => info.author = rest.to_owned(),
                    "author-mail" => info.email = rest.trim_matches(|c| c == '<' || c == '>').to_owned(),
                    "author-time" => info.time = rest.parse().unwrap_or(0),
                    "summary" => info.summary = rest.to_owned(),
                    _ => {}
                }
            }
        }
        blame
    }
}

/// A header line starts an entry: a full commit id, then two or three numbers.
fn is_header(word: &str, rest: &str) -> bool {
    word.len() >= 40 && word.bytes().all(|b| b.is_ascii_hexdigit()) && rest.split(' ').all(|n| n.parse::<u32>().is_ok())
}

impl GitCli {
    /// Who last changed each line of `path` as it is in `rev`, or in the working tree (committed or not) when `rev` is
    /// `None`. Whitespace-only changes are not blamed (`-w`), and a file that has no such revision is an error.
    pub fn blame(&self, rev: Option<&str>, path: &str) -> Result<Blame, Error> {
        self.blame_with(rev, path, true)
    }

    /// [`GitCli::blame`], counting whitespace-only changes too when `ignore_whitespace` is false: in a conflict a
    /// re-indent is a change like any other.
    pub(crate) fn blame_with(&self, rev: Option<&str>, path: &str, ignore_whitespace: bool) -> Result<Blame, Error> {
        let mut args = vec!["blame", "--line-porcelain"];
        if ignore_whitespace {
            args.push("-w");
        }
        if let Some(rev) = rev {
            args.push(rev);
        }
        args.extend(["--", path]);
        let bytes = self.run(&args)?;
        Ok(Blame::parse(&String::from_utf8_lossy(&bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[allow(clippy::too_many_arguments)]
    fn entry(commit: &str, original: u32, line: u32, group: Option<u32>, author: &str, time: i64, summary: &str, text: &str) -> String {
        let group = group.map(|n| format!(" {n}")).unwrap_or_default();
        format!(
            "{commit} {original} {line}{group}\nauthor {author}\nauthor-mail <{}@x.dev>\nauthor-time {time}\nauthor-tz +0700\n\
             committer {author}\ncommitter-mail <{}@x.dev>\ncommitter-time {time}\ncommitter-tz +0700\nsummary {summary}\n\
             filename f.txt\n\t{text}\n",
            author.to_lowercase(),
            author.to_lowercase(),
        )
    }

    #[test]
    fn each_line_finds_its_commit_and_a_commit_is_kept_once() {
        let text = [
            entry(A, 1, 1, Some(2), "Ada", 100, "first", "one"),
            entry(A, 2, 2, None, "Ada", 100, "first", "two"),
            entry(B, 1, 3, Some(1), "Grace", 200, "second: a fix", "three"),
        ]
        .concat();
        let blame = Blame::parse(&text);
        assert_eq!(blame.len(), 3);
        assert_eq!(blame.infos.len(), 2, "two commits, however many lines");
        let first = blame.line(1).unwrap();
        assert_eq!((first.commit.as_str(), first.author.as_str(), first.email.as_str(), first.time), (A, "Ada", "ada@x.dev", 100));
        assert_eq!(first.summary, "first");
        assert_eq!(blame.line(2), blame.line(1));
        let third = blame.line(3).unwrap();
        assert_eq!((third.author.as_str(), third.summary.as_str()), ("Grace", "second: a fix"));
        assert_eq!(blame.line(0), None, "lines count from 1");
        assert_eq!(blame.line(4), None);
    }

    #[test]
    fn a_line_that_is_not_committed_says_so() {
        let zeros = "0".repeat(40);
        let text = entry(&zeros, 1, 1, Some(1), "Not Committed Yet", 300, "Version of f.txt from f.txt", "draft");
        let blame = Blame::parse(&text);
        assert!(blame.line(1).unwrap().uncommitted());
        assert!(!Blame::parse(&entry(A, 1, 1, Some(1), "Ada", 1, "s", "x")).line(1).unwrap().uncommitted());
    }

    #[test]
    fn a_line_that_looks_like_a_header_inside_the_code_is_only_code() {
        // The file's own text starts with a tab in the porcelain, so it can never be taken for an entry.
        let code = format!("{B} 9 9 9");
        let text = [entry(A, 1, 1, Some(2), "Ada", 1, "s", &code), entry(A, 2, 2, None, "Ada", 1, "s", "next")].concat();
        let blame = Blame::parse(&text);
        assert_eq!(blame.len(), 2);
        assert_eq!(blame.infos.len(), 1);
    }

    #[test]
    fn nothing_printed_is_an_empty_blame() {
        let blame = Blame::parse("");
        assert!(blame.is_empty());
        assert_eq!(blame.line(1), None);
    }
}
