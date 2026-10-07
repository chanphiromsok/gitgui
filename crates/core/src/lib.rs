pub mod actions;
pub mod backend;
pub mod clone;
pub mod diff;
pub mod filter;
pub mod graph;
pub mod group;
pub mod hosting;
pub mod lineage;
pub mod model;
pub mod people;
pub mod squash;
pub mod tree;
pub mod worktree;

pub use actions::{CheckoutTarget, Operation, Outcome};
pub use backend::{Backend, Error, GitCli, LogOptions, ScanCache};
pub use graph::{Half, LaneLayout, Lineage, Row, Stroke};
pub use hosting::{Host, WebRemote, web_remote};
pub use group::{Placed, descendants, group_by_parent};
pub use lineage::{
    CommitKind, branch_rank, commit_branches, commit_kind, commit_rank, conventional_prefix, lineage_names, merged_branch_name,
};
pub use filter::{Query, Scope, filter_commits, matches_text, stash_count};
pub use diff::{DiffLine, FileDiff, Hunk, LineKind, Pair};
pub use people::{People, Person, people};
pub use model::{Commit, CommitDetail, FileChange, FileStatus, Label, LabelKind, Ref, RefKind, labels};
pub use squash::{BranchTip, Evidence, MergeClue, MergeScan, scan_inputs, subject_pr};
pub use worktree::WorkFile;
pub use tree::{Layout, TreeRow, file_tree, flat_list, visible_rows};
