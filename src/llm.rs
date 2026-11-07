//! Background `claude -p` work: quiz questions, judging, side questions.

/// Work for the LLM worker. Real variants arrive with quiz and ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmJob {
    Noop,
}

/// Results from the LLM worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmEvent {
    Noop,
}
