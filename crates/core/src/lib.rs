pub mod actions;
pub mod backend;
pub mod diff;
pub mod graph;
pub mod group;
pub mod lineage;
pub mod model;
pub mod squash;
pub mod tree;

pub use actions::{CheckoutTarget, Operation, Outcome};
pub use backend::{Backend, Error, GitCli, LogOptions};
pub use graph::{Half, LaneLayout, Lineage, Row, Stroke};
pub use group::{Placed, descendants, group_by_parent};
pub use lineage::{CommitKind, branch_rank, commit_kind, commit_rank, lineage_names, merged_branch_name};
pub use diff::{DiffLine, FileDiff, Hunk, LineKind, Pair};
pub use model::{Commit, CommitDetail, FileChange, FileStatus, Label, LabelKind, Ref, RefKind, labels};
pub use squash::{BranchTip, Evidence, MergeClue, MergeScan, scan_inputs};
pub use tree::{Layout, TreeRow, file_tree, flat_list, visible_rows};
