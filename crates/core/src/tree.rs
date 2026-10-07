//! Lays a commit's changed files out as a folder tree or a flat list, optionally filtered.

use std::collections::{BTreeMap, HashSet};

use crate::model::FileChange;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeRow {
    /// A folder. A chain of folders that each hold only the next one is one row: `api / query`.
    /// `path` is the whole folder path (`src/api/query`), which names it for folding.
    Dir { depth: usize, name: String, path: String },
    /// A file; `index` points into the slice given to [`file_tree`] or [`visible_rows`]. `dir` is the
    /// folder it is in, filled in only for a flat list, where the row has to say where the file is.
    File { depth: usize, name: String, dir: String, index: usize },
}

/// How the changed files are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Tree,
    Flat,
}

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<(String, usize)>,
}

/// Folders first, then files, each sorted by name. Rows are in display order, so a list can draw
/// them top to bottom with `depth` as the indent.
pub fn file_tree(changes: &[FileChange]) -> Vec<TreeRow> {
    let mut root = Node::default();
    for (index, change) in changes.iter().enumerate() {
        let mut parts: Vec<&str> = change.path.split('/').collect();
        let name = parts.pop().unwrap_or_default().to_owned();
        let mut node = &mut root;
        for part in parts {
            node = node.dirs.entry(part.to_owned()).or_default();
        }
        node.files.push((name, index));
    }
    let mut rows = Vec::new();
    flatten(&root, "", 0, &mut rows);
    rows
}

fn flatten(node: &Node, parent: &str, depth: usize, rows: &mut Vec<TreeRow>) {
    for (name, child) in &node.dirs {
        let (mut label, mut node) = (name.clone(), child);
        let mut path = if parent.is_empty() { name.clone() } else { format!("{parent}/{name}") };
        while node.files.is_empty() && node.dirs.len() == 1 {
            let (next_name, next) = node.dirs.iter().next().expect("one child");
            label = format!("{label} / {next_name}");
            path = format!("{path}/{next_name}");
            node = next;
        }
        rows.push(TreeRow::Dir { depth, name: label, path: path.clone() });
        flatten(node, &path, depth + 1, rows);
    }
    let mut files: Vec<&(String, usize)> = node.files.iter().collect();
    files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.0.cmp(&b.0)));
    for (name, index) in files {
        rows.push(TreeRow::File { depth, name: name.clone(), dir: String::new(), index: *index });
    }
}

/// Every file on its own row, sorted by path, each with its folder beside its name.
pub fn flat_list(changes: &[FileChange]) -> Vec<TreeRow> {
    let mut order: Vec<usize> = (0..changes.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&changes[a].path, &changes[b].path);
        a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b))
    });
    order
        .into_iter()
        .map(|index| {
            let path = &changes[index].path;
            let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
            TreeRow::File { depth: 0, name: name.to_owned(), dir: dir.to_owned(), index }
        })
        .collect()
}

/// The rows to show for `changes` in `layout`, keeping only files whose path contains `filter`
/// (ignoring case; a blank filter keeps everything). `index` always points into `changes`.
/// What is inside a folder in `folded` is left out, unless a filter is on: then every match shows.
pub fn visible_rows(changes: &[FileChange], filter: &str, layout: Layout, folded: &HashSet<String>) -> Vec<TreeRow> {
    let needle = filter.trim().to_lowercase();
    let kept: Vec<usize> = (0..changes.len())
        .filter(|&i| needle.is_empty() || changes[i].path.to_lowercase().contains(&needle))
        .collect();
    let subset: Vec<FileChange> = kept.iter().map(|&i| changes[i].clone()).collect();
    let rows = match layout {
        Layout::Tree => file_tree(&subset),
        Layout::Flat => flat_list(&subset),
    };
    let mut shown = Vec::with_capacity(rows.len());
    // The depth of the folded folder being skipped, while inside one.
    let mut skipping: Option<usize> = None;
    for row in rows {
        let depth = match &row {
            TreeRow::Dir { depth, .. } | TreeRow::File { depth, .. } => *depth,
        };
        if skipping.is_some_and(|folded_at| depth > folded_at) {
            continue;
        }
        skipping = match &row {
            TreeRow::Dir { depth, path, .. } if needle.is_empty() && folded.contains(path) => Some(*depth),
            _ => None,
        };
        shown.push(match row {
            TreeRow::File { depth, name, dir, index } => TreeRow::File { depth, name, dir, index: kept[index] },
            dir => dir,
        });
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FileStatus;

    fn change(path: &str) -> FileChange {
        FileChange { path: path.into(), old_path: None, status: FileStatus::Modified, additions: Some(1), deletions: Some(0) }
    }

    fn show(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                TreeRow::Dir { depth, name, .. } => format!("{}{name}/", "  ".repeat(*depth)),
                TreeRow::File { depth, name, dir, index } if dir.is_empty() => {
                    format!("{}{name} #{index}", "  ".repeat(*depth))
                }
                TreeRow::File { name, dir, index, .. } => format!("{name} ({dir}) #{index}"),
            })
            .collect()
    }

    #[test]
    fn a_folded_folder_hides_what_is_inside_it_unless_a_filter_is_on() {
        let changes = [change("src/api/query/a.ts"), change("src/b.ts"), change("src/components/c.tsx"), change("d.md")];
        let folded: HashSet<String> = ["src/api/query".to_owned()].into();
        assert_eq!(
            show(&visible_rows(&changes, "", Layout::Tree, &folded)),
            ["src/", "  api / query/", "  components/", "    c.tsx #2", "  b.ts #1", "d.md #3"]
        );
        let folded: HashSet<String> = ["src".to_owned()].into();
        assert_eq!(show(&visible_rows(&changes, "", Layout::Tree, &folded)), ["src/", "d.md #3"]);
        assert_eq!(show(&visible_rows(&changes, "a.ts", Layout::Tree, &folded)), ["src / api / query/", "  a.ts #0"]);
        let paths: Vec<String> = file_tree(&changes)
            .into_iter()
            .filter_map(|row| match row {
                TreeRow::Dir { path, .. } => Some(path),
                TreeRow::File { .. } => None,
            })
            .collect();
        assert_eq!(paths, ["src", "src/api/query", "src/components"]);
    }

    #[test]
    fn folders_come_first_single_child_chains_collapse_and_files_sort_by_name() {
        let changes = [
            change("src/components/POI.tsx"),
            change("src/api/query/useQueryBookings.ts"),
            change("src/components/poi/StripRoute.tsx"),
            change("README.md"),
            change("src/components/Button.tsx"),
            change("src/components/poi/RoutePieces.tsx"),
            change("src/features/bookings/index.ts"),
        ];
        assert_eq!(
            show(&file_tree(&changes)),
            [
                "src/",
                "  api / query/",
                "    useQueryBookings.ts #1",
                "  components/",
                "    poi/",
                "      RoutePieces.tsx #5",
                "      StripRoute.tsx #2",
                "    Button.tsx #4",
                "    POI.tsx #0",
                "  features / bookings/",
                "    index.ts #6",
                "README.md #3",
            ]
        );
    }

    #[test]
    fn a_lone_file_in_a_deep_folder_is_one_folder_row_and_one_file_row() {
        assert_eq!(show(&file_tree(&[change("a/b/c/d.txt")])), ["a / b / c/", "  d.txt #0"]);
    }

    #[test]
    fn a_folder_that_holds_a_file_and_a_folder_does_not_collapse() {
        let rows = show(&file_tree(&[change("a/x.txt"), change("a/b/y.txt")]));
        assert_eq!(rows, ["a/", "  b/", "    y.txt #1", "  x.txt #0"]);
    }

    #[test]
    fn root_files_and_an_empty_list() {
        assert_eq!(show(&file_tree(&[change("b.txt"), change("A.txt")])), ["A.txt #1", "b.txt #0"]);
        assert!(file_tree(&[]).is_empty());
    }

    #[test]
    fn every_change_appears_exactly_once() {
        let changes: Vec<FileChange> = (0..20).map(|i| change(&format!("d{}/e{}/f{i}.rs", i % 3, i % 2))).collect();
        let mut seen: Vec<usize> = file_tree(&changes)
            .into_iter()
            .filter_map(|row| match row {
                TreeRow::File { index, .. } => Some(index),
                TreeRow::Dir { .. } => None,
            })
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn a_flat_list_is_sorted_by_path_and_names_each_folder() {
        let changes = [change("src/b.rs"), change("README.md"), change("src/a/z.rs"), change("Src/Aa.rs")];
        // Sorted by whole path, ignoring case: `src/a/z.rs` comes before `Src/Aa.rs` because `/` sorts before `a`.
        assert_eq!(
            show(&flat_list(&changes)),
            ["README.md #1", "z.rs (src/a) #2", "Aa.rs (Src) #3", "b.rs (src) #0"]
        );
    }

    #[test]
    fn the_filter_matches_any_part_of_the_path_ignoring_case() {
        let changes = [change("src/Button.tsx"), change("src/api/query.ts"), change("docs/button.md")];
        let kept = |rows: Vec<TreeRow>| -> Vec<usize> {
            rows.into_iter()
                .filter_map(|row| match row {
                    TreeRow::File { index, .. } => Some(index),
                    TreeRow::Dir { .. } => None,
                })
                .collect()
        };
        assert_eq!(kept(visible_rows(&changes, "BUTTON", Layout::Flat, &HashSet::new())), [2, 0]);
        assert_eq!(kept(visible_rows(&changes, "api/", Layout::Tree, &HashSet::new())), [1]);
        assert_eq!(kept(visible_rows(&changes, "  ", Layout::Tree, &HashSet::new())).len(), 3, "blank filter keeps everything");
        assert!(visible_rows(&changes, "nothing like this", Layout::Tree, &HashSet::new()).is_empty());
    }

    #[test]
    fn filtered_rows_still_index_the_full_list_and_drop_empty_folders() {
        let changes = [change("a/x.rs"), change("b/y.rs"), change("b/z.rs")];
        let rows = visible_rows(&changes, "z.rs", Layout::Tree, &HashSet::new());
        assert_eq!(show(&rows), ["b/", "  z.rs #2"], "index 2 is z.rs in the original list; folder a is gone");
    }
}
