use std::sync::Arc;

use aq::agents::TmuxConn;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "aq",
    version,
    about = "Attention queue for Claude Code agents in tmux"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Claude Code hook entry point
    Hook {
        kind: HookKind,
        /// Seconds to wait for an answer before giving up
        #[arg(long)]
        deadline_secs: Option<u64>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum HookKind {
    PreTool,
    PostTool,
    Permission,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => {
            let conn = Arc::new(TmuxConn::default());
            tokio::runtime::Runtime::new()?.block_on(aq::runtime::run(aq::config::paths(), conn))
        }
        // With no aq listening a hook must exit 0 silently.
        Some(Command::Hook { .. }) => Ok(()),
    }
}
