//! Embeds the Material Icon Theme's SVGs: a sorted table of (icon name, bytes) the app looks up.

use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/material-icons/icons");
    println!("cargo:rerun-if-changed=assets/material-icons");
    let mut icons: Vec<(String, String)> = std::fs::read_dir(&dir)
        .expect("assets/material-icons/icons")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
        .map(|path| (path.file_stem().unwrap().to_string_lossy().into_owned(), path.display().to_string()))
        .collect();
    icons.sort();
    let mut out = String::from("/// Material icon name → SVG, sorted by name.\npub static MATERIAL_ICONS: &[(&str, &[u8])] = &[\n");
    for (name, path) in &icons {
        out.push_str(&format!("    ({name:?}, include_bytes!({path:?})),\n"));
    }
    out.push_str("];\n");
    let target = Path::new(&std::env::var("OUT_DIR").unwrap()).join("material_icons.rs");
    std::fs::write(target, out).expect("write the icon table");
}
