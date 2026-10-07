//! Syntax highlighting for the diff, with tree-sitter.
//!
//! A diff alone is a poor thing to parse: a hunk starts mid-function, or inside a string. So the
//! whole file is highlighted, old and new, and each diff line takes the colors of its line in the
//! file it came from. The result is a list of capture names per span; the theme turns a name into
//! a color, the way Zed does: `function.method` falls back to `function` when the theme has no
//! color for it.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;

use gitgui_core::{DiffLine, FileDiff, LineKind};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Files bigger than this are shown plain: highlighting them would hold up the diff.
const MAX_BYTES: usize = 1 << 20;

/// The capture names highlighting knows. A query's capture takes the longest of these that it starts
/// with (`function.method.call` → `function.method`); a theme then colors it, falling back to
/// shorter names.
pub const NAMES: &[&str] = &[
    "attribute",
    "boolean",
    "character",
    "comment",
    "comment.doc",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "emphasis",
    "emphasis.strong",
    "enum",
    "escape",
    "function",
    "function.builtin",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "link_text",
    "link_uri",
    "module",
    "namespace",
    "number",
    "operator",
    "preproc",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.list_marker",
    "punctuation.special",
    "string",
    "string.escape",
    "string.regex",
    "string.special",
    "string.special.key",
    "string.special.symbol",
    "tag",
    "tag.attribute",
    "text.literal",
    "text.title",
    "title",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.member",
    "variable.parameter",
    "variable.special",
    "variant",
];

/// A colored run within one line: byte offsets into the line, and an index into [`NAMES`].
pub type Span = (Range<u32>, u16);

/// The spans of each line of a file, by line number less one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lines(pub Vec<Vec<Span>>);

impl Lines {
    /// The spans of line `number` (counted from 1).
    pub fn line(&self, number: u32) -> &[Span] {
        number.checked_sub(1).and_then(|ix| self.0.get(ix as usize)).map_or(&[], Vec::as_slice)
    }
}

const KOTLIN_HIGHLIGHTS: &str = r#"
[(line_comment) (block_comment)] @comment
[(string_literal) (multiline_string_literal) (character_literal)] @string
(escape_sequence) @string.escape
(interpolation) @embedded
[(number_literal) (float_literal)] @number
(annotation) @attribute
(label) @label
(function_declaration name: (identifier) @function)
(call_expression (identifier) @function)
(call_expression (navigation_expression (identifier) @function.method .))
(class_declaration name: (identifier) @type)
(object_declaration name: (identifier) @type)
(user_type (identifier) @type)
(parameter (identifier) @variable.parameter)
(this_expression) @variable.builtin
(super_expression) @variable.builtin
[
  "abstract" "actual" "annotation" "as" "by" "catch" "class" "companion" "const" "constructor"
  "crossinline" "data" "do" "else" "enum" "expect" "external" "final" "finally" "for" "fun" "get"
  "if" "import" "in" "infix" "init" "inline" "inner" "interface" "internal" "is" "lateinit"
  "noinline" "object" "open" "operator" "out" "override" "package" "private" "protected" "public"
  "return" "sealed" "set" "suspend" "tailrec" "throw" "try" "typealias" "val" "value" "var"
  "vararg" "when" "where" "while"
] @keyword
"#;

struct Language {
    name: &'static str,
    language: tree_sitter::Language,
    highlights: String,
    injections: &'static str,
    locals: String,
}

fn languages() -> Vec<Language> {
    use tree_sitter_javascript as js;
    use tree_sitter_typescript as ts;
    let lang = |name, language: tree_sitter_language::LanguageFn, highlights: &str, injections, locals: &str| Language {
        name,
        language: language.into(),
        highlights: highlights.to_owned(),
        injections,
        locals: locals.to_owned(),
    };
    let join = |parts: &[&str]| parts.join("\n");
    vec![
        lang("rust", tree_sitter_rust::LANGUAGE, tree_sitter_rust::HIGHLIGHTS_QUERY, tree_sitter_rust::INJECTIONS_QUERY, ""),
        lang(
            "typescript",
            ts::LANGUAGE_TYPESCRIPT,
            &join(&[ts::HIGHLIGHTS_QUERY, js::HIGHLIGHT_QUERY]),
            js::INJECTIONS_QUERY,
            &join(&[ts::LOCALS_QUERY, js::LOCALS_QUERY]),
        ),
        lang(
            "tsx",
            ts::LANGUAGE_TSX,
            &join(&[ts::HIGHLIGHTS_QUERY, js::JSX_HIGHLIGHT_QUERY, js::HIGHLIGHT_QUERY]),
            js::INJECTIONS_QUERY,
            &join(&[ts::LOCALS_QUERY, js::LOCALS_QUERY]),
        ),
        lang(
            "javascript",
            js::LANGUAGE,
            &join(&[js::JSX_HIGHLIGHT_QUERY, js::HIGHLIGHT_QUERY]),
            js::INJECTIONS_QUERY,
            js::LOCALS_QUERY,
        ),
        lang("json", tree_sitter_json::LANGUAGE, tree_sitter_json::HIGHLIGHTS_QUERY, "", ""),
        lang("css", tree_sitter_css::LANGUAGE, tree_sitter_css::HIGHLIGHTS_QUERY, "", ""),
        lang("html", tree_sitter_html::LANGUAGE, tree_sitter_html::HIGHLIGHTS_QUERY, tree_sitter_html::INJECTIONS_QUERY, ""),
        lang("python", tree_sitter_python::LANGUAGE, tree_sitter_python::HIGHLIGHTS_QUERY, "", ""),
        lang("go", tree_sitter_go::LANGUAGE, tree_sitter_go::HIGHLIGHTS_QUERY, "", ""),
        lang("bash", tree_sitter_bash::LANGUAGE, tree_sitter_bash::HIGHLIGHT_QUERY, "", ""),
        lang("c", tree_sitter_c::LANGUAGE, tree_sitter_c::HIGHLIGHT_QUERY, "", ""),
        lang(
            "cpp",
            tree_sitter_cpp::LANGUAGE,
            &join(&[tree_sitter_cpp::HIGHLIGHT_QUERY, tree_sitter_c::HIGHLIGHT_QUERY]),
            "",
            "",
        ),
        lang("java", tree_sitter_java::LANGUAGE, tree_sitter_java::HIGHLIGHTS_QUERY, "", ""),
        lang("ruby", tree_sitter_ruby::LANGUAGE, tree_sitter_ruby::HIGHLIGHTS_QUERY, "", tree_sitter_ruby::LOCALS_QUERY),
        lang(
            "swift",
            tree_sitter_swift::LANGUAGE,
            tree_sitter_swift::HIGHLIGHTS_QUERY,
            tree_sitter_swift::INJECTIONS_QUERY,
            tree_sitter_swift::LOCALS_QUERY,
        ),
        lang("kotlin", tree_sitter_kotlin_ng::LANGUAGE, KOTLIN_HIGHLIGHTS, "", ""),
        lang("toml", tree_sitter_toml_ng::LANGUAGE, tree_sitter_toml_ng::HIGHLIGHTS_QUERY, "", ""),
        lang("yaml", tree_sitter_yaml::LANGUAGE, tree_sitter_yaml::HIGHLIGHTS_QUERY, "", ""),
        lang(
            "markdown",
            tree_sitter_md::LANGUAGE,
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
            tree_sitter_md::INJECTION_QUERY_BLOCK,
            "",
        ),
        lang(
            "markdown_inline",
            tree_sitter_md::INLINE_LANGUAGE,
            tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
            tree_sitter_md::INJECTION_QUERY_INLINE,
            "",
        ),
    ]
}

/// Every language whose queries compile, by name. Built once, on first use.
fn configs() -> &'static HashMap<&'static str, HighlightConfiguration> {
    static CONFIGS: OnceLock<HashMap<&'static str, HighlightConfiguration>> = OnceLock::new();
    CONFIGS.get_or_init(|| {
        languages()
            .into_iter()
            .filter_map(|lang| {
                let mut config =
                    HighlightConfiguration::new(lang.language, lang.name, &lang.highlights, lang.injections, &lang.locals)
                        .ok()?;
                config.configure(NAMES);
                Some((lang.name, config))
            })
            .collect()
    })
}

/// The language of a file, from its name.
pub fn language_of(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    let by_name = match lower.as_str() {
        "dockerfile" | "makefile" | ".bashrc" | ".zshrc" | ".profile" => Some("bash"),
        "cargo.lock" => Some("toml"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    let ext = lower.rsplit_once('.')?.1;
    Some(match ext {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "json" | "jsonc" | "json5" => "json",
        "css" | "scss" | "less" => "css",
        "html" | "htm" | "xhtml" | "vue" | "svelte" => "html",
        "py" | "pyi" => "python",
        "go" => "go",
        "sh" | "bash" | "zsh" => "bash",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "mm" | "m" => "cpp",
        "java" => "java",
        "rb" | "rake" | "gemspec" => "ruby",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "md" | "markdown" | "mdx" => "markdown",
        _ => return None,
    })
}

/// Colors every line of `source`, a file named `path`. `None` when the language is not known, the
/// file is too big, or the parser gives up.
pub fn highlight(path: &str, source: &str) -> Option<Lines> {
    if source.len() > MAX_BYTES {
        return None;
    }
    let configs = configs();
    let config = configs.get(language_of(path)?)?;
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(config, source.as_bytes(), None, |name| {
            let name = match name {
                "js" | "jsx" => "javascript",
                "ts" => "typescript",
                "sh" | "shell" => "bash",
                "yml" => "yaml",
                "md" => "markdown",
                other => other,
            };
            configs.get(name)
        })
        .ok()?;

    // Where each line starts, to split runs at line ends.
    let starts: Vec<usize> =
        std::iter::once(0).chain(source.match_indices('\n').map(|(at, _)| at + 1)).collect();
    let mut lines: Vec<Vec<Span>> = vec![Vec::new(); starts.len()];
    let mut stack: Vec<u16> = Vec::new();
    for event in events {
        match event.ok()? {
            HighlightEvent::HighlightStart(highlight) => stack.push(highlight.0 as u16),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let Some(&name) = stack.last() else { continue };
                let mut at = start;
                while at < end {
                    let line = starts.partition_point(|&s| s <= at) - 1;
                    // Where the line's newline is, or the end of the file.
                    let line_end = starts.get(line + 1).map_or(source.len(), |&next| next - 1);
                    let stop = line_end.min(end);
                    if stop > at {
                        let base = starts[line];
                        lines[line].push(((at - base) as u32..(stop - base) as u32, name));
                    }
                    at = if stop < end { line_end + 1 } else { end };
                }
            }
        }
    }
    Some(Lines(lines))
}

/// The colors of both sides of one file's diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileColors {
    pub old: Lines,
    pub new: Lines,
}

impl FileColors {
    /// The spans for a diff line: from the new file unless it was removed.
    pub fn of(&self, line: &DiffLine) -> &[Span] {
        match (line.kind, line.old_no, line.new_no) {
            (LineKind::Removed, Some(old), _) => self.old.line(old),
            (_, _, Some(new)) => self.new.line(new),
            _ => &[],
        }
    }

    /// The spans for one side of a split row.
    pub fn side(&self, line: &DiffLine, old_side: bool) -> &[Span] {
        match (old_side, line.old_no, line.new_no) {
            (true, Some(old), _) => self.old.line(old),
            (false, _, Some(new)) => self.new.line(new),
            _ => &[],
        }
    }
}

/// Colors `diff` of the file at `path`, from the whole file before and after. A side whose text does
/// not match the diff's lines (line endings, filters) is left plain rather than colored wrongly.
pub fn for_diff(path: &str, old: Option<&str>, new: Option<&str>, diff: &FileDiff) -> FileColors {
    let side = |text: Option<&str>, number: fn(&DiffLine) -> Option<u32>| -> Lines {
        // Too big to color: say so before splitting a huge file into lines for nothing.
        let Some(text) = text.filter(|text| text.len() <= MAX_BYTES) else { return Lines::default() };
        let file: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
        let matches = diff.hunks.iter().flat_map(|h| &h.lines).all(|line| {
            number(line).is_none_or(|n| file.get(n as usize - 1).is_some_and(|f| *f == line.text.trim_end_matches('\r')))
        });
        if !matches {
            return Lines::default();
        }
        highlight(path, text).unwrap_or_default()
    };
    FileColors {
        old: side(old, |line| if line.kind == LineKind::Added { None } else { line.old_no }),
        new: side(new, |line| if line.kind == LineKind::Removed { None } else { line.new_no }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(spans: &[Span], line: &str) -> Vec<(String, &'static str)> {
        spans.iter().map(|(range, name)| (line[range.start as usize..range.end as usize].to_owned(), NAMES[*name as usize])).collect()
    }

    #[test]
    fn every_language_compiles_its_queries() {
        let built: Vec<&str> = languages().iter().map(|l| l.name).filter(|name| !configs().contains_key(name)).collect();
        assert!(built.is_empty(), "these languages failed to build: {built:?}");
    }

    #[test]
    fn files_are_known_by_extension_and_name() {
        assert_eq!(language_of("src/api/useQueryMembership.ts"), Some("typescript"));
        assert_eq!(language_of("App.TSX"), Some("tsx"));
        assert_eq!(language_of("crates/app/Cargo.toml"), Some("toml"));
        assert_eq!(language_of("Makefile"), Some("bash"));
        assert_eq!(language_of("locales/en/messages.po"), None);
    }

    #[test]
    fn a_typescript_file_is_colored_line_by_line() {
        let source = "import { useQuery } from \"@tanstack/react-query\";\n\nexport function useQueryMembership() {\n  // mocked\n  return 42;\n}\n";
        let lines = highlight("a.ts", source).expect("typescript highlights");
        let text: Vec<&str> = source.split('\n').collect();
        let first = names(lines.line(1), text[0]);
        assert!(first.contains(&("import".into(), "keyword")), "{first:?}");
        assert!(first.contains(&("\"@tanstack/react-query\"".into(), "string")), "{first:?}");
        assert!(lines.line(2).is_empty());
        let third = names(lines.line(3), text[2]);
        assert!(third.iter().any(|(t, n)| t == "useQueryMembership" && n.starts_with("function")), "{third:?}");
        assert_eq!(names(lines.line(4), text[3]), [("// mocked".into(), "comment")]);
        assert!(names(lines.line(5), text[4]).contains(&("42".into(), "number")));
    }

    #[test]
    fn a_run_across_lines_is_split_at_each_line_end() {
        let source = "/* one\ntwo */ let x = 1;";
        let lines = highlight("a.rs", source).unwrap();
        assert_eq!(names(lines.line(1), "/* one"), [("/* one".into(), "comment")]);
        assert_eq!(names(&lines.line(2)[..1], "two */ let x = 1;"), [("two */".into(), "comment")]);
    }

    #[test]
    fn a_diff_takes_its_colors_from_the_file_each_line_came_from() {
        let old = "let a = 1;\nlet b = \"x\";\n";
        let new = "let a = 1;\n// note\n";
        let diff = gitgui_core::diff::parse(
            "@@ -1,2 +1,2 @@\n let a = 1;\n-let b = \"x\";\n+// note\n",
        );
        let colors = for_diff("a.rs", Some(old), Some(new), &diff);
        let lines = &diff.hunks[0].lines;
        let name = |spans: &[Span]| spans.iter().map(|(_, n)| NAMES[*n as usize]).collect::<Vec<_>>();
        assert!(name(colors.of(&lines[1])).contains(&"string"), "the removed line is colored from the old file");
        assert_eq!(name(colors.of(&lines[2])), ["comment"], "the added line from the new file");
        assert_eq!(name(colors.side(&lines[0], true)), name(colors.side(&lines[0], false)));

        // A file that does not match the diff is left plain.
        let plain = for_diff("a.rs", Some("something else"), Some(new), &diff);
        assert!(plain.old.0.is_empty());
        assert!(!plain.new.0.is_empty());
    }

    #[test]
    fn unknown_or_huge_files_are_left_plain() {
        assert_eq!(highlight("notes.po", "msgid \"x\""), None);
        assert_eq!(highlight("big.rs", &"x".repeat(MAX_BYTES + 1)), None);
    }
}
