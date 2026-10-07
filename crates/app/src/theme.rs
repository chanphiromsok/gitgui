//! Color themes, read from Zed's theme format, so a theme made for Zed works here as it is.
//!
//! A theme file is a family (`{ "name", "author", "themes": [...] }`); each theme has an
//! `appearance` and a `style` of flat keys (`"panel.background": "#21252b"`) plus a `syntax` map from
//! capture names to colors. Keys a theme leaves out are worked out from the ones it has.
//!
//! Themes come from three places: the ones built in, `themes/` in the app's data folder, and the
//! themes of extensions installed in Zed. The chosen one is held in a global, so drawing code that
//! has no context to hand still finds its colors.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use gpui::{FontStyle, FontWeight, HighlightStyle};
use serde_json::{Map, Value};

use crate::syntax;

const BUNDLED: &[&str] = &[include_str!("../themes/gitgui.json"), include_str!("../themes/one.json")];

/// The theme used when none is chosen, or the chosen one is gone.
pub const DEFAULT: &str = "gitgui Dark";

/// Where a theme was read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    BuiltIn,
    /// A file in the app's data folder.
    User(PathBuf),
    /// A theme of an extension installed in Zed.
    Zed(PathBuf),
}

/// Every color the app draws with, as opaque `0xrrggbb`: translucent theme colors are laid over the
/// background they sit on when the theme is read.
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub dark: bool,
    pub origin: Origin,

    pub bg: u32,
    pub panel: u32,
    pub border: u32,
    pub text: u32,
    /// Headings and the current branch: a step brighter (or darker) than `text`.
    pub text_strong: u32,
    pub muted: u32,
    pub accent: u32,
    /// Text drawn on top of `accent`.
    pub on_accent: u32,
    pub link: u32,
    pub selected: u32,
    pub hover: u32,
    pub head_row: u32,
    pub added: u32,
    pub removed: u32,
    pub modified: u32,
    pub warning: u32,
    /// Buttons and badges.
    pub element: u32,
    pub element_hover: u32,
    /// Menus, dialogs and comment cards.
    pub card: u32,
    pub input_bg: u32,
    pub input_border: u32,
    pub guide: u32,

    pub editor_bg: u32,
    pub editor_fg: u32,
    pub line_number: u32,
    pub added_bg: u32,
    pub removed_bg: u32,
    /// The empty side of a split diff.
    pub empty_bg: u32,
    pub hunk_bg: u32,
    pub hunk_fg: u32,

    /// Colors for branch lines in the graph.
    pub lanes: Vec<u32>,
    /// A style for each name in [`syntax::NAMES`], after falling back to shorter names.
    pub syntax: Vec<Option<HighlightStyle>>,
}

static CURRENT: RwLock<Option<Arc<Theme>>> = RwLock::new(None);

/// The theme in use.
pub fn t() -> Arc<Theme> {
    if let Some(theme) = CURRENT.read().ok().and_then(|current| current.clone()) {
        return theme;
    }
    let theme = Arc::new(built_in().into_iter().find(|t| t.name == DEFAULT).expect("the default theme is built in"));
    if let Ok(mut current) = CURRENT.write() {
        current.get_or_insert_with(|| theme.clone());
    }
    theme
}

/// Makes `theme` the one in use.
pub fn set(theme: Arc<Theme>) {
    if let Ok(mut current) = CURRENT.write() {
        *current = Some(theme);
    }
}

impl Theme {
    /// The color of a branch line. Lines are numbered in the order they start, so neighbors differ.
    pub fn lane(&self, lineage: usize) -> u32 {
        self.lanes[lineage % self.lanes.len()]
    }

    /// A color that says what the theme's code looks like: its keyword color, else its accent.
    pub fn syntax_hint(&self) -> u32 {
        let keyword = syntax::NAMES.iter().position(|n| *n == "keyword").unwrap_or(0);
        self.syntax
            .get(keyword)
            .copied()
            .flatten()
            .and_then(|style| style.color)
            .map_or(self.accent, |color| u32::from(gpui::Rgba::from(color)) >> 8)
    }

    /// How to draw the capture at `index` in [`syntax::NAMES`].
    pub fn syntax_style(&self, index: u16) -> Option<HighlightStyle> {
        self.syntax.get(index as usize).copied().flatten()
    }
}

// ---- reading --------------------------------------------------------------------------------

/// `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` → (0xrrggbb, alpha 0–1).
fn parse_color(text: &str) -> Option<(u32, f32)> {
    let hex = text.trim().strip_prefix('#')?;
    let expand = |s: &str| s.chars().flat_map(|c| [c, c]).collect::<String>();
    let full = match hex.len() {
        3 | 4 => expand(hex),
        6 | 8 => hex.to_owned(),
        _ => return None,
    };
    let value = u32::from_str_radix(&full, 16).ok()?;
    Some(if full.len() == 8 { (value >> 8, (value & 0xff) as f32 / 255.) } else { (value, 1.) })
}

/// `color` with `alpha`, laid over `under`.
fn over(color: u32, alpha: f32, under: u32) -> u32 {
    let channel = |shift: u32| {
        let top = ((color >> shift) & 0xff) as f32;
        let bottom = ((under >> shift) & 0xff) as f32;
        ((top * alpha + bottom * (1. - alpha)).round() as u32).min(255) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// Part of the way (by `amount`) from `from` to `to`.
pub fn mix(from: u32, to: u32, amount: f32) -> u32 {
    over(to, amount, from)
}

/// Relative luminance, as WCAG defines it: 0 for black, 1 for white.
fn luminance(color: u32) -> f32 {
    let channel = |shift: u32| {
        let c = ((color >> shift) & 0xff) as f32 / 255.;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// Dark or light text, whichever reads on `background`.
pub fn text_on(background: u32) -> u32 {
    if luminance(background) > 0.3 { 0x111111 } else { 0xffffff }
}

/// Reads one theme from a Zed `style` object.
fn from_style(name: String, dark: bool, origin: Origin, style: &Map<String, Value>) -> Theme {
    let raw = |key: &str| style.get(key).and_then(Value::as_str).and_then(parse_color);
    let first = |keys: &[&str]| keys.iter().find_map(|key| raw(key));

    let bg = first(&["editor.background", "background"]).map_or(if dark { 0x1e1e1e } else { 0xfafafa }, |(c, a)| {
        over(c, a, if dark { 0 } else { 0xffffff })
    });
    // Any other color, laid over the background.
    let color = |keys: &[&str], fallback: u32| first(keys).map_or(fallback, |(c, a)| over(c, a, bg));

    let text = color(&["text", "editor.foreground"], if dark { 0xd4d4d4 } else { 0x383a42 });
    let muted = color(&["text.muted", "editor.line_number"], mix(text, bg, 0.4));
    let panel = color(&["panel.background", "surface.background", "background"], bg);
    let border = color(&["border.variant", "border"], mix(bg, text, 0.15));
    let accent = color(&["text.accent", "border.focused", "icon.accent"], if dark { 0x4fc1ff } else { 0x4078f2 });
    let added = color(&["created", "version_control.added", "success"], 0x4ec9b0);
    let removed = color(&["deleted", "version_control.deleted", "error"], 0xf48771);
    let modified = color(&["modified", "version_control.modified", "warning"], 0xe5c07b);
    let editor_bg = color(&["editor.background", "background"], bg);
    // Translucent change colors are laid over the editor's own background.
    let tint = |keys: &[&str], base: u32| first(keys).map_or(mix(editor_bg, base, 0.16), |(c, a)| over(c, a, editor_bg));
    let lanes: Vec<u32> = style
        .get("accents")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).filter_map(parse_color).map(|(c, a)| over(c, a, bg)).collect())
        .filter(|list: &Vec<u32>| list.len() >= 3)
        .unwrap_or_else(|| vec![accent, 0xc586c0, 0xdcdcaa, added, 0xce9178, 0x569cd6, 0xb5cea8, removed]);

    let syntax_map = style.get("syntax").and_then(Value::as_object);
    let style_of = |name: &str| -> Option<HighlightStyle> {
        let entry = syntax_map?.get(name)?.as_object()?;
        let color = entry.get("color").and_then(Value::as_str).and_then(parse_color);
        let italic = entry.get("font_style").and_then(Value::as_str) == Some("italic");
        let weight = entry.get("font_weight").and_then(Value::as_f64);
        if color.is_none() && !italic && weight.is_none() {
            return None;
        }
        Some(HighlightStyle {
            color: color.map(|(c, a)| gpui::rgb(over(c, a, editor_bg)).into()),
            font_style: italic.then_some(FontStyle::Italic),
            font_weight: weight.map(|w| FontWeight(w as f32)),
            ..Default::default()
        })
    };
    // `function.method` → `function.method`, else `function`.
    let syntax = syntax::NAMES
        .iter()
        .map(|name| {
            let mut name: &str = name;
            loop {
                if let Some(style) = style_of(name) {
                    return Some(style);
                }
                name = name.rsplit_once('.')?.0;
            }
        })
        .collect();

    Theme {
        name,
        dark,
        origin,
        bg,
        panel,
        border,
        text,
        text_strong: mix(text, if dark { 0xffffff } else { 0x000000 }, 0.6),
        muted,
        accent,
        on_accent: text_on(accent),
        link: color(&["link_text.hover", "text.accent"], accent),
        selected: color(&["element.selected", "ghost_element.selected"], mix(bg, accent, 0.3)),
        hover: color(&["ghost_element.hover", "element.hover"], mix(bg, text, 0.06)),
        head_row: color(&["editor.active_line.background"], mix(bg, accent, 0.1)),
        added,
        removed,
        modified,
        warning: color(&["warning"], modified),
        element: color(&["element.background"], mix(bg, text, 0.15)),
        element_hover: color(&["element.hover"], mix(bg, text, 0.22)),
        card: color(&["elevated_surface.background", "surface.background"], panel),
        input_bg: color(&["editor.background"], bg),
        input_border: color(&["border"], border),
        guide: color(&["editor.indent_guide", "editor.wrap_guide"], mix(bg, text, 0.2)),
        editor_bg,
        editor_fg: color(&["editor.foreground", "text"], text),
        line_number: color(&["editor.line_number"], muted),
        added_bg: tint(&["created.background", "version_control.added.background"], added),
        removed_bg: tint(&["deleted.background", "version_control.deleted.background"], removed),
        empty_bg: color(&["editor.gutter.background"], mix(editor_bg, text, 0.03)),
        hunk_bg: color(&["editor.subheader.background"], mix(editor_bg, accent, 0.08)),
        hunk_fg: color(&["text.accent"], accent),
        lanes,
        syntax,
    }
}

/// Every theme in one Zed theme file. A file that does not read as one gives none.
pub fn parse_family(json: &str, origin: Origin) -> Vec<Theme> {
    let Ok(family) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let Some(themes) = family.get("themes").and_then(Value::as_array) else { return Vec::new() };
    themes
        .iter()
        .filter_map(|theme| {
            let name = theme.get("name")?.as_str()?.to_owned();
            let dark = theme.get("appearance").and_then(Value::as_str) != Some("light");
            let style = theme.get("style")?.as_object()?;
            Some(from_style(name, dark, origin.clone(), style))
        })
        .collect()
}

fn built_in() -> Vec<Theme> {
    BUNDLED.iter().flat_map(|json| parse_family(json, Origin::BuiltIn)).collect()
}

/// The `.json` files in `dir`.
fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> =
        entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    files
}

/// Where Zed keeps installed extensions.
pub fn zed_extensions() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut roots = Vec::new();
    if let Some(home) = &home {
        roots.push(home.join("Library/Application Support/Zed/extensions/installed"));
        roots.push(home.join(".local/share/zed/extensions/installed"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join("Zed/extensions/installed"));
    }
    let mut extensions: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flat_map(|entries| entries.flatten().map(|e| e.path()))
        .filter(|path| path.is_dir())
        .collect();
    extensions.sort();
    extensions
}

/// Every theme there is: built in, then the user's (from `data_dir/themes` and Zed's own themes
/// folder), then Zed extensions'. Later ones with a name already taken are left out.
pub fn all(data_dir: Option<&Path>) -> Vec<Arc<Theme>> {
    let mut themes = built_in();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let user_dirs = data_dir.map(|d| d.join("themes")).into_iter().chain(home.map(|h| h.join(".config/zed/themes")));
    for file in user_dirs.flat_map(|dir| json_files(&dir)) {
        if let Ok(json) = std::fs::read_to_string(&file) {
            themes.extend(parse_family(&json, Origin::User(file)));
        }
    }
    for file in zed_extensions().iter().flat_map(|ext| json_files(&ext.join("themes"))) {
        if let Ok(json) = std::fs::read_to_string(&file) {
            themes.extend(parse_family(&json, Origin::Zed(file)));
        }
    }
    let mut seen = std::collections::HashSet::new();
    themes.into_iter().filter(|theme| seen.insert(theme.name.clone())).map(Arc::new).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_read_in_every_hex_form() {
        assert_eq!(parse_color("#abc"), Some((0xaabbcc, 1.)));
        assert_eq!(parse_color("#1e1e1e"), Some((0x1e1e1e, 1.)));
        assert_eq!(parse_color("#ff000080").map(|(c, a)| (c, (a * 100.).round())), Some((0xff0000, 50.)));
        assert_eq!(parse_color("red"), None);
        assert_eq!(over(0xffffff, 0.5, 0x000000), 0x808080);
    }

    #[test]
    fn the_built_in_themes_read_and_the_default_matches_the_old_colors() {
        let themes = built_in();
        let names: Vec<&str> = themes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["gitgui Dark", "One Dark", "One Light"]);
        let default = &themes[0];
        assert_eq!((default.bg, default.panel, default.text, default.muted, default.accent), (0x1e1e1e, 0x252526, 0xd4d4d4, 0x858585, 0x4fc1ff));
        assert_eq!((default.added_bg, default.removed_bg), (0x1d3b2c, 0x4b2326));
        assert_eq!(default.lane(0), 0x4fc1ff);
        assert!(!themes[2].dark);
        assert_eq!(themes[2].on_accent, 0xffffff, "white on a dark accent");
    }

    #[test]
    fn a_zed_theme_with_few_keys_still_has_every_color_and_names_fall_back() {
        let json = r##"{ "name": "Mini", "author": "me", "themes": [ { "name": "Mini Light", "appearance": "light",
            "style": { "background": "#ffffff", "text": "#000000", "created": "#00aa0040",
                       "syntax": { "function": { "color": "#0000ff" }, "comment": { "font_style": "italic" } } } } ] }"##;
        let [theme] = &parse_family(json, Origin::BuiltIn)[..] else { panic!("one theme") };
        assert!(!theme.dark);
        assert_eq!(theme.added, over(0x00aa00, 0x40 as f32 / 255., 0xffffff));
        let at = |name: &str| syntax::NAMES.iter().position(|n| *n == name).unwrap() as u16;
        let method = theme.syntax_style(at("function.method")).unwrap();
        assert_eq!(method.color, Some(gpui::rgb(0x0000ff).into()));
        assert_eq!(theme.syntax_style(at("comment")).unwrap().font_style, Some(FontStyle::Italic));
        assert!(theme.syntax_style(at("keyword")).is_none());
    }

    #[test]
    fn a_file_that_is_not_a_theme_gives_none() {
        assert!(parse_family("not json", Origin::BuiltIn).is_empty());
        assert!(parse_family("{\"themes\": 3}", Origin::BuiltIn).is_empty());
    }
}
