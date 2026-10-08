//! Showing a branch's commits under the pull request (or merge) they belong to.
//!
//! The graph lists commits by date, so the commits of a merged pull request are scattered among the
//! trunk's own. Grouping lists them right under their merge instead, one level in. A squash-merged
//! branch has no merge commit, so its commits go under the squash commit.
//!
//! This only reorders. Every commit still appears exactly once and no commit is ever listed above a
//! commit it descends from (the layout needs children first), so a group is only formed when nothing
//! outside it depends on its commits.

use std::collections::{HashMap, HashSet};

use crate::graph::LaneLayout;
use crate::model::Commit;

/// One commit's place in the grouped list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// Index into the commits that were given.
    pub index: usize,
    /// How many groups deep: 0 for a commit on its own, 1 for a commit under a pull request.
    pub depth: usize,
    /// The commit this one is listed under.
    pub under: Option<usize>,
}

/// Puts each branch's commits under the commit they belong to.
///
/// `commits` are newest first, children before parents. `squashed` maps the tip commit of a branch
/// that was squash-merged to the id of the squash commit that carries its changes.
pub fn group_by_parent(commits: &[Commit], squashed: &HashMap<String, String>) -> Vec<Placed> {
    let n = commits.len();
    let index_of: HashMap<&str, usize> = commits.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();

    let mut layout = LaneLayout::new();
    let rows: Vec<_> = commits.iter().map(|c| layout.push(&c.id, &c.parents)).collect();
    let lineages = layout.lineages();

    let mut members: Vec<Vec<usize>> = vec![Vec::new(); lineages.len()];
    for (i, row) in rows.iter().enumerate() {
        members[row.lineage].push(i);
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, commit) in commits.iter().enumerate() {
        for parent in commit.parents.iter().filter_map(|p| index_of.get(p.as_str())) {
            children[*parent].push(i);
        }
    }

    // For each line, the row it would be listed under, if it may be.
    let mut under: Vec<Option<usize>> = vec![None; n]; // for a group's first member: its parent row
    let mut groups: HashMap<usize, Vec<Vec<usize>>> = HashMap::new(); // parent row -> groups, in line order
    let mut grouped = vec![false; n];
    for (line, group) in lineages.iter().zip(&members) {
        let Some(&first) = group.first() else { continue };
        let parent = line.opened_by_merge.or_else(|| {
            let tip = &commits[first];
            squashed.get(&tip.id).and_then(|squash| index_of.get(squash.as_str()).copied())
        });
        let Some(parent) = parent.filter(|&p| p < first) else { continue };
        let inside: HashSet<usize> = group.iter().copied().collect();
        // Nothing but the group itself and its parent may descend from the group's commits, or
        // moving them up would put a commit above one of its children.
        let closed = group.iter().all(|&i| children[i].iter().all(|&c| c == parent || inside.contains(&c)));
        // A commit already under another group is not moved twice.
        if !closed || group.iter().any(|&i| grouped[i]) {
            continue;
        }
        for &i in group {
            grouped[i] = true;
        }
        under[first] = Some(parent);
        groups.entry(parent).or_default().push(group.clone());
    }

    fn emit(
        i: usize,
        depth: usize,
        parent: Option<usize>,
        groups: &HashMap<usize, Vec<Vec<usize>>>,
        out: &mut Vec<Placed>,
    ) {
        out.push(Placed { index: i, depth, under: parent });
        for group in groups.get(&i).into_iter().flatten() {
            for &member in group {
                emit(member, depth + 1, Some(i), groups, out);
            }
        }
    }
    let mut out = Vec::with_capacity(n);
    for (i, _) in grouped.iter().enumerate().filter(|(_, grouped)| !**grouped) {
        emit(i, 0, None, &groups, &mut out);
    }
    out
}

/// The commits listed under `parent`, directly or nested, as indices into the grouped list.
pub fn descendants(placed: &[Placed], parent_position: usize) -> std::ops::Range<usize> {
    let depth = placed[parent_position].depth;
    let end = placed[parent_position + 1..]
        .iter()
        .position(|p| p.depth <= depth)
        .map_or(placed.len(), |offset| parent_position + 1 + offset);
    parent_position + 1..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.into(),
            parents: parents.iter().map(|p| (*p).to_owned()).collect(),
            author: "A".into(),
            email: "a@b".into(),
            time: 0,
            date: String::new(),
            summary: id.into(),
            refs: Vec::new(),
            stash: None,
            committer: String::new(),
            committer_email: String::new(),
        }
    }

    fn history(items: &[(&str, &[&str])]) -> Vec<Commit> {
        items.iter().map(|(id, parents)| commit(id, parents)).collect()
    }

    fn order(commits: &[Commit], placed: &[Placed]) -> Vec<String> {
        placed.iter().map(|p| format!("{}{}", "  ".repeat(p.depth), commits[p.index].id)).collect()
    }

    #[test]
    fn a_merged_branchs_commits_move_under_their_merge_and_the_trunk_keeps_its_order() {
        // In date order the branch's commits are spread among the trunk's: b2, t2, b1.
        let commits = history(&[
            ("t3", &["m"]),
            ("m", &["t2", "b2"]),
            ("b2", &["b1"]),
            ("t2", &["t1"]),
            ("b1", &["t1"]),
            ("t1", &[]),
        ]);
        let placed = group_by_parent(&commits, &HashMap::new());
        assert_eq!(order(&commits, &placed), ["t3", "m", "  b2", "  b1", "t2", "t1"]);
        assert_eq!(placed[2].under, Some(1), "b2 is listed under m");
        assert_eq!(placed[0].under, None);
    }

    #[test]
    fn a_squash_merged_branch_goes_under_its_squash_commit() {
        let commits = history(&[
            ("s", &["t1"]), // the squash commit on the trunk
            ("f2", &["f1"]),
            ("f1", &["t1"]),
            ("t1", &[]),
        ]);
        let squashed = HashMap::from([("f2".to_owned(), "s".to_owned())]);
        let placed = group_by_parent(&commits, &squashed);
        assert_eq!(order(&commits, &placed), ["s", "  f2", "  f1", "t1"]);

        // Without the squash clue nothing is grouped.
        let flat = group_by_parent(&commits, &HashMap::new());
        assert_eq!(order(&commits, &flat), ["s", "f2", "f1", "t1"]);
    }

    #[test]
    fn nested_groups_indent_one_level_each() {
        // m1 merges the feature line (fm -> f1); at fm that line had merged g (g1) in.
        let commits = history(&[
            ("m1", &["t1", "fm"]),
            ("fm", &["f1", "g1"]),
            ("g1", &["t1"]),
            ("f1", &["t1"]),
            ("t1", &[]),
        ]);
        let placed = group_by_parent(&commits, &HashMap::new());
        assert_eq!(order(&commits, &placed), ["m1", "  fm", "    g1", "  f1", "t1"]);
        assert_eq!(placed[2].under, Some(1), "g1 is under fm, which is under m1");
    }

    #[test]
    fn a_group_is_left_alone_when_something_else_descends_from_one_of_its_commits() {
        // `other` was branched from b1 inside the merged branch, so moving b1 up would put it above `other`.
        let commits = history(&[
            ("m", &["t1", "b2"]),
            ("other", &["b1"]),
            ("b2", &["b1"]),
            ("b1", &["t1"]),
            ("t1", &[]),
        ]);
        let placed = group_by_parent(&commits, &HashMap::new());
        assert!(placed.iter().all(|p| p.depth == 0), "{:?}", order(&commits, &placed));
        assert_eq!(placed.iter().map(|p| p.index).collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn descendants_covers_a_group_and_its_nested_groups() {
        let placed = [
            Placed { index: 0, depth: 0, under: None },
            Placed { index: 1, depth: 1, under: Some(0) },
            Placed { index: 2, depth: 2, under: Some(1) },
            Placed { index: 3, depth: 1, under: Some(0) },
            Placed { index: 4, depth: 0, under: None },
        ];
        assert_eq!(descendants(&placed, 0), 1..4);
        assert_eq!(descendants(&placed, 1), 2..3);
        assert_eq!(descendants(&placed, 4), 5..5);
    }

    /// A small deterministic generator, so the random histories are the same on every run.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, below: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % below.max(1)
        }
    }

    #[test]
    fn on_random_histories_every_commit_appears_once_and_no_parent_comes_before_its_child() {
        for seed in 0..300u64 {
            let mut rng = Lcg(seed + 1);
            let n = 3 + rng.next(40);
            // Commit i may only have parents with a larger index, so the list is children first.
            let commits: Vec<Commit> = (0..n)
                .map(|i| {
                    let mut parents = Vec::new();
                    if i + 1 < n {
                        parents.push(format!("c{}", i + 1 + rng.next((n - i - 1).min(4))));
                        if rng.next(4) == 0 {
                            let second = format!("c{}", i + 1 + rng.next(n - i - 1));
                            if !parents.contains(&second) {
                                parents.push(second);
                            }
                        }
                    }
                    Commit { parents, ..commit(&format!("c{i}"), &[]) }
                })
                .collect();
            let squashed: HashMap<String, String> = if rng.next(2) == 0 {
                HashMap::from([(format!("c{}", rng.next(n)), format!("c{}", rng.next(n)))])
            } else {
                HashMap::new()
            };
            let placed = group_by_parent(&commits, &squashed);

            let mut seen: Vec<usize> = placed.iter().map(|p| p.index).collect();
            seen.sort_unstable();
            assert_eq!(seen, (0..n).collect::<Vec<_>>(), "seed {seed}: not a permutation");

            let position: HashMap<&str, usize> =
                placed.iter().enumerate().map(|(pos, p)| (commits[p.index].id.as_str(), pos)).collect();
            for (pos, p) in placed.iter().enumerate() {
                for parent in &commits[p.index].parents {
                    assert!(
                        position[parent.as_str()] > pos,
                        "seed {seed}: {} is listed after its parent {parent}",
                        commits[p.index].id
                    );
                }
                if let Some(under) = p.under {
                    assert!(placed.iter().position(|q| q.index == under).unwrap() < pos, "seed {seed}: group before its parent");
                }
            }
        }
    }
}
