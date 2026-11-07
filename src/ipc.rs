//! Hook to TUI messages over the Unix socket.

/// Something a hook sent. Real variants arrive with the socket server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcEvent {
    Noop,
}

/// The answer to a waiting hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { reason: String },
}
