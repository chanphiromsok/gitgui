//! File and folder icons: the Material Icon Theme built in, or an icon theme installed in Zed.
//!
//! The built-in set is PKief's Material Icon Theme for VS Code (MIT; `assets/material-icons`), with
//! its own table of which icon goes with which file, read the way VS Code reads it. Zed icon themes
//! (`icon_themes/*.json` in an extension) map file names and suffixes to SVG files in the extension.
//! Both are drawn in full color.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use gpui::RenderImage;
use serde_json::Value;

use crate::theme;

/// The built-in icons' name in the picker.
pub const BUILT_IN: &str = "Material Icon Theme";

include!(concat!(env!("OUT_DIR"), "/material_icons.rs"));

/// An icon theme from a Zed extension.
#[derive(Clone, Debug, Default)]
pub struct IconTheme {
    pub name: String,
    /// The extension folder; icon paths are relative to it.
    root: PathBuf,
    folder: Option<String>,
    folder_open: Option<String>,
    named_folders: HashMap<String, (Option<String>, Option<String>)>,
    stems: HashMap<String, String>,
    suffixes: HashMap<String, String>,
    icons: HashMap<String, String>,
}

impl IconTheme {
    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative.trim_start_matches("./"))
    }

    /// The icon file for a file named `name`, as Zed picks it: the whole name, then each suffix from
    /// the longest (`d.ts` before `ts`), then the default.
    fn file(&self, name: &str) -> Option<PathBuf> {
        let lower = name.to_ascii_lowercase();
        let by_stem = self.stems.get(name).or_else(|| self.stems.get(&lower)).or_else(|| {
            let stem = name.split('.').next().unwrap_or(name);
            self.stems.get(stem)
        });
        let by_suffix = || {
            lower.char_indices().filter(|(_, c)| *c == '.').find_map(|(at, _)| self.suffixes.get(&lower[at + 1..]))
        };
        let key = by_stem.or_else(by_suffix).map_or("default", String::as_str);
        let icon = self.icons.get(key).or_else(|| self.icons.get("default"))?;
        Some(self.path(icon))
    }

    fn folder(&self, name: &str, open: bool) -> Option<PathBuf> {
        let named = self.named_folders.get(name).or_else(|| self.named_folders.get(&name.to_ascii_lowercase()));
        let pick = |pair: (&Option<String>, &Option<String>)| if open { pair.1.clone() } else { pair.0.clone() };
        named
            .and_then(|(closed, opened)| pick((closed, opened)))
            .or_else(|| pick((&self.folder, &self.folder_open)))
            .map(|relative| self.path(&relative))
    }
}

/// Every icon theme in Zed's installed extensions.
pub fn all() -> Vec<Arc<IconTheme>> {
    let mut found = Vec::new();
    for extension in theme::zed_extensions() {
        let Ok(entries) = std::fs::read_dir(extension.join("icon_themes")) else { continue };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
        files.sort();
        for file in files {
            let Ok(json) = std::fs::read_to_string(&file) else { continue };
            found.extend(parse(&json, &extension));
        }
    }
    let mut seen = std::collections::HashSet::new();
    found.into_iter().filter(|t| seen.insert(t.name.clone())).map(Arc::new).collect()
}

/// The icon themes in one Zed icon theme file, whose icons live under `root`.
pub fn parse(json: &str, root: &Path) -> Vec<IconTheme> {
    let Ok(family) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let Some(themes) = family.get("themes").and_then(Value::as_array) else { return Vec::new() };
    let strings = |value: Option<&Value>| -> HashMap<String, String> {
        value
            .and_then(Value::as_object)
            .map(|map| map.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
            .unwrap_or_default()
    };
    let pair = |value: &Value| {
        let get = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        (get("collapsed"), get("expanded"))
    };
    themes
        .iter()
        .filter_map(|theme| {
            let name = theme.get("name")?.as_str()?.to_owned();
            let (folder, folder_open) = theme.get("directory_icons").map(pair).unwrap_or_default();
            Some(IconTheme {
                name,
                root: root.to_path_buf(),
                folder,
                folder_open,
                named_folders: theme
                    .get("named_directory_icons")
                    .and_then(Value::as_object)
                    .map(|map| map.iter().map(|(k, v)| (k.clone(), pair(v))).collect())
                    .unwrap_or_default(),
                stems: strings(theme.get("file_stems")),
                suffixes: strings(theme.get("file_suffixes")),
                icons: theme
                    .get("file_icons")
                    .and_then(Value::as_object)
                    .map(|map| {
                        map.iter().filter_map(|(k, v)| Some((k.clone(), v.get("path")?.as_str()?.to_owned()))).collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

// ---- which set is in use ----------------------------------------------------------------------

static CURRENT: RwLock<Option<Arc<IconTheme>>> = RwLock::new(None);

/// Uses `theme` for icons, or the built-in set for `None`.
pub fn set(theme: Option<Arc<IconTheme>>) {
    if let Ok(mut current) = CURRENT.write() {
        *current = theme;
    }
    if let Ok(mut cache) = cache().lock() {
        cache.clear();
    }
}

fn current() -> Option<Arc<IconTheme>> {
    CURRENT.read().ok().and_then(|current| current.clone())
}

/// Icons are drawn this many pixels square, so they stay sharp when shown small on a dense screen.
const PIXELS: u32 = 48;

fn cache() -> &'static Mutex<HashMap<String, Arc<RenderImage>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<RenderImage>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The image for `key`, drawn once from the SVG `make` gives.
fn cached(key: String, make: impl FnOnce() -> Option<String>) -> Option<Arc<RenderImage>> {
    let mut cache = cache().lock().ok()?;
    if let Some(image) = cache.get(&key) {
        return Some(image.clone());
    }
    let image = rasterize(&make()?, PIXELS)?;
    cache.insert(key, image.clone());
    Some(image)
}

/// Draws `svg` with its longer side `size` pixels, in the pixel order GPUI draws: blue, green, red,
/// alpha, not premultiplied. (GPUI turns PNGs into that order, but not SVGs, so they come out with
/// red and blue swapped: a blue React icon drawn orange.)
pub fn rasterize(svg: &str, size: u32) -> Option<Arc<RenderImage>> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).ok()?;
    let view = tree.size();
    let scale = size as f32 / view.width().max(view.height());
    let (width, height) = ((view.width() * scale).round().max(1.) as u32, (view.height() * scale).round().max(1.) as u32);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let bytes: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let c = pixel.demultiply();
            [c.blue(), c.green(), c.red(), c.alpha()]
        })
        .collect();
    let buffer = image::RgbaImage::from_raw(width, height, bytes)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// An SVG file from an icon theme, with `currentColor` in the theme's muted text color.
fn from_disk(path: &Path) -> Option<String> {
    let svg = std::fs::read_to_string(path).ok()?;
    Some(svg.replace("currentColor", &format!("#{:06x}", theme::t().muted)))
}

/// A small cloud in `color`: the branch is on a remote.
pub fn remote(color: u32) -> Arc<RenderImage> {
    let c = format!("#{color:06x}");
    cached(format!("remote:{color:06x}"), || {
        Some(format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M4.6 12.8H11.9Q14.6 12.8 14.6 10.2Q14.6 7.7 12 7.5Q11.5 4 8.3 4Q5.6 4 4.8 6.6Q1.4 6.9 1.4 9.8Q1.4 12.8 4.6 12.8Z" fill="{c}"/></svg>"#
        ))
    })
    .unwrap_or_else(|| Arc::new(RenderImage::new(Vec::new())))
}

/// A small flag in `color`: a tag.
pub fn flag(color: u32) -> Arc<RenderImage> {
    let c = format!("#{color:06x}");
    cached(format!("flag:{color:06x}"), || {
        Some(format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M3.6 14.2V2" stroke="{c}" stroke-width="1.6" stroke-linecap="round" fill="none"/><path d="M4.2 2.6H13L10.7 5.9L13 9.2H4.2Z" fill="{c}"/></svg>"#
        ))
    })
    .unwrap_or_else(|| Arc::new(RenderImage::new(Vec::new())))
}

/// The show/hide sidebar button: a window outline, its left panel filled in while the sidebar shows.
pub fn sidebar(shown: bool) -> Option<Arc<RenderImage>> {
    let color = theme::t().muted;
    cached(format!("sidebar:{shown}:{color:06x}"), || {
        let c = format!("#{color:06x}");
        let panel = if shown { format!(r#"<rect x="2.5" y="3" width="4" height="10" rx="0.6" fill="{c}"/>"#) } else { String::new() };
        Some(format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect x="1.5" y="2.5" width="13" height="11" rx="2" fill="none" stroke="{c}" stroke-width="1.2"/><path d="M6.5 2.5V13.5" stroke="{c}" stroke-width="1.2"/>{panel}</svg>"#
        ))
    })
}

/// The icon for a file named `name`.
pub fn file(name: &str) -> Option<Arc<RenderImage>> {
    if let Some(theme) = current()
        && let Some(path) = theme.file(name)
    {
        return cached(path.display().to_string(), || from_disk(&path));
    }
    material(material_table().file(name, !theme::t().dark))
}

/// The icon for a folder named `name` (the last folder, for a chain like `api / query`).
pub fn folder(name: &str, open: bool) -> Option<Arc<RenderImage>> {
    let name = name.rsplit(" / ").next().unwrap_or(name);
    if let Some(theme) = current()
        && let Some(path) = theme.folder(name, open)
    {
        return cached(path.display().to_string(), || from_disk(&path));
    }
    material(material_table().folder(name, open, !theme::t().dark))
}

// ---- the built-in set: Material Icon Theme -------------------------------------------------------

const MATERIAL_TABLE: &str = include_str!("../assets/material-icons/material-icons.json");

/// One of Material's name → icon tables, with the light-theme overrides beside it.
#[derive(Default)]
struct Names {
    dark: HashMap<String, String>,
    light: HashMap<String, String>,
}

impl Names {
    fn get(&self, key: &str, light: bool) -> Option<&String> {
        light.then(|| self.light.get(key)).flatten().or_else(|| self.dark.get(key))
    }
}

/// Material's `material-icons.json`: which icon a file or folder gets.
#[derive(Default)]
struct Material {
    file_names: Names,
    extensions: Names,
    language_ids: Names,
    folders: Names,
    folders_open: Names,
    file: String,
    folder: String,
    folder_open: String,
    /// Icon name → the SVG file it is drawn from, without `.svg` (`folder-scrap` → `folder-scrap.clone`).
    files: HashMap<String, String>,
}

fn material_table() -> &'static Material {
    static TABLE: OnceLock<Material> = OnceLock::new();
    TABLE.get_or_init(|| {
        let Ok(json) = serde_json::from_str::<Value>(MATERIAL_TABLE) else { return Material::default() };
        let map = |value: Option<&Value>| -> HashMap<String, String> {
            value
                .and_then(Value::as_object)
                .map(|m| m.iter().filter_map(|(k, v)| Some((k.to_ascii_lowercase(), v.as_str()?.to_owned()))).collect())
                .unwrap_or_default()
        };
        let names = |key: &str| Names { dark: map(json.get(key)), light: map(json.get("light").and_then(|l| l.get(key))) };
        let name = |key: &str, fallback: &str| json.get(key).and_then(Value::as_str).unwrap_or(fallback).to_owned();
        Material {
            file_names: names("fileNames"),
            extensions: names("fileExtensions"),
            language_ids: names("languageIds"),
            folders: names("folderNames"),
            folders_open: names("folderNamesExpanded"),
            file: name("file", "file"),
            folder: name("folder", "folder"),
            folder_open: name("folderExpanded", "folder-open"),
            files: json
                .get("iconDefinitions")
                .and_then(Value::as_object)
                .map(|defs| {
                    defs.iter()
                        .filter_map(|(id, def)| {
                            let path = def.get("iconPath")?.as_str()?;
                            let file = path.rsplit('/').next()?.strip_suffix(".svg")?;
                            Some((id.clone(), file.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    })
}

impl Material {
    /// The icon for a file, as VS Code picks it: the whole name, then each extension from the
    /// longest (`test.ts` before `ts`), then the file's language, then the plain file icon.
    fn file(&self, name: &str, light: bool) -> &str {
        let lower = name.to_ascii_lowercase();
        let by_name = || self.file_names.get(&lower, light);
        let by_extension = || {
            lower.char_indices().filter(|(_, c)| *c == '.').find_map(|(at, _)| self.extensions.get(&lower[at + 1..], light))
        };
        let by_language = || language_id(&lower).and_then(|id| self.language_ids.get(id, light));
        by_name().or_else(by_extension).or_else(by_language).map_or(&self.file, String::as_str)
    }

    fn folder(&self, name: &str, open: bool, light: bool) -> &str {
        let lower = name.to_ascii_lowercase();
        let (named, plain) = if open { (&self.folders_open, &self.folder_open) } else { (&self.folders, &self.folder) };
        named.get(&lower, light).map_or(plain, String::as_str)
    }
}

/// The VS Code language of a file whose extension Material leaves to the language table.
fn language_id(lower: &str) -> Option<&'static str> {
    let ext = lower.rsplit_once('.').map(|(_, ext)| ext)?;
    Some(match ext {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "json" => "json",
        "jsonc" => "jsonc",
        "md" => "markdown",
        "py" => "python",
        "rs" => "rust",
        "go" => "go",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "java" => "java",
        "rb" => "ruby",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "html" | "htm" => "html",
        "xml" => "xml",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "sh" | "bash" | "zsh" => "shellscript",
        "php" => "php",
        "sql" => "sql",
        "dart" => "dart",
        "lua" => "lua",
        "m" => "objective-c",
        "mm" => "objective-cpp",
        _ => return None,
    })
}

/// The embedded SVG of a Material icon.
fn material_svg(name: &str) -> Option<&'static [u8]> {
    let file = material_table().files.get(name).map_or(name, String::as_str);
    let at = MATERIAL_ICONS.binary_search_by(|(icon, _)| (*icon).cmp(file)).ok()?;
    Some(MATERIAL_ICONS[at].1)
}

/// A Material icon by name, drawn once.
fn material(name: &str) -> Option<Arc<RenderImage>> {
    let bytes = material_svg(name)?;
    cached(format!("material:{name}"), || Some(std::str::from_utf8(bytes).ok()?.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_get_material_icons_by_name_then_extension_then_language() {
        let table = material_table();
        let file = |name: &str| table.file(name, false);
        assert_eq!(file("useQueryMembership.ts"), "typescript");
        assert_eq!(file("TierProgress.tsx"), "react_ts");
        assert_eq!(file("index.d.ts"), "typescript-def", "the longest extension wins");
        assert_eq!(file("api.test.ts"), "test-ts");
        assert_eq!(file("package.json"), "nodejs", "a known name beats its extension");
        assert_eq!(file("messages.po"), "i18n");
        assert_eq!(file("yarn.lock"), "yarn");
        assert_eq!(file("README"), "readme");
        assert_eq!(file("notes"), "file");
        assert_eq!(table.folder("src", false, false), "folder-src");
        assert_eq!(table.folder("locales", true, false), "folder-i18n-open");
        assert_eq!(table.folder("membership", false, false), "folder");
        assert_eq!(table.file("Cargo.toml", true), "toml_light", "light themes get the light variant");
    }

    #[test]
    fn every_icon_the_table_names_is_embedded_and_parses() {
        let table = material_table();
        let named = [&table.file_names, &table.extensions, &table.language_ids, &table.folders, &table.folders_open]
            .into_iter()
            .flat_map(|names| names.dark.values().chain(names.light.values()));
        let mut missing: Vec<&String> = named.filter(|name| material_svg(name).is_none()).collect();
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "icons named but not embedded: {missing:?}");
        assert!(MATERIAL_ICONS.windows(2).all(|pair| pair[0].0 < pair[1].0), "sorted for lookup");
        for (name, bytes) in MATERIAL_ICONS {
            assert!(rasterize(std::str::from_utf8(bytes).unwrap(), 16).is_some(), "{name} does not draw");
        }
    }

    #[test]
    fn icons_are_drawn_blue_green_red_alpha_as_gpui_expects() {
        // Material's React TypeScript icon is blue (#0288d1).
        let svg = std::str::from_utf8(material_svg("react_ts").unwrap()).unwrap();
        let image = rasterize(svg, PIXELS).unwrap();
        let bytes = image.as_bytes(0).unwrap();
        assert_eq!(bytes.len(), (PIXELS * PIXELS * 4) as usize);
        let solid = bytes.chunks_exact(4).find(|p| p[3] == 255).expect("an opaque pixel");
        assert_eq!(solid, [0xd1, 0x88, 0x02, 0xff], "blue first, red third");
    }

    #[test]
    fn a_zed_icon_theme_picks_by_name_then_longest_suffix_then_default() {
        let json = r#"{ "name": "Icons", "themes": [ { "name": "Test Icons", "appearance": "dark",
            "directory_icons": { "collapsed": "./icons/folder.svg", "expanded": "./icons/folder-open.svg" },
            "named_directory_icons": { "src": { "collapsed": "./icons/src.svg", "expanded": "./icons/src-open.svg" } },
            "file_stems": { "Dockerfile": "docker" },
            "file_suffixes": { "ts": "typescript", "d.ts": "typings" },
            "file_icons": { "docker": { "path": "./icons/docker.svg" }, "typescript": { "path": "./icons/ts.svg" },
                            "typings": { "path": "./icons/dts.svg" }, "default": { "path": "./icons/file.svg" } } } ] }"#;
        let [theme] = &parse(json, Path::new("/ext"))[..] else { panic!("one icon theme") };
        assert_eq!(theme.name, "Test Icons");
        assert_eq!(theme.file("Dockerfile"), Some(PathBuf::from("/ext/icons/docker.svg")));
        assert_eq!(theme.file("index.d.ts"), Some(PathBuf::from("/ext/icons/dts.svg")));
        assert_eq!(theme.file("App.ts"), Some(PathBuf::from("/ext/icons/ts.svg")));
        assert_eq!(theme.file("notes.xyz"), Some(PathBuf::from("/ext/icons/file.svg")));
        assert_eq!(theme.folder("src", true), Some(PathBuf::from("/ext/icons/src-open.svg")));
        assert_eq!(theme.folder("other", false), Some(PathBuf::from("/ext/icons/folder.svg")));
    }
}
