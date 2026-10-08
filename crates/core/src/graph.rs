//! Commit-graph lane layout.
//!
//! Commits arrive newest first, parents after children. [`LaneLayout`] keeps only the lanes still
//! waiting for a parent, so history can be laid out in chunks: push a page of commits, render it,
//! push the next page later.
//!
//! Every line in the graph belongs to a *lineage*: one branch's run of commits, from its tip down to
//! where it forks off another. A lineage keeps one id, and so one color, for its whole length. A
//! branch's line is never bent into its parent early; it runs down to the commit it was branched
//! from, and that commit is where it ends, so the fork point is visible.
//!
//! A line only changes lane at a commit's row: it leaves a merge sideways into its lane, or comes
//! down its lane into the commit it was branched from. Everywhere else it runs straight down, so the
//! lanes read as even, parallel lines and every bend sits beside the commit it belongs to.

/// Which part of a row a graph line crosses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    /// From the row's top into its commit.
    Top,
    /// From the commit down to the row's bottom.
    Bottom,
    /// Past the commit, top to bottom.
    Through,
}

/// A line of the graph in one row, from a lane to a lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stroke {
    pub from: usize,
    pub to: usize,
    pub half: Half,
    /// The branch line this stroke belongs to.
    pub lineage: usize,
}

/// One commit's row: its lane, the strokes through it, and how many lanes it spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub lane: usize,
    /// The branch line this commit is on.
    pub lineage: usize,
    pub strokes: Vec<Stroke>,
    pub width: usize,
    /// Branch lines that end at this commit because they were branched from it: the commit is
    /// their fork point.
    pub joins: Vec<usize>,
}

/// One branch's run of commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lineage {
    /// The row of its newest commit.
    pub tip_row: usize,
    /// The row of its oldest commit so far.
    pub last_row: usize,
    /// How many commits are on it.
    pub commits: usize,
    /// For a line that starts as a merge's second parent, the row of that merge.
    pub opened_by_merge: Option<usize>,
    /// The row of the commit this line was branched from, once that row has been laid out.
    pub fork_row: Option<usize>,
    /// The line that commit is on.
    pub base: Option<usize>,
    /// Which line wins a fork point: the higher rank carries on through it (a trunk beats a feature).
    pub rank: i32,
}

struct Wait {
    id: String,
    lineage: usize,
}

#[derive(Default)]
pub struct LaneLayout {
    /// For each lane, the commit it is waiting to reach and the line it belongs to.
    waiting: Vec<Option<Wait>>,
    lineages: Vec<Lineage>,
    /// For each lineage, the branches it is the line of (empty when it is not known yet).
    branches: Vec<Vec<String>>,
    rows: usize,
}

impl LaneLayout {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lanes still open below the last pushed row.
    pub fn open_lanes(&self) -> usize {
        self.waiting.len()
    }

    /// The branch lines seen so far, indexed by lineage id.
    pub fn lineages(&self) -> &[Lineage] {
        &self.lineages
    }

    /// Lays out the next commit with no preference between lines.
    pub fn push<S: AsRef<str>>(&mut self, id: &str, parents: &[S]) -> Row {
        self.push_ranked(id, parents, 0)
    }

    /// Lays out the next commit. `rank` is how trunk-like the branch at this commit is, and only
    /// matters when the commit starts a new line.
    pub fn push_ranked<S: AsRef<str>>(&mut self, id: &str, parents: &[S], rank: i32) -> Row {
        self.push_branch(id, parents, rank, &[] as &[&str])
    }

    /// Lays out the next commit, which the `branches` point at (names without a remote prefix).
    ///
    /// A commit takes the lane of the line waiting for it; if several wait, the highest ranked one
    /// carries on (ties go to the older line) and the others end here. A line does not carry on into
    /// the tip of a branch that outranks it, unless it is that branch's line: a feature line that
    /// reaches the tip of the trunk it was cut from ends there, bending into a fresh lane where the
    /// trunk's line starts, so the fork point shows. With none carrying on the commit starts
    /// a line in the first free lane. Its first parent carries on in its lane. Any other parent
    /// joins the line already running down to it, if there is one; otherwise it starts a new line,
    /// in the free lane its bend reaches crossing the fewest lines, which runs down to that parent.
    pub fn push_branch<S: AsRef<str>, B: AsRef<str>>(&mut self, id: &str, parents: &[S], rank: i32, branches: &[B]) -> Row {
        let row = self.rows;
        self.rows += 1;
        let before = self.waiting.len();
        let branches: Vec<String> = branches.iter().map(|b| b.as_ref().to_owned()).collect();

        let waiting_here: Vec<usize> = (0..self.waiting.len())
            .filter(|&at| self.waiting[at].as_ref().is_some_and(|wait| wait.id == id))
            .collect();
        // A line only stops at the tip of a more trunk-like branch than its own. A trunk runs on through
        // the tips of branches that were fast-forwarded or rebased into it: those commits are its own.
        let carries_on = |line: usize| {
            let own = &self.branches[line];
            branches.is_empty()
                || own.is_empty()
                || self.lineages[line].rank >= rank
                || own.iter().any(|name| branches.contains(name))
        };
        let (lane, lineage) = match waiting_here
            .iter()
            .copied()
            .filter(|&at| carries_on(self.waiting[at].as_ref().map_or(0, |wait| wait.lineage)))
            .max_by_key(|&at| {
                let line = self.waiting[at].as_ref().map_or(0, |wait| wait.lineage);
                (self.lineages[line].rank, std::cmp::Reverse(line), std::cmp::Reverse(at))
            }) {
            Some(at) => (at, self.waiting[at].as_ref().map_or(0, |wait| wait.lineage)),
            None => {
                // Lanes of the lines ending here are still taken, so this is a lane of its own.
                let lane = self.take_free_lane();
                (lane, self.start_line(row, None, rank))
            }
        };
        if self.branches[lineage].is_empty() && !branches.is_empty() {
            // A line opened by a merge learns its branch, and how trunk-like it is, at its first commit with one.
            self.branches[lineage] = branches;
            let line = &mut self.lineages[lineage];
            line.rank = line.rank.max(rank);
        }

        let mut strokes = Vec::new();
        let mut joins = Vec::new();
        for (at, wait) in self.waiting.iter().enumerate() {
            let Some(wait) = wait else { continue };
            if wait.id == id {
                strokes.push(Stroke { from: at, to: lane, half: Half::Top, lineage: wait.lineage });
                if wait.lineage != lineage {
                    joins.push(wait.lineage);
                }
            } else {
                strokes.push(Stroke { from: at, to: at, half: Half::Through, lineage: wait.lineage });
            }
        }
        for wait in self.waiting.iter_mut().filter(|wait| wait.as_ref().is_some_and(|w| w.id == id)) {
            *wait = None;
        }

        {
            let line = &mut self.lineages[lineage];
            if line.commits == 0 {
                // A line that began as a merge's second parent only has a tip once its first commit arrives.
                line.tip_row = row;
            }
            line.commits += 1;
            line.last_row = row;
        }
        for &joined in &joins {
            let ended = &mut self.lineages[joined];
            ended.fork_row = Some(row);
            ended.base = Some(lineage);
        }

        for (ix, parent) in parents.iter().map(AsRef::as_ref).enumerate() {
            if ix == 0 {
                self.waiting[lane] = Some(Wait { id: parent.to_owned(), lineage });
                strokes.push(Stroke { from: lane, to: lane, half: Half::Bottom, lineage });
                continue;
            }
            let running: Vec<usize> = (0..self.waiting.len())
                .filter(|&at| self.waiting[at].as_ref().is_some_and(|wait| wait.id == parent))
                .collect();
            if running.contains(&lane) {
                // The same parent twice: the commit's own line already goes there.
                continue;
            }
            if let Some(at) = running.into_iter().min_by_key(|&at| (self.crossings(lane, at), at.abs_diff(lane))) {
                // A line already runs down to this parent (an earlier merge brought the same branch in, or the
                // branch went on after it was merged): the merge bends into that line, in its color, instead of
                // opening a lane beside it.
                let joined = self.waiting[at].as_ref().map_or(0, |wait| wait.lineage);
                strokes.push(Stroke { from: lane, to: at, half: Half::Bottom, lineage: joined });
            } else {
                let free = self.merge_lane(lane);
                let started = self.start_line(row, Some(row), 0);
                self.waiting[free] = Some(Wait { id: parent.to_owned(), lineage: started });
                strokes.push(Stroke { from: lane, to: free, half: Half::Bottom, lineage: started });
            }
        }

        let width = before.max(self.waiting.len()).max(lane + 1);
        while matches!(self.waiting.last(), Some(None)) {
            self.waiting.pop();
        }
        Row { lane, lineage, strokes, width, joins }
    }

    fn start_line(&mut self, tip_row: usize, opened_by_merge: Option<usize>, rank: i32) -> usize {
        self.lineages.push(Lineage {
            tip_row,
            last_row: tip_row,
            commits: 0,
            opened_by_merge,
            fork_row: None,
            base: None,
            rank,
        });
        self.branches.push(Vec::new());
        self.lineages.len() - 1
    }

    fn take_free_lane(&mut self) -> usize {
        self.waiting.iter().position(Option::is_none).unwrap_or_else(|| {
            self.waiting.push(None);
            self.waiting.len() - 1
        })
    }

    /// How many lines a bend from lane `a` across to lane `b` crosses: the lanes between them that are taken.
    fn crossings(&self, a: usize, b: usize) -> usize {
        let (low, high) = (a.min(b), a.max(b));
        (low + 1..high).filter(|&at| self.waiting.get(at).is_some_and(Option::is_some)).count()
    }

    /// The lane for a line a merge in `lane` opens. Its bend runs across at the merge's row, so the lane is the free
    /// one it reaches crossing the fewest lines; then one inside the lanes in use rather than a new one; then one to
    /// the right, where a branch is looked for; then the nearest.
    fn merge_lane(&mut self, lane: usize) -> usize {
        let end = self.waiting.len();
        let best = (0..=end)
            .filter(|&at| at == end || self.waiting[at].is_none())
            .min_by_key(|&at| (self.crossings(lane, at), at == end, at < lane, at.abs_diff(lane)))
            .unwrap_or(end);
        if best == end {
            self.waiting.push(None);
        }
        best
    }
}

/// Lays out a whole history at once.
pub fn lanes<'a>(commits: impl IntoIterator<Item = (&'a str, &'a [&'a str])>) -> Vec<Row> {
    let mut layout = LaneLayout::new();
    commits.into_iter().map(|(id, parents)| layout.push(id, parents)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(from: usize, to: usize, half: Half, lineage: usize) -> Stroke {
        Stroke { from, to, half, lineage }
    }

    #[test]
    fn a_line_of_commits_keeps_one_lane_and_one_lineage() {
        let rows = lanes([("c", &["b"][..]), ("b", &["a"][..]), ("a", &[][..])]);
        assert!(rows.iter().all(|row| row.lane == 0 && row.width == 1 && row.lineage == 0 && row.joins.is_empty()));
        assert_eq!(rows[1].strokes, [stroke(0, 0, Half::Top, 0), stroke(0, 0, Half::Bottom, 0)]);
    }

    #[test]
    fn a_merged_branch_runs_down_to_where_it_forked_instead_of_bending_early() {
        use Half::*;
        // m merges a (first parent) and b; both come from c.
        let rows = lanes([
            ("m", &["a", "b"][..]),
            ("a", &["c"][..]),
            ("b", &["c"][..]),
            ("c", &[][..]),
        ]);
        let lanes: Vec<usize> = rows.iter().map(|row| row.lane).collect();
        assert_eq!(lanes, [0, 0, 1, 0]);
        // m: its own line continues in lane 0; the merged branch starts in lane 1 as a new line.
        assert_eq!(rows[0].strokes, [stroke(0, 0, Bottom, 0), stroke(0, 1, Bottom, 1)]);
        assert_eq!(rows[1].strokes, [stroke(0, 0, Top, 0), stroke(1, 1, Through, 1), stroke(0, 0, Bottom, 0)]);
        // b stays in lane 1, and lane 0 keeps waiting for c: neither bends early.
        assert_eq!(rows[2].strokes, [stroke(0, 0, Through, 0), stroke(1, 1, Top, 1), stroke(1, 1, Bottom, 1)]);
        // c is where they meet: the merged line ends here and c is its fork point.
        assert_eq!(rows[3].strokes, [stroke(0, 0, Top, 0), stroke(1, 0, Top, 1)]);
        assert_eq!(rows[3].joins, [1]);
        assert_eq!(rows[2].width, 2);
    }

    #[test]
    fn a_lineage_records_where_it_started_ended_and_what_it_forked_from() {
        let mut layout = LaneLayout::new();
        for (id, parents) in [("m", &["a", "b"][..]), ("a", &["c"]), ("b", &["c"]), ("c", &[])] {
            layout.push(id, parents);
        }
        let [trunk, merged] = layout.lineages() else { panic!("two lineages, got {:?}", layout.lineages()) };
        assert_eq!((trunk.tip_row, trunk.commits, trunk.fork_row, trunk.base), (0, 3, None, None));
        assert_eq!(
            (merged.tip_row, merged.last_row, merged.commits, merged.opened_by_merge, merged.fork_row, merged.base),
            (2, 2, 1, Some(0), Some(3), Some(0))
        );
    }

    #[test]
    fn a_feature_branch_is_drawn_down_to_the_commit_it_was_branched_from() {
        // feat: f2 -> f1 -> t2; trunk: t3 -> t2 -> t1. The trunk tip is newer than f1 but older than f2.
        let mut layout = LaneLayout::new();
        let rows: Vec<Row> = [
            ("f2", &["f1"][..], 10),
            ("t3", &["t2"][..], 100),
            ("f1", &["t2"][..], 0),
            ("t2", &["t1"][..], 0),
            ("t1", &[][..], 0),
        ]
        .into_iter()
        .map(|(id, parents, rank)| layout.push_ranked(id, parents, rank))
        .collect();

        // f1 is still on the feature's lane; the line does not bend into the trunk there.
        assert_eq!((rows[2].lane, rows[2].lineage), (0, 0));
        assert!(rows[2].joins.is_empty());
        // At t2 the two lines meet. The trunk (higher rank) carries on; the feature line ends: t2 is its fork point.
        assert_eq!(rows[3].lineage, 1, "the trunk carries on through its fork points");
        assert_eq!(rows[3].joins, [0]);
        assert_eq!(layout.lineages()[0].fork_row, Some(3));
        assert_eq!(layout.lineages()[0].base, Some(1));
        // And t2 sits on the trunk's lane, with the feature's line curving into it.
        assert_eq!(rows[3].lane, 1);
        assert!(rows[3].strokes.contains(&stroke(0, 1, Half::Top, 0)));
    }

    #[test]
    fn a_feature_line_ends_at_the_tip_of_the_branch_it_was_cut_from() {
        // feat: f2 -> f1 -> r1 (the tip of release) -> r0. Only the feature line reaches r1.
        let mut layout = LaneLayout::new();
        let rows: Vec<Row> = [
            ("f2", &["f1"][..], 10, &["feat/x"][..]),
            ("f1", &["r1"][..], 0, &[][..]),
            ("r1", &["r0"][..], 60, &["release/1.0.0", "feat/old"][..]),
            ("r0", &[][..], 0, &[][..]),
        ]
        .into_iter()
        .map(|(id, parents, rank, branches)| layout.push_branch(id, parents, rank, branches))
        .collect();

        assert_eq!((rows[1].lane, rows[1].lineage), (0, 0));
        // Release starts its own line in a lane of its own, and the feature bends into it and ends.
        assert_eq!((rows[2].lane, rows[2].lineage, rows[2].joins.clone()), (1, 1, vec![0]));
        assert!(rows[2].strokes.contains(&stroke(0, 1, Half::Top, 0)));
        assert_eq!(layout.lineages()[0].fork_row, Some(2));
        assert_eq!((rows[3].lane, rows[3].lineage), (1, 1));
        assert_eq!(layout.open_lanes(), 0);
    }

    #[test]
    fn a_line_carries_on_through_its_own_branch_and_learns_a_branch_from_its_first_commit() {
        // A local branch ahead of its remote side; a merged-in line reaching a branch tip.
        let mut layout = LaneLayout::new();
        let rows: Vec<Row> = [
            ("m", &["a", "b"][..], 60, &["release"][..]),
            ("a", &["c"][..], 10, &["release"][..]),
            ("b", &["x"][..], 10, &["feat/y"][..]),
            ("x", &["c"][..], 10, &["feat/z"][..]),
            ("c", &[][..], 0, &[][..]),
        ]
        .into_iter()
        .map(|(id, parents, rank, branches)| layout.push_branch(id, parents, rank, branches))
        .collect();

        assert_eq!(rows[1].lineage, 0, "release carries on into its own remote side");
        assert_eq!(rows[2].lineage, 1, "the merged-in line is the line of the branch it reaches");
        assert_eq!(rows[3].lineage, 1, "a branch stacked on one of the same rank stays on its line");
        assert!(rows[3].joins.is_empty());
    }

    #[test]
    fn a_trunk_runs_on_through_the_tips_of_branches_fast_forwarded_into_it() {
        // main: m2 -> fix/b's tip -> fix/a's tip -> m0, all one line of history.
        let mut layout = LaneLayout::new();
        let rows: Vec<Row> = [
            ("m2", &["b"][..], 100, &["main"][..]),
            ("b", &["a"][..], 10, &["fix/b"][..]),
            ("a", &["m0"][..], 10, &["fix/a", "fix/c"][..]),
            ("m0", &[][..], 0, &[][..]),
        ]
        .into_iter()
        .map(|(id, parents, rank, branches)| layout.push_branch(id, parents, rank, branches))
        .collect();
        assert!(rows.iter().all(|row| row.lane == 0 && row.lineage == 0 && row.joins.is_empty()));
        assert_eq!(layout.lineages().len(), 1);
    }

    #[test]
    fn with_equal_ranks_the_older_line_carries_on() {
        let rows = lanes([("b", &["a"][..]), ("c", &["a"][..]), ("a", &[][..])]);
        assert_eq!(rows[1].lane, 1);
        assert_eq!(rows[1].strokes, [stroke(0, 0, Half::Through, 0), stroke(1, 1, Half::Bottom, 1)]);
        assert_eq!((rows[2].lane, rows[2].lineage, rows[2].joins.clone()), (0, 0, vec![1]));
        assert_eq!(rows[2].strokes, [stroke(0, 0, Half::Top, 0), stroke(1, 0, Half::Top, 1)]);
    }

    #[test]
    fn a_second_merge_of_the_same_branch_bends_into_the_line_already_running_to_it() {
        use Half::*;
        // main (m1) and develop (d1) both merge release (r); release was cut from t1.
        let mut layout = LaneLayout::new();
        let rows: Vec<Row> = [
            ("m1", &["t1", "r"][..]),
            ("d1", &["t2", "r"][..]),
            ("r", &["t1"][..]),
            ("t2", &["t1"][..]),
            ("t1", &[][..]),
        ]
        .into_iter()
        .map(|(id, parents)| layout.push(id, parents))
        .collect();
        assert_eq!(rows[0].strokes, [stroke(0, 0, Bottom, 0), stroke(0, 1, Bottom, 1)]);
        // develop starts in lane 2 and its merge bends left into release's lane, in release's color: no new lane.
        assert_eq!(rows[1].lane, 2);
        assert_eq!(
            rows[1].strokes,
            [stroke(0, 0, Through, 0), stroke(1, 1, Through, 1), stroke(2, 2, Bottom, 2), stroke(2, 1, Bottom, 1)]
        );
        assert_eq!(rows[1].width, 3);
        assert_eq!(layout.lineages().len(), 3, "main, release and develop; the second merge opened no line");
        assert!(rows[2].joins.is_empty(), "r is release's own commit, not a fork point of the merge");
        assert_eq!(rows[4].joins, [1, 2]);
    }

    #[test]
    fn a_merge_opens_its_line_where_the_bend_crosses_the_fewest_lines() {
        // Lanes 0 and 1 wait for p, 2 for q, 3 for r. At p lane 1 ends and frees up; then r, in lane 3, merges s.
        let rows = lanes([
            ("a", &["p"][..]),
            ("b", &["p"][..]),
            ("c", &["q"][..]),
            ("e", &["r"][..]),
            ("p", &["z"][..]),
            ("r", &["z", "s"][..]),
            ("q", &["z"][..]),
            ("s", &["z"][..]),
            ("z", &[][..]),
        ]);
        assert_eq!(rows[5].lane, 3);
        // Lane 1 is free, but reaching it would cross lane 2; the bend goes right instead, crossing nothing.
        assert!(rows[5].strokes.contains(&stroke(3, 4, Half::Bottom, 4)), "{:?}", rows[5].strokes);
        assert_eq!(rows[7].lane, 4);
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
    fn on_random_histories_lines_only_bend_at_commits_and_meet_at_every_row_edge() {
        use std::collections::BTreeSet;
        for seed in 0..300u64 {
            let mut rng = Lcg(seed + 7);
            let n = 3 + rng.next(60);
            let ids: Vec<String> = (0..n).map(|i| format!("c{i}")).collect();
            let history: Vec<(String, Vec<String>)> = (0..n)
                .map(|i| {
                    let mut parents = Vec::new();
                    if i + 1 < n && rng.next(9) != 0 {
                        parents.push(ids[i + 1 + rng.next((n - i - 1).min(5))].clone());
                        for _ in 0..rng.next(3).saturating_sub(1) {
                            let other = ids[i + 1 + rng.next(n - i - 1)].clone();
                            if !parents.contains(&other) {
                                parents.push(other);
                            }
                        }
                    }
                    (ids[i].clone(), parents)
                })
                .collect();
            let mut layout = LaneLayout::new();
            let rows: Vec<Row> = history.iter().map(|(id, parents)| layout.push(id, parents)).collect();

            let mut above: BTreeSet<(usize, usize)> = BTreeSet::new();
            for (at, row) in rows.iter().enumerate() {
                let mut top = BTreeSet::new();
                let mut bottom = BTreeSet::new();
                for s in &row.strokes {
                    match s.half {
                        Half::Through => {
                            assert_eq!(s.from, s.to, "seed {seed} row {at}: a line changed lane between commits");
                            top.insert((s.from, s.lineage));
                            bottom.insert((s.to, s.lineage));
                        }
                        Half::Top => {
                            assert_eq!(s.to, row.lane, "seed {seed} row {at}: a line bent somewhere but into the commit");
                            top.insert((s.from, s.lineage));
                        }
                        Half::Bottom => {
                            assert_eq!(s.from, row.lane, "seed {seed} row {at}: a line bent somewhere but out of the commit");
                            bottom.insert((s.to, s.lineage));
                        }
                    }
                    assert!(s.from < row.width && s.to < row.width, "seed {seed} row {at}: a line outside the row's width");
                }
                assert_eq!(top, above, "seed {seed} row {at}: the lines leaving the row above are the ones arriving here");
                let lanes: BTreeSet<usize> = bottom.iter().map(|&(lane, _)| lane).collect();
                assert_eq!(lanes.len(), bottom.len(), "seed {seed} row {at}: two lines in one lane");
                above = bottom;
            }
            assert!(above.is_empty(), "seed {seed}: every line reached its commit");
        }
    }

    #[test]
    fn chunked_layout_matches_all_at_once() {
        let history: [(&str, &[&str]); 6] = [
            ("m", &["a", "b"]),
            ("a", &["c"]),
            ("b", &["d"]),
            ("c", &["e"]),
            ("d", &["e"]),
            ("e", &[]),
        ];
        let whole = lanes(history);

        let mut layout = LaneLayout::new();
        let mut chunked = Vec::new();
        for page in history.chunks(2) {
            chunked.extend(page.iter().map(|(id, parents)| layout.push(id, parents)));
        }
        assert_eq!(whole, chunked);
        assert_eq!(layout.open_lanes(), 0);
    }

    #[test]
    fn every_open_line_ends_at_the_root_and_every_commit_is_counted_once() {
        let mut layout = LaneLayout::new();
        let history: [(&str, &[&str]); 7] = [
            ("m2", &["m1", "x"]),
            ("m1", &["a", "y"]),
            ("x", &["a"]),
            ("y", &["r"]),
            ("a", &["r"]),
            ("z", &["r"]),
            ("r", &[]),
        ];
        // z is a stray tip, listed after the rest.
        for (id, parents) in history {
            layout.push(id, parents);
        }
        assert_eq!(layout.open_lanes(), 0, "every line reached a commit");
        assert_eq!(layout.lineages().iter().map(|l| l.commits).sum::<usize>(), history.len());
    }
}
