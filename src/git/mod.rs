pub mod diff;
pub mod repo;

pub use diff::{DiffLine, FileDiff, FileStatus, Hunk, parse_unified};
pub use repo::{
    GIT_SAFETY_ARGS, GrepHit, base_tree, default_branch, diff_trees, file_at, git_guard, git_in,
    grep_word, snapshot_tree,
};
