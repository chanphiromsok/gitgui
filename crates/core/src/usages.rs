//! Where a word is used: `git grep` at a commit (or in the working folder), grouped by file, with the lines that
//! define it told apart from the lines that use it.

use std::io::{BufRead, BufReader};
use std::ops::Range;
use std::process::Stdio;

use crate::backend::{Error, GitCli, check_rev};

/// Most matches kept. A word that is everywhere (`id`, `data`) has more than anyone reads; the rest is said, not listed.
pub const MAX_MATCHES: usize = 500;
/// Shortest word searched for.
pub const MIN_WORD: usize = 2;
/// Longest word searched for.
const MAX_WORD: usize = 200;
/// How much of a long line is kept for the preview, around the match.
const CLIP: usize = 200;

/// One line that has the word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageMatch {
    /// 1-based.
    pub line: u32,
    /// The line, trimmed and cut to what fits a preview, around the word.
    pub text: String,
    /// The line declares the word (`function x`, `const x =`, `class x`, `fn x` ...) rather than using it.
    pub definition: bool,
}

/// The matches in one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageFile {
    pub path: String,
    pub matches: Vec<UsageMatch>,
}

/// Everything found for a word.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usages {
    pub word: String,
    /// Files in the order git lists them (by path), each with its matches in line order.
    pub files: Vec<UsageFile>,
    /// How many matches are in `files`.
    pub matches: usize,
    /// There were more than [`MAX_MATCHES`]; the rest was not read.
    pub truncated: bool,
}

impl Usages {
    /// The lines that declare the word, with their files.
    pub fn definitions(&self) -> impl Iterator<Item = (&str, &UsageMatch)> {
        self.files.iter().flat_map(|file| file.matches.iter().filter(|m| m.definition).map(move |m| (file.path.as_str(), m)))
    }
}

/// Why `word` is not worth searching for, in words for the person; `None` when it is.
pub fn word_problem(word: &str) -> Option<&'static str> {
    let length = word.chars().count();
    if length < MIN_WORD {
        Some("Pick a word of at least two characters.")
    } else if length > MAX_WORD {
        Some("That is too long to search for.")
    } else if word.contains(['\n', '\r', '\0']) {
        Some("Pick a single line of text.")
    } else {
        None
    }
}

/// A character a name is made of.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The name under byte `offset` of `text`: where it starts and ends; `None` when `offset` is not on one.
pub fn word_at(text: &str, offset: usize) -> Option<Range<usize>> {
    let offset = offset.min(text.len());
    // A pointer at the end of a name (just past its last letter) still means that name.
    let on = offset < text.len() && text[offset..].chars().next().is_some_and(is_word_char);
    let before = text[..offset].chars().next_back().is_some_and(is_word_char);
    if !on && !before {
        return None;
    }
    let start_from = offset;
    let start = text[..start_from].char_indices().rev().take_while(|(_, c)| is_word_char(*c)).last().map_or(start_from, |(at, _)| at);
    let end = start_from + text[start_from..].char_indices().find(|(_, c)| !is_word_char(*c)).map_or(text.len() - start_from, |(at, _)| at);
    (start < end).then_some(start..end)
}

/// Where `word` is in `text` as a whole name (not inside a longer one).
pub fn occurrences(text: &str, word: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    if word.is_empty() {
        return found;
    }
    for (at, _) in text.match_indices(word) {
        let end = at + word.len();
        let before = text[..at].chars().next_back().is_some_and(is_word_char);
        let after = text[end..].chars().next().is_some_and(is_word_char);
        if !before && !after {
            found.push(at..end);
        }
    }
    found
}

/// Words that, just before a name, make the line the place it is declared.
const DECLARES: &[&str] = &[
    "function", "function*", "const", "let", "var", "class", "interface", "type", "enum", "namespace", "def", "fn", "struct", "trait", "mod", "static",
    "func", "protocol", "module", "fun", "val", "object", "record", "sub", "proc", "macro_rules!", "typealias",
];

/// Whether `text` declares `word` (`export const word =`, `pub fn word(`, `class word`, `def word`): no parsing, only
/// what stands just before the name. A comment or a markdown line declares nothing.
pub fn declares(text: &str, word: &str, whole: bool) -> bool {
    let trimmed = text.trim_start();
    if ["//", "/*", "*", "#", "--", "<!--"].iter().any(|start| trimmed.starts_with(start)) {
        return false;
    }
    let spots: Vec<usize> = if whole {
        occurrences(text, word).into_iter().map(|r| r.start).collect()
    } else {
        let lower = text.to_lowercase();
        // Lowercasing may move offsets (a few letters change length); keep to texts where it did not.
        if lower.len() != text.len() { Vec::new() } else { lower.match_indices(&word.to_lowercase()).map(|(at, _)| at).collect() }
    };
    spots.into_iter().any(|at| {
        let last = text[..at].trim_end().rsplit(char::is_whitespace).next().unwrap_or("");
        DECLARES.contains(&last)
    })
}

/// `text` for a preview: without its indentation, and when it is long, the part around the first `word` with an
/// ellipsis where it was cut.
fn clip(text: &str, word: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= CLIP {
        return text.to_owned();
    }
    let at = text.find(word).or_else(|| text.to_lowercase().find(&word.to_lowercase())).unwrap_or(0).min(text.len());
    let from = text[..at].char_indices().rev().nth(60).map_or(0, |(i, _)| i);
    let kept: String = text[from..].chars().take(CLIP).collect();
    format!("{}{kept}{}", if from > 0 { "…" } else { "" }, if text[from..].chars().count() > CLIP { "…" } else { "" })
}

/// One line of `git grep --null -n` output: `[rev:]path NUL line NUL text`.
fn parse_record<'a>(rev: Option<&str>, record: &'a [u8]) -> Option<(String, u32, std::borrow::Cow<'a, str>)> {
    let mut parts = record.splitn(3, |&byte| byte == 0);
    let path = String::from_utf8_lossy(parts.next()?).into_owned();
    let line: u32 = std::str::from_utf8(parts.next()?).ok()?.parse().ok()?;
    let text = String::from_utf8_lossy(parts.next()?);
    let path = match rev {
        Some(rev) => path.strip_prefix(&format!("{rev}:"))?.to_owned(),
        None => path,
    };
    Some((path, line, text))
}

impl GitCli {
    /// Every line that has `word`, as the files were in commit `rev`; with `None`, in the working folder (what is
    /// tracked, and what is new but not ignored). `loose` finds the word inside longer ones and ignores case;
    /// otherwise only the whole name, in exactly that case, counts. Stops reading after [`MAX_MATCHES`].
    pub fn usages(&self, rev: Option<&str>, word: &str, loose: bool) -> Result<Usages, Error> {
        if let Some(problem) = word_problem(word) {
            return Err(Error::Parse(problem.into()));
        }
        let mut args = vec!["grep", "-n", "-I", "--no-color", "--null", "-F", if loose { "-i" } else { "-w" }, "-e", word];
        match rev {
            Some(rev) => {
                check_rev(rev)?;
                args.push(rev);
            }
            None => args.push("--untracked"),
        }
        args.push("--");
        let mut command = self.command(&args);
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().map_err(Error::Spawn)?;
        let mut reader = BufReader::new(child.stdout.take().expect("stdout was piped"));

        let mut found = Usages { word: word.to_owned(), ..Usages::default() };
        let mut record = Vec::new();
        loop {
            record.clear();
            if reader.read_until(b'\n', &mut record).map_err(Error::Spawn)? == 0 {
                break;
            }
            while record.last().is_some_and(|byte| matches!(byte, b'\n' | b'\r')) {
                record.pop();
            }
            let Some((path, line, text)) = parse_record(rev, &record) else { continue };
            if found.matches >= MAX_MATCHES {
                found.truncated = true;
                // The rest is not wanted: stop git instead of reading a flood.
                let _ = child.kill();
                break;
            }
            let definition = declares(&text, word, !loose);
            let m = UsageMatch { line, text: clip(&text, word), definition };
            match found.files.last_mut() {
                Some(file) if file.path == path => file.matches.push(m),
                _ => found.files.push(UsageFile { path, matches: vec![m] }),
            }
            found.matches += 1;
        }
        let output = child.wait_with_output().map_err(Error::Spawn)?;
        // `git grep` exits 1 when nothing matched; that is an answer, not a failure. Killed on purpose is no failure either.
        if !found.truncated && !output.status.success() && output.status.code() != Some(1) {
            return Err(Error::Git { status: output.status.code(), stderr: String::from_utf8_lossy(&output.stderr).into_owned() });
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_found_under_the_pointer_or_just_before_it() {
        let text = "export function refetchManifestQueries({ id }) {";
        let at = text.find("Manifest").unwrap();
        assert_eq!(&text[word_at(text, at).unwrap()], "refetchManifestQueries");
        // The first letter, and just past the last one.
        assert_eq!(&text[word_at(text, text.find("refetch").unwrap()).unwrap()], "refetchManifestQueries");
        assert_eq!(&text[word_at(text, text.find('(').unwrap()).unwrap()], "refetchManifestQueries");
        // On a space or a bracket nothing is picked, and `$` and `_` belong to names.
        assert_eq!(word_at(text, text.find('{').unwrap()), None);
        assert_eq!(&"a $ref_1 b"[word_at("a $ref_1 b", 4).unwrap()], "$ref_1");
        assert_eq!(word_at("", 0), None);
        assert_eq!(word_at("  ", 1), None);
        // Letters outside ASCII are letters.
        assert_eq!(&"naïve café"[word_at("naïve café", 8).unwrap()], "café");
    }

    #[test]
    fn only_a_whole_name_counts_as_the_word() {
        let text = "load(loadAll, reload, load_x, load)";
        let found: Vec<&str> = occurrences(text, "load").iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(found.len(), 2);
        assert_eq!(occurrences(text, "load").iter().map(|r| r.start).collect::<Vec<_>>(), [0, text.rfind("load").unwrap()]);
        assert!(occurrences(text, "").is_empty());
    }

    #[test]
    fn the_line_that_declares_a_name_is_told_from_the_lines_that_use_it() {
        for line in [
            "export function refetchManifestQueries({",
            "export async function refetchManifestQueries(",
            "export const refetchManifestQueries = (a) =>",
            "pub fn refetchManifestQueries(&self) {",
            "class refetchManifestQueries extends X {",
            "def refetchManifestQueries(self):",
            "type refetchManifestQueries = {",
            "    interface refetchManifestQueries {",
        ] {
            assert!(declares(line, "refetchManifestQueries", true), "{line}");
        }
        for line in [
            "    onSuccess: (_d, v) => refetchManifestQueries(v),",
            "import { refetchManifestQueries } from \"@/api/utils/refetchManifestQueries\";",
            "const result = refetchManifestQueries(x);",
            "// export function refetchManifestQueries(",
            "# function refetchManifestQueries notes",
            "return refetchManifestQueries;",
        ] {
            assert!(!declares(line, "refetchManifestQueries", true), "{line}");
        }
        // Loose matching finds a declaration in other letters too.
        assert!(declares("function REFETCH(", "refetch", false));
    }

    #[test]
    fn a_long_line_is_cut_around_the_word_and_a_short_one_only_trimmed() {
        assert_eq!(clip("    short line with word", "word"), "short line with word");
        let long = format!("{}needle{}", "a ".repeat(150), " b".repeat(150));
        let clipped = clip(&long, "needle");
        assert!(clipped.contains("needle") && clipped.starts_with('…') && clipped.ends_with('…'), "{clipped}");
        assert!(clipped.chars().count() <= CLIP + 2);
        // A multi-byte character at the cut does not split.
        let wide = format!("{}word{}", "é".repeat(150), "ü".repeat(150));
        assert!(clip(&wide, "word").contains("word"));
    }

    #[test]
    fn a_record_has_the_path_the_line_and_the_text() {
        let record = b"abc123:src/a.ts\x0012\x00  const x = 1;";
        let (path, line, text) = parse_record(Some("abc123"), record).unwrap();
        assert_eq!((path.as_str(), line, &*text), ("src/a.ts", 12, "  const x = 1;"));
        // In the working folder there is no commit before the path; a path with a colon is kept whole.
        let (path, line, _) = parse_record(None, b"docs/a:b.md\x003\x00text").unwrap();
        assert_eq!((path.as_str(), line), ("docs/a:b.md", 3));
        // Not a record: skipped, not a failure.
        assert!(parse_record(None, b"Binary file matches").is_none());
        assert!(parse_record(Some("abc123"), b"other:src/a.ts\x001\x00t").is_none());
    }

    #[test]
    fn some_words_are_not_worth_searching_for() {
        assert!(word_problem("a").is_some());
        assert!(word_problem("").is_some());
        assert!(word_problem("two\nlines").is_some());
        assert!(word_problem(&"x".repeat(MAX_WORD + 1)).is_some());
        assert!(word_problem("ok").is_none());
        assert!(word_problem("fn main").is_none());
    }
}
