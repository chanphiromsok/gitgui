//! A small reader for the markdown that GitHub issues and pull requests are written in: enough to read them in the
//! window. Headings, paragraphs, lists (with tasks), quotes, code, rules, links, bold, italic, code spans and
//! strikethrough are drawn. Raw HTML is dropped, except line breaks and images, and comments (issue templates are full
//! of them) are removed.
//!
//! Pictures are not fetched. An image in someone's issue is a request to a server we did not choose (and on a private
//! repository GitHub would not answer it without a browser's cookie), so it shows as a link that opens in the browser.

use std::ops::Range;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, FontStyle, FontWeight, HighlightStyle, InteractiveText, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement,
    Styled, StrikethroughStyle, StyledText, UnderlineStyle, div, px, rgb,
};

use crate::theme::t;
use crate::ui::MONO;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Style {
    Bold,
    Italic,
    Code,
    Strike,
    Link(String),
}

/// Text with the styles that cover parts of it, by byte range.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Text {
    pub text: String,
    pub spans: Vec<(Range<usize>, Style)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Marker {
    Bullet,
    Number(u32),
    Task(bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading(u8, Text),
    Paragraph(Text),
    Item { depth: usize, marker: Marker, text: Text },
    Quote(Text),
    Code { lang: String, text: String },
    Rule,
    Image { alt: String, url: String },
}

// ---- reading ------------------------------------------------------------------------------------------

/// Takes out `<!-- … -->`, wherever it is, over any number of lines.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The value of attribute `name` in the text of an HTML tag (`<img src="x" alt='y'>`).
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag[at..];
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let inner = &rest[1..];
    Some(inner[..inner.find(quote)?].to_owned())
}

/// One line with its HTML turned into what markdown can say: `<br>` is a line break, `<img>` an image, `<summary>` a
/// bold line; any other tag goes and what is inside it stays.
fn html_to_markdown(line: &str) -> String {
    if !line.contains('<') {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        let Some(close) = after.find('>') else {
            out.push_str(after);
            return out;
        };
        let tag = &after[..=close];
        let inner = tag[1..tag.len() - 1].trim();
        let name = inner.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
        let is_tag = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
        if !is_tag {
            // `a < b`, `<3`: not a tag.
            out.push('<');
            rest = &after[1..];
            continue;
        }
        match name.as_str() {
            "br" => out.push('\n'),
            "img" => {
                if let Some(src) = attribute(tag, "src") {
                    out.push_str(&format!("![{}]({src})", attribute(tag, "alt").unwrap_or_default()));
                }
            }
            "summary" => out.push_str("**"),
            _ => {}
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// A list item's line: how deep, which marker, and the text after it.
fn list_item(line: &str) -> Option<(usize, Marker, &str)> {
    let spaces = line.len() - line.trim_start_matches(' ').len();
    let body = &line[spaces..];
    let (marker, rest) = if let Some(rest) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")).or_else(|| body.strip_prefix("+ ")) {
        (Marker::Bullet, rest)
    } else {
        let digits = body.bytes().take_while(u8::is_ascii_digit).count();
        let after = body[digits..].strip_prefix('.').or_else(|| body[digits..].strip_prefix(')'))?;
        let rest = after.strip_prefix(' ')?;
        (Marker::Number(body[..digits].parse().ok().filter(|_| digits > 0 && digits < 10)?), rest)
    };
    let task = |mark: &str, done: bool| rest.strip_prefix(mark).map(|text| (Marker::Task(done), text.trim_start()));
    let (marker, rest) = if marker == Marker::Bullet {
        task("[ ] ", false).or_else(|| task("[x] ", true)).or_else(|| task("[X] ", true)).unwrap_or((marker, rest))
    } else {
        (marker, rest)
    };
    Some((spaces / 2, marker, rest))
}

/// `## Title` as its level and its text; a hash with no space after it is not a heading.
fn heading(trimmed: &str) -> Option<(u8, &str)> {
    let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
    ((1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ')).then(|| (hashes as u8, trimmed[hashes..].trim().trim_end_matches('#').trim()))
}

fn is_rule(trimmed: &str) -> bool {
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3 && ["-", "*", "_"].iter().any(|mark| compact.chars().all(|c| mark.starts_with(c)))
}

/// What a block is before its text has been read for emphasis and links.
enum Raw {
    Heading(u8, String),
    Paragraph(String),
    Item(usize, Marker, String),
    Quote(String),
    Code(String, String),
    Rule,
}

/// Reads GitHub-flavoured markdown (the part that matters in an issue) into blocks.
pub fn parse(source: &str) -> Vec<Block> {
    let source = strip_comments(&source.replace("\r\n", "\n"));
    let mut raw: Vec<Raw> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut quote: Vec<String> = Vec::new();
    let mut fence: Option<(String, String, Vec<String>)> = None; // marker, language, lines

    fn flush(raw: &mut Vec<Raw>, paragraph: &mut Vec<String>, quote: &mut Vec<String>) {
        if !paragraph.is_empty() {
            raw.push(Raw::Paragraph(paragraph.join("\n")));
            paragraph.clear();
        }
        if !quote.is_empty() {
            raw.push(Raw::Quote(quote.join("\n")));
            quote.clear();
        }
    }

    for line in source.lines() {
        if let Some((marker, lang, lines)) = fence.as_mut() {
            if line.trim_start().starts_with(marker.as_str()) {
                raw.push(Raw::Code(lang.clone(), lines.join("\n")));
                fence = None;
            } else {
                lines.push(line.to_owned());
            }
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(marker) = ["```", "~~~"].iter().find(|m| trimmed.starts_with(**m)) {
            flush(&mut raw, &mut paragraph, &mut quote);
            fence = Some(((*marker).to_owned(), trimmed.trim_start_matches(*marker).trim().to_owned(), Vec::new()));
            continue;
        }
        let line = html_to_markdown(line.trim_end());
        // A `<br>` or a `<summary>` can have made one line into several.
        for line in line.split('\n') {
            let trimmed = line.trim_start();
            if trimmed.is_empty() {
                flush(&mut raw, &mut paragraph, &mut quote);
            } else if let Some(text) = trimmed.strip_prefix('>') {
                if !paragraph.is_empty() {
                    flush(&mut raw, &mut paragraph, &mut quote);
                }
                quote.push(text.strip_prefix(' ').unwrap_or(text).to_owned());
            } else if let Some((level, title)) = heading(trimmed) {
                flush(&mut raw, &mut paragraph, &mut quote);
                raw.push(Raw::Heading(level, title.to_owned()));
            } else if is_rule(trimmed) {
                flush(&mut raw, &mut paragraph, &mut quote);
                raw.push(Raw::Rule);
            } else if let Some((depth, marker, text)) = list_item(line) {
                flush(&mut raw, &mut paragraph, &mut quote);
                raw.push(Raw::Item(depth, marker, text.to_owned()));
            } else if line.starts_with("  ") && matches!(raw.last(), Some(Raw::Item(..))) && paragraph.is_empty() {
                // The next line of a list item.
                if let Some(Raw::Item(_, _, text)) = raw.last_mut() {
                    text.push('\n');
                    text.push_str(trimmed);
                }
            } else {
                if !quote.is_empty() {
                    flush(&mut raw, &mut paragraph, &mut quote);
                }
                paragraph.push(trimmed.to_owned());
            }
        }
    }
    if let Some((_, lang, lines)) = fence {
        raw.push(Raw::Code(lang, lines.join("\n")));
    }
    flush(&mut raw, &mut paragraph, &mut quote);

    let mut blocks = Vec::new();
    for item in raw {
        match item {
            Raw::Heading(level, text) => blocks.extend(inline_blocks(&text, |t| Block::Heading(level, t))),
            Raw::Paragraph(text) => blocks.extend(inline_blocks(&text, Block::Paragraph)),
            Raw::Quote(text) => blocks.extend(inline_blocks(&text, Block::Quote)),
            Raw::Item(depth, marker, text) => {
                let marker2 = marker.clone();
                blocks.extend(inline_blocks(&text, move |text| Block::Item { depth, marker: marker2.clone(), text }));
            }
            Raw::Code(lang, text) => blocks.push(Block::Code { lang, text }),
            Raw::Rule => blocks.push(Block::Rule),
        }
    }
    blocks
}

/// A piece of text as blocks: the text, with any image in it taken out as a block of its own where it was.
fn inline_blocks(source: &str, make: impl Fn(Text) -> Block) -> Vec<Block> {
    let mut out = Text::default();
    let mut images: Vec<(usize, String, String)> = Vec::new();
    parse_inline(source, &mut out, &mut images, false);
    let mut blocks = Vec::new();
    let mut from = 0;
    for (at, alt, url) in images {
        let before = slice(&out, from..at);
        if !before.text.trim().is_empty() {
            blocks.push(make(before));
        }
        blocks.push(Block::Image { alt, url });
        from = at;
    }
    let after = slice(&out, from..out.text.len());
    if !after.text.trim().is_empty() || blocks.is_empty() {
        blocks.push(make(after));
    }
    blocks
}

/// The part of `text` inside `range`, trimmed, with the styles cut to fit.
fn slice(text: &Text, range: Range<usize>) -> Text {
    let piece = &text.text[range.clone()];
    let base = range.start + (piece.len() - piece.trim_start().len());
    let trimmed = piece.trim();
    let end = base + trimmed.len();
    let spans = text
        .spans
        .iter()
        .filter_map(|(span, style)| {
            let (start, stop) = (span.start.max(base), span.end.min(end));
            (start < stop).then(|| (start - base..stop - base, style.clone()))
        })
        .collect();
    Text { text: trimmed.to_owned(), spans }
}

/// Reads emphasis, code, links and bare addresses from `src` and adds them to `out`. An image is not added: its place
/// in the text and its address go to `images`.
fn parse_inline(src: &str, out: &mut Text, images: &mut Vec<(usize, String, String)>, in_link: bool) {
    let mut i = 0;
    while i < src.len() {
        let rest = &src[i..];
        let prev = src[..i].chars().next_back();
        // An image, or a linked image (the badge form `[![alt](img)](page)`: the picture is what shows).
        if let Some(body) = rest.strip_prefix("![")
            && let Some((alt, url, used)) = bracketed(body)
        {
            images.push((out.text.len(), alt.to_owned(), url.to_owned()));
            i += 2 + used;
            continue;
        }
        if rest.starts_with('`') {
            let ticks = rest.bytes().take_while(|b| *b == b'`').count();
            let fence = "`".repeat(ticks);
            if let Some(end) = rest[ticks..].find(&fence) {
                let start = out.text.len();
                out.text.push_str(rest[ticks..ticks + end].trim_matches(' '));
                out.spans.push((start..out.text.len(), Style::Code));
                i += ticks + end + ticks;
                continue;
            }
        }
        for (mark, style) in [("**", Style::Bold), ("__", Style::Bold), ("~~", Style::Strike)] {
            if let Some(body) = rest.strip_prefix(mark)
                && let Some(end) = body.find(mark).filter(|end| *end > 0)
            {
                let start = out.text.len();
                parse_inline(&body[..end], out, images, in_link);
                out.spans.push((start..out.text.len(), style));
                i += mark.len() * 2 + end;
                return parse_rest(src, i, out, images, in_link);
            }
        }
        if let Some(mark) = rest.chars().next().filter(|c| *c == '*' || *c == '_') {
            // `snake_case_names` keep their underscores: emphasis with `_` starts at the edge of a word.
            let at_edge = mark == '*' || prev.is_none_or(|c| !c.is_alphanumeric());
            let body = &rest[1..];
            if at_edge
                && !body.starts_with(char::is_whitespace)
                && let Some(end) = body.find(mark).filter(|end| *end > 0 && !body[..*end].ends_with(char::is_whitespace))
                && (mark == '*' || body[end + 1..].chars().next().is_none_or(|c| !c.is_alphanumeric()))
            {
                let start = out.text.len();
                parse_inline(&body[..end], out, images, in_link);
                out.spans.push((start..out.text.len(), Style::Italic));
                i += 2 + end;
                continue;
            }
        }
        if !in_link
            && let Some(body) = rest.strip_prefix('[')
            && let Some((label, url, used)) = bracketed(body)
        {
            let start = out.text.len();
            parse_inline(label, out, images, true);
            if out.text.len() > start {
                out.spans.push((start..out.text.len(), Style::Link(url.to_owned())));
            }
            i += 1 + used;
            continue;
        }
        if !in_link && (rest.starts_with("https://") || rest.starts_with("http://")) && prev.is_none_or(|c| !c.is_alphanumeric()) {
            let end = rest.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'')).unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']);
            if url.len() > "https://".len() {
                let start = out.text.len();
                out.text.push_str(url);
                out.spans.push((start..out.text.len(), Style::Link(url.to_owned())));
                i += url.len();
                continue;
            }
        }
        let ch = rest.chars().next().unwrap_or(' ');
        // `\*` is a star.
        if ch == '\\' && rest[1..].chars().next().is_some_and(|c| c.is_ascii_punctuation()) {
            out.text.push(rest[1..].chars().next().unwrap_or(' '));
            i += 2;
            continue;
        }
        out.text.push(ch);
        i += ch.len_utf8();
    }
}

/// Carries on after a delimiter pair was read inside `parse_inline`'s loop.
fn parse_rest(src: &str, from: usize, out: &mut Text, images: &mut Vec<(usize, String, String)>, in_link: bool) {
    if from < src.len() {
        parse_inline(&src[from..], out, images, in_link);
    }
}

/// `label](address)` from the start of `body` (what follows an opening `[`): the label, the address, and how many bytes
/// were used. The label may hold brackets of its own, as `[![alt](img)](page)` does.
fn bracketed(body: &str) -> Option<(&str, &str, usize)> {
    let mut depth = 1;
    let mut close = None;
    for (at, c) in body.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(at);
                    break;
                }
            }
            '\n' if at > 400 => return None,
            _ => {}
        }
    }
    let close = close?;
    let after = body[close + 1..].strip_prefix('(')?;
    let end = after.find(')')?;
    let target = after[..end].split_whitespace().next()?;
    Some((&body[..close], target, close + 2 + end + 1))
}

// ---- drawing ------------------------------------------------------------------------------------------

/// What covers one stretch of text, once overlapping styles are put together.
#[derive(Clone, Default)]
struct Merged {
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    link: bool,
}

/// The text's styles as separate stretches: GPUI wants highlights that do not overlap.
fn highlights(text: &Text) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut points: Vec<usize> = vec![0, text.text.len()];
    for (range, _) in &text.spans {
        points.push(range.start.min(text.text.len()));
        points.push(range.end.min(text.text.len()));
    }
    points.sort_unstable();
    points.dedup();
    let mut found = Vec::new();
    for pair in points.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let mut merged = Merged::default();
        for (range, style) in &text.spans {
            if range.start <= start && end <= range.end {
                match style {
                    Style::Bold => merged.bold = true,
                    Style::Italic => merged.italic = true,
                    Style::Code => merged.code = true,
                    Style::Strike => merged.strike = true,
                    Style::Link(_) => merged.link = true,
                }
            }
        }
        if !(merged.bold || merged.italic || merged.code || merged.strike || merged.link) {
            continue;
        }
        let mut style = HighlightStyle::default();
        if merged.bold {
            style.font_weight = Some(FontWeight::BOLD);
            style.color = Some(rgb(t().text_strong).into());
        }
        if merged.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if merged.code {
            style.background_color = Some(rgb(t().element).into());
            style.color = Some(rgb(t().text_strong).into());
        }
        if merged.strike {
            style.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: None });
            style.color = Some(rgb(t().muted).into());
        }
        if merged.link {
            style.color = Some(rgb(t().accent).into());
            style.underline = Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false });
        }
        found.push((start..end, style));
    }
    found
}

/// A paragraph-like piece of text, with its links clickable.
fn text_element(id: (&'static str, usize), text: &Text) -> AnyElement {
    let styled = StyledText::new(text.text.clone()).with_highlights(highlights(text));
    let links: Vec<(Range<usize>, String)> = text
        .spans
        .iter()
        .filter_map(|(range, style)| match style {
            Style::Link(url) => Some((range.clone(), url.clone())),
            _ => None,
        })
        .collect();
    if links.is_empty() {
        return styled.into_any_element();
    }
    let ranges: Vec<Range<usize>> = links.iter().map(|(range, _)| range.clone()).collect();
    InteractiveText::new(id, styled)
        .on_click(ranges, move |ix, _, cx| {
            if let Some((_, url)) = links.get(ix) {
                cx.open_url(url);
            }
        })
        .into_any_element()
}

/// The blocks as one column, in the window's own text colors.
pub fn render(blocks: &[Block]) -> AnyElement {
    let mut column = div().flex().flex_col().gap(px(6.)).text_color(rgb(t().text));
    for (ix, block) in blocks.iter().enumerate() {
        column = column.child(match block {
            Block::Heading(level, text) => {
                let size = match level {
                    1 => 17.,
                    2 => 15.,
                    3 => 14.,
                    _ => 13.,
                };
                div()
                    .mt(px(if ix == 0 { 0. } else { 6. }))
                    .text_size(px(size))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(t().text_strong))
                    .child(text_element(("md-heading", ix), text))
                    .into_any_element()
            }
            Block::Paragraph(text) => div().child(text_element(("md-text", ix), text)).into_any_element(),
            Block::Quote(text) => div()
                .pl_2()
                .border_l_1()
                .border_color(rgb(t().border))
                .text_color(rgb(t().muted))
                .child(text_element(("md-quote", ix), text))
                .into_any_element(),
            Block::Item { depth, marker, text } => {
                let mark: AnyElement = match marker {
                    Marker::Bullet => div().size(px(4.)).rounded_full().bg(rgb(t().muted)).into_any_element(),
                    Marker::Number(n) => div().text_color(rgb(t().muted)).child(format!("{n}.")).into_any_element(),
                    Marker::Task(done) => div()
                        .size(px(12.))
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(if *done { t().accent } else { t().muted }))
                        .when(*done, |checked| checked.bg(rgb(t().accent)))
                        .into_any_element(),
                };
                div()
                    .flex()
                    .gap_2()
                    .ml(px((*depth as f32).min(4.) * 16.))
                    .child(div().flex_none().w(px(18.)).h(px(20.)).flex().items_center().justify_end().child(mark))
                    .child(div().min_w_0().flex_1().child(text_element(("md-item", ix), text)))
                    .into_any_element()
            }
            Block::Code { text, .. } => {
                let shown: String = text.lines().take(80).collect::<Vec<_>>().join("\n");
                let more = text.lines().count() > 80;
                div()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(t().border))
                    .bg(rgb(t().element))
                    .font_family(MONO)
                    .text_xs()
                    .text_color(rgb(t().text))
                    .child(if more { format!("{shown}\n…") } else { shown })
                    .into_any_element()
            }
            Block::Rule => div().my_1().h(px(1.)).bg(rgb(t().border)).into_any_element(),
            Block::Image { alt, url } => {
                let url = url.clone();
                let name = if alt.trim().is_empty() { url.rsplit('/').next().unwrap_or("image").to_owned() } else { alt.clone() };
                div()
                    .id(("md-image", ix))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(24.))
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(t().border))
                    .text_xs()
                    .text_color(rgb(t().muted))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(t().hover)))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .child("Image")
                    .child(div().min_w_0().flex_1().overflow_hidden().line_clamp(1).text_ellipsis().text_color(rgb(t().text)).child(name))
                    .child(div().text_color(rgb(t().accent)).child("Open"))
                    .into_any_element()
            }
        });
    }
    column.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .map(|block| match block {
                Block::Heading(_, t) | Block::Paragraph(t) | Block::Quote(t) | Block::Item { text: t, .. } => t.text.clone(),
                Block::Code { text, .. } => format!("code:{text}"),
                Block::Rule => "rule".to_owned(),
                Block::Image { url, .. } => format!("image:{url}"),
            })
            .collect()
    }

    #[test]
    fn headings_paragraphs_rules_and_code_are_told_apart() {
        let blocks = parse("# Title\n\nSome words\nover two lines.\n\n---\n\n```rust\nfn main() {}\n```\n\n## Next");
        assert_eq!(text(&blocks), ["Title", "Some words\nover two lines.", "rule", "code:fn main() {}", "Next"]);
        assert!(matches!(blocks[0], Block::Heading(1, _)) && matches!(blocks[4], Block::Heading(2, _)));
        // A hash with no space after it is not a heading.
        assert!(matches!(parse("#hashtag")[0], Block::Paragraph(_)));
    }

    #[test]
    fn lists_have_depth_numbers_and_tasks() {
        let blocks = parse("- one\n  - nested\n- [ ] todo\n- [x] done\n1. first\n2. second");
        let kinds: Vec<(usize, Marker)> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Item { depth, marker, .. } => Some((*depth, marker.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            kinds,
            [
                (0, Marker::Bullet),
                (1, Marker::Bullet),
                (0, Marker::Task(false)),
                (0, Marker::Task(true)),
                (0, Marker::Number(1)),
                (0, Marker::Number(2))
            ]
        );
        assert_eq!(text(&blocks)[2], "todo");
    }

    #[test]
    fn emphasis_code_and_links_become_styles_over_plain_text() {
        let blocks = parse("a **bold** and *slanted* and `code` and ~~gone~~ and [the docs](https://x.dev/a) end");
        let Block::Paragraph(t) = &blocks[0] else { panic!("a paragraph") };
        assert_eq!(t.text, "a bold and slanted and code and gone and the docs end");
        let styled = |style: &Style| -> Vec<&str> { t.spans.iter().filter(|(_, s)| s == style).map(|(r, _)| &t.text[r.clone()]).collect() };
        assert_eq!(styled(&Style::Bold), ["bold"]);
        assert_eq!(styled(&Style::Italic), ["slanted"]);
        assert_eq!(styled(&Style::Code), ["code"]);
        assert_eq!(styled(&Style::Strike), ["gone"]);
        assert_eq!(styled(&Style::Link("https://x.dev/a".into())), ["the docs"]);
    }

    #[test]
    fn a_bare_address_is_a_link_without_the_full_stop_after_it() {
        let blocks = parse("See https://developer.x.com/en/docs/cards/overview.\nNext line");
        let Block::Paragraph(t) = &blocks[0] else { panic!("a paragraph") };
        let link: Vec<&str> = t.spans.iter().filter(|(_, s)| matches!(s, Style::Link(_))).map(|(r, _)| &t.text[r.clone()]).collect();
        assert_eq!(link, ["https://developer.x.com/en/docs/cards/overview"]);
    }

    #[test]
    fn snake_case_and_stars_in_words_are_left_alone() {
        let blocks = parse("use some_snake_case_name and 2 * 3 * 4");
        let Block::Paragraph(t) = &blocks[0] else { panic!("a paragraph") };
        assert_eq!(t.text, "use some_snake_case_name and 2 * 3 * 4");
        assert!(t.spans.is_empty(), "{:?}", t.spans);
    }

    #[test]
    fn an_image_is_a_block_where_it_was_in_the_text() {
        let blocks = parse("Before\n![shot](https://github.com/user-attachments/assets/abc)\nAfter");
        assert_eq!(text(&blocks), ["Before", "image:https://github.com/user-attachments/assets/abc", "After"]);
        let badge = parse("[![build](https://img.shields.io/b.svg)](https://ci.example.com)");
        assert_eq!(text(&badge), ["image:https://img.shields.io/b.svg"]);
        let html = parse("<img width=\"200\" src=\"https://github.com/a/b.png\" alt=\"pic\">");
        assert!(matches!(&html[0], Block::Image { alt, url } if alt == "pic" && url == "https://github.com/a/b.png"));
    }

    #[test]
    fn comments_and_other_html_go_and_a_break_is_a_break() {
        let blocks = parse("<!-- fill this in -->\nHello<br>world <b>strong</b>\n<details>\n<summary>More</summary>\nhidden text\n</details>");
        assert_eq!(text(&blocks)[0], "Hello\nworld strong", "the break stays inside the paragraph, the bold tag goes");
        assert!(text(&blocks).iter().any(|t| t.starts_with("More")), "the summary stays, in bold");
        assert!(!text(&blocks).iter().any(|t| t.contains("fill this in")));
    }

    #[test]
    fn quotes_and_unclosed_fences_do_not_lose_text() {
        let blocks = parse("> quoted\n> two lines\n\n```\nnever closed");
        assert_eq!(text(&blocks), ["quoted\ntwo lines", "code:never closed"]);
    }

    #[test]
    fn the_highlights_do_not_overlap_when_styles_do() {
        let blocks = parse("**bold [link](https://x.dev) end**");
        let Block::Paragraph(t) = &blocks[0] else { panic!("a paragraph") };
        let found = highlights(t);
        for pair in found.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "{found:?}");
        }
        assert_eq!(t.text, "bold link end");
    }
}
