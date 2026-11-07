pub mod diff;
pub mod repo;

pub use diff::{DiffLine, FileDiff, FileStatus, Hunk, parse_unified};
pub use repo::{base_tree, branch_diff, default_branch, diff_trees, file_at, snapshot_tree};
