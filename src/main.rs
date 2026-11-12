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
    /// Ask another Claude session to do something; you approve it in aq
    /// first. With no arguments, lists the sessions it can tell.
    Tell {
        /// The session: its project name, as its tab shows it
        to: Option<String>,
        /// What to ask it
        #[arg(trailing_var_arg = true)]
        message: Vec<String>,
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
            let _open = OpenFlag::set();
            tokio::runtime::Runtime::new()?.block_on(aq::runtime::run(aq::config::paths(), conn))
        }
        Some(Command::Hook {
            event,
            deadline_secs,
        }) => {
            hook(event, Duration::from_secs(deadline_secs));
            Ok(())
        }
        Some(Command::Tell { to, message }) => {
            let pane = aq::hook::tmux_pane();
            let req = match to {
                None => aq::ipc::HookRequest::Tellable { pane },
                Some(to) => aq::ipc::HookRequest::Tell {
                    pane,
                    to,
                    text: message.join(" "),
                },
            };
            let (ok, text) = aq::hook::tell(&aq::config::paths().socket, &req);
            if ok {
                println!("{text}");
                Ok(())
            } else {
                eprintln!("aq tell: {text}");
                std::process::exit(1);
            }
        }
    }
}

/// Marks aq as open in tmux until dropped, panics included.
struct OpenFlag;

impl OpenFlag {
    fn set() -> Self {
        aq::agents::tmux::mark_open(true);
        OpenFlag
    }
}

impl Drop for OpenFlag {
    fn drop(&mut self) {
        aq::agents::tmux::mark_open(false);
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
