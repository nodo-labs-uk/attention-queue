use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

pub use crate::permission::PermissionCard;
pub use crate::question::QuestionCard;
pub use crate::quiz::QuizCard;
pub use crate::review::ReviewCard;
pub use crate::shell::ShellCard;
pub use crate::step::StepCard;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Review,
    Step,
    Quiz,
    /// Nothing is checked: the agent just codes.
    Vibe,
}

/// Worktree root, canonicalised.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Checkout(pub PathBuf);

impl Checkout {
    /// Canonicalises `path`; falls back to the path as given if it does not exist.
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        Checkout(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentId {
    /// tmux pane id, like `%54`.
    pub pane: String,
    pub claude_pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardKind {
    Review(ReviewCard),
    Step(Box<StepCard>),
    ShellChange(ShellCard),
    Quiz(QuizCard),
    Permission(PermissionCard),
    /// The session's last message asks for an answer.
    Question(QuestionCard),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub id: u64,
    pub checkout: Checkout,
    pub agent: AgentId,
    pub created: SystemTime,
    pub kind: CardKind,
}
