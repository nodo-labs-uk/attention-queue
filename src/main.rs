use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use aq::agents::TmuxConn;
use aq::hook::{DEFAULT_DEADLINE_SECS, HookEvent};
use clap::{Parser, Subcommand};

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
        event: HookEvent,
        /// Seconds to wait for an answer before giving up
        #[arg(long, default_value_t = DEFAULT_DEADLINE_SECS)]
        deadline_secs: u64,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // A hook must never fail Claude: exit 2 from PreToolUse would block
        // every edit. Say what was wrong on stderr and give no decision.
        Err(e) if std::env::args_os().nth(1).is_some_and(|a| a == "hook") => {
            eprintln!("aq hook: {e}");
            return Ok(());
        }
        Err(e) => e.exit(),
    };
    match cli.command {
        None => {
            let conn = Arc::new(TmuxConn::default());
            tokio::runtime::Runtime::new()?.block_on(aq::runtime::run(aq::config::paths(), conn))
        }
        Some(Command::Hook {
            event,
            deadline_secs,
        }) => {
            hook(event, Duration::from_secs(deadline_secs));
            Ok(())
        }
    }
}

/// Prints only the decision JSON on stdout and always exits 0, so a broken
/// hook never gets in Claude's way. Errors go to stderr.
fn hook(event: HookEvent, deadline: Duration) {
    match aq::hook::run_hook(event, std::io::stdin().lock(), deadline) {
        Ok(Some(json)) => {
            if let Err(e) = writeln!(std::io::stdout(), "{json}") {
                eprintln!("aq hook: writing the decision: {e}");
            }
        }
        Ok(None) => {}
        Err(e) => eprintln!("aq hook: {e:#}"),
    }
}
