// Scaffolding: later tasks consume the model and config items.
#![allow(dead_code)]

mod config;
mod model;

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

fn main() {
    let cli = Cli::parse();
    match cli.command {
        None => println!("not yet"),
        // With no aq listening a hook must exit 0 silently.
        Some(Command::Hook { .. }) => {}
    }
}
