pub mod claude;
pub mod procs;
pub mod tmux;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Duration;

use anyhow::Result;

use crate::model::{AgentId, Checkout};

pub use claude::{ClaudeProc, parse_agents_json};
pub use procs::is_ancestor;
pub use tmux::{Pane, TmuxConn, parse_list_panes};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Busy,
    Idle,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: AgentId,
    pub session: String,
    pub window_index: u32,
    pub window_name: String,
    pub cwd: PathBuf,
    pub checkout: Option<Checkout>,
    pub session_id: String,
    pub status: AgentStatus,
    pub waiting: bool,
    /// Claude Code's input box, read from the screen of a waiting agent.
    pub input: InputBox,
    /// The permission mode its footer shows ("auto", "plan"...), if seen.
    pub permission_mode: Option<&'static str>,
    /// What Claude Code suggests sending next, read with `input`.
    pub suggestion: Option<String>,
    /// When it was last sent a prompt, from its transcript.
    pub last_prompt: Option<String>,
    /// A menu of Claude Code's own on its screen, waiting for a pick.
    pub menu: Option<ScreenMenu>,
}

/// A menu Claude Code draws in its pane and waits on, such as whether to
/// trust a folder. It is answered by moving the cursor and pressing Enter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenMenu {
    /// The lines above the options, saying what is asked.
    pub question: Vec<String>,
    /// The options as drawn, numbers and all.
    pub options: Vec<String>,
    /// The option the cursor is on.
    pub cursor: usize,
}

/// How many lines above its options a menu's question may take.
const MENU_QUESTION_ROWS: usize = 8;

/// The menu on a plain screen, if one is waiting: a `❯` line that is not
/// the input box, on a screen with no input box (a menu takes its place),
/// with at least one more option lined up with it.
pub fn read_menu(screen: &str) -> Option<ScreenMenu> {
    let rows: Vec<&str> = screen.lines().map(unboxed).collect();
    if input_row(&rows).is_some() {
        return None;
    }
    let p = rows.iter().rposition(|r| r.trim_start().starts_with('❯'))?;
    let indent = |r: &str| r.chars().take_while(|c| *c == ' ').count();
    // The option text starts two columns after the cursor mark.
    let col = indent(rows[p]) + 2;
    let is_option = |r: &str| !r.trim().is_empty() && indent(r) == col;
    let is_detail = |r: &str| !r.trim().is_empty() && indent(r) > col;
    let mut first = p;
    while first > 0 && (is_option(rows[first - 1]) || is_detail(rows[first - 1])) {
        first -= 1;
    }
    let mut last = p;
    while last + 1 < rows.len() && (is_option(rows[last + 1]) || is_detail(rows[last + 1])) {
        last += 1;
    }
    let mut options = Vec::new();
    let mut cursor = 0;
    for (i, row) in rows.iter().enumerate().take(last + 1).skip(first) {
        if i == p {
            cursor = options.len();
            options.push(row.trim_start().trim_start_matches('❯').trim().to_string());
        } else if is_option(row) {
            options.push(row.trim().to_string());
        }
    }
    if options.len() < 2 {
        return None;
    }
    let mut question: Vec<String> = rows[..first]
        .iter()
        .rev()
        .take_while(|r| !is_rule(r) && !is_box_edge(r))
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .take(MENU_QUESTION_ROWS)
        .map(str::to_string)
        .collect();
    question.reverse();
    Some(ScreenMenu {
        question,
        options,
        cursor,
    })
}

/// The top or bottom edge of a box drawn round a dialog.
fn is_box_edge(row: &str) -> bool {
    let r = row.trim();
    r.starts_with(['╭', '╰']) && r.chars().all(|c| "─╭╮╰╯".contains(c))
}

/// `row` without the side borders of a box drawn round a dialog.
fn unboxed(row: &str) -> &str {
    let row = row.trim_end();
    let row = row.strip_suffix('│').unwrap_or(row).trim_end();
    match row.trim_start().strip_prefix('│') {
        Some(inside) => inside.strip_prefix(' ').unwrap_or(inside),
        None => row,
    }
}

/// Claude Code's input box as last read from the agent's screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InputBox {
    /// Not on screen (a turn or a menu is showing), or the screen was not read.
    #[default]
    Unseen,
    /// On screen and empty: text pasted now becomes the whole message.
    Empty,
    /// Holding text typed and not sent.
    Holding(String),
}

/// Whether `line` is one of the rules around Claude Code's input box.
fn is_rule(line: &str) -> bool {
    let l = line.trim();
    !l.is_empty() && l.chars().all(|c| c == '─')
}

/// Whether `line` starts a broadcast from `wall` or `shutdown`, written
/// over whatever was on screen.
pub fn is_broadcast(line: &str) -> bool {
    line.starts_with("Broadcast message from ")
}

/// The row of Claude Code's input box on a plain screen: the last `❯` line
/// with a rule right above it. Menus use `❯` too, but with no rule above.
pub fn input_row(lines: &[&str]) -> Option<usize> {
    let p = lines.iter().rposition(|l| l.starts_with('❯'))?;
    (p > 0 && is_rule(lines[p - 1])).then_some(p)
}

/// Reads the input box from a screen captured with its colours
/// (`capture-pane -e`). Typed text is drawn in the default colour; a dimmed
/// or coloured suggestion in an empty box does not count as typed.
pub fn read_input_box(screen: &str) -> InputBox {
    let styled: Vec<&str> = screen.lines().collect();
    let plain: Vec<String> = styled.iter().map(|l| strip_sgr(l)).collect();
    let rows: Vec<&str> = plain.iter().map(String::as_str).collect();
    let Some(p) = input_row(&rows) else {
        return InputBox::Unseen;
    };
    let mut text = typed_after_prompt(styled[p]);
    // A broadcast from `wall` can be written over the rule that closes the
    // box: it and what follows are not typed.
    for row in rows[p + 1..]
        .iter()
        .take_while(|r| !is_rule(r) && !is_broadcast(r))
    {
        text.push('\n');
        text.push_str(row);
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        InputBox::Empty
    } else {
        InputBox::Holding(text)
    }
}

/// What Claude Code suggests you send next: the dimmed text in an empty
/// input box. A fresh session's `Try "..."` hint and the note under queued
/// messages are not suggestions.
pub fn read_suggestion(screen: &str) -> Option<String> {
    if read_input_box(screen) != InputBox::Empty {
        return None;
    }
    let plain: Vec<String> = screen.lines().map(strip_sgr).collect();
    let rows: Vec<&str> = plain.iter().map(String::as_str).collect();
    let line = rows[input_row(&rows)?];
    let text = line
        .split_once('❯')?
        .1
        .replace('\u{a0}', " ")
        .trim()
        .to_string();
    let hint = text.starts_with("Try \"") || text.starts_with("Press up to edit");
    (!text.is_empty() && !hint).then_some(text)
}

/// `line` without its colour escapes.
fn strip_sgr(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // ESC [ params final-byte
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The text after `❯` on a styled line that is drawn in the default colour
/// and not dimmed.
fn typed_after_prompt(line: &str) -> String {
    let (mut dim, mut coloured, mut seen) = (false, false, false);
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            let mut seq = String::new();
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    if d == 'm' {
                        apply_sgr(&seq, &mut dim, &mut coloured);
                    }
                    break;
                }
                seq.push(d);
            }
        } else if !seen {
            seen = c == '❯';
        } else if !dim && !coloured {
            out.push(if c == '\u{a0}' { ' ' } else { c });
        }
    }
    out
}

/// Updates the dim and foreground-colour state from one SGR sequence (`[1;2m` without the m).
fn apply_sgr(seq: &str, dim: &mut bool, coloured: &mut bool) {
    let params: Vec<&str> = seq.trim_start_matches('[').split(';').collect();
    let mut i = 0;
    while i < params.len() {
        match params[i].parse::<u16>().unwrap_or(0) {
            0 => (*dim, *coloured) = (false, false),
            2 => *dim = true,
            22 => *dim = false,
            30..=37 | 90..=97 => *coloured = true,
            39 => *coloured = false,
            38 => {
                *coloured = true;
                // 38;5;n or 38;2;r;g;b
                i += if params.get(i + 1) == Some(&"2") {
                    4
                } else {
                    2
                };
            }
            _ => {}
        }
        i += 1;
    }
}

pub trait AgentConn: Send + Sync {
    fn list(&self) -> Result<Vec<Agent>>;
    fn send_text(&self, agent: &AgentId, text: &str) -> Result<()>;
    fn is_alive(&self, agent: &AgentId) -> bool;
}

/// Runs `cmd` with `input` on stdin and returns its status, stdout and
/// stderr; past `timeout` it is killed and this is an error.
pub fn run_for(
    cmd: Command,
    input: &str,
    timeout: Duration,
) -> Result<(ExitStatus, String, String)> {
    crate::llm::run_child(cmd, input, timeout, &crate::llm::Children::default())
}

/// Worktree root containing `dir`, if it is inside a git checkout.
pub fn checkout_of(dir: &Path) -> Option<Checkout> {
    let out = crate::git::git_in(dir)
        .ok()?
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let top = String::from_utf8(out.stdout).ok()?;
    Some(Checkout::new(top.trim_end_matches('\n')))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUST: &str = "\
────────────────────────────────────────
 Accessing workspace:

 /home/saman/devel/autobiografia

 Quick safety check: Is this a project you created or one you trust?
 Claude Code'll be able to read, edit, and execute files here.

 Security guide

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm · Esc to cancel
";

    #[test]
    fn the_trust_menu_is_read_with_its_question_and_cursor() {
        let menu = read_menu(TRUST).unwrap();
        assert_eq!(menu.options, ["No, exit", "Yes, I trust this folder"]);
        assert_eq!(menu.cursor, 0);
        assert_eq!(menu.question[0], "Accessing workspace:");
        assert_eq!(menu.question[1], "/home/saman/devel/autobiografia");
        assert_eq!(menu.question.last().unwrap(), "Security guide");
    }

    #[test]
    fn a_boxed_numbered_menu_skips_option_details() {
        let screen = "\
╭──────────────────────────────╮
│ New MCP server found in .mcp.json: db │
│                              │
│   1. Use this server         │
│ ❯ 2. Use all servers         │
│      in this project         │
│   3. Do not use it           │
╰──────────────────────────────╯
";
        let menu = read_menu(screen).unwrap();
        assert_eq!(
            menu.options,
            [
                "1. Use this server",
                "2. Use all servers",
                "3. Do not use it"
            ]
        );
        assert_eq!(menu.cursor, 1);
        assert_eq!(menu.question, ["New MCP server found in .mcp.json: db"]);
    }

    #[test]
    fn no_menu_while_the_input_box_shows_or_with_one_option() {
        let prompt = "❯ an old prompt\n  more of it\n────\n❯ \n────\n";
        assert_eq!(read_menu(prompt), None);
        assert_eq!(read_menu("Something\n ❯ Only one\n\n"), None);
        assert_eq!(read_menu(""), None);
    }

    #[test]
    fn pane_identity_includes_claude_pid() {
        let a = AgentId {
            pane: "%1".into(),
            claude_pid: 10,
        };
        let b = AgentId {
            pane: "%1".into(),
            claude_pid: 11,
        };
        assert_ne!(a, b);
        assert_eq!(
            a,
            AgentId {
                pane: "%1".into(),
                claude_pid: 10
            }
        );
    }

    #[test]
    fn checkout_of_repo_and_non_repo() {
        let here = checkout_of(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert!(here.0.is_absolute());
        let tmp = tempfile::tempdir().unwrap();
        assert!(checkout_of(tmp.path()).is_none());
    }
}

#[cfg(test)]
mod input_box_tests {
    use super::*;

    const RULE: &str = "\x1b[2m──────────\x1b[0m";

    fn screen(prompt_line: &str) -> String {
        format!("● Done.\n{RULE}\n{prompt_line}\n{RULE}\n  ctx:11% Opus\n")
    }

    #[test]
    fn a_broadcast_over_the_box_is_not_typed() {
        let wall = format!(
            "● Done.\n{RULE}\n❯ fix it\nBroadcast message from root@host (Fri 11:00):──────\n  ctx:9% Opus\nThe system will reboot!\n\n"
        );
        assert_eq!(read_input_box(&wall), InputBox::Holding("fix it".into()));
        let empty = wall.replace("❯ fix it", "❯ ");
        assert_eq!(read_input_box(&empty), InputBox::Empty);
    }

    #[test]
    fn claudes_suggestion_is_read_but_not_its_placeholders() {
        let suggestion = |line: &str| read_suggestion(&screen(line));
        assert_eq!(
            suggestion("\x1b[39m❯\u{a0}\x1b[2mcommit this\x1b[0m"),
            Some("commit this".into())
        );
        assert_eq!(
            suggestion("\x1b[39m❯\u{a0}\x1b[38;5;246mrun the tests\x1b[39m"),
            Some("run the tests".into())
        );
        // A fresh session's hint, and the note under queued messages.
        assert_eq!(
            suggestion("\x1b[39m❯\u{a0}\x1b[2mTry \"fix lint errors\"\x1b[0m"),
            None
        );
        assert_eq!(
            suggestion("\x1b[39m❯\u{a0}\x1b[2mPress up to edit queued messages\x1b[0m"),
            None
        );
        // Typed text is yours, not a suggestion; an empty box has none.
        assert_eq!(suggestion("\x1b[39m❯\u{a0}go ahead"), None);
        assert_eq!(suggestion("\x1b[39m❯\u{a0}"), None);
        assert_eq!(read_suggestion("Allow this?\n❯ 1. Yes"), None, "a menu");
    }

    #[test]
    fn empty_typed_and_suggested_prompts() {
        assert_eq!(read_input_box(&screen("\x1b[39m❯\u{a0}")), InputBox::Empty);
        assert_eq!(
            read_input_box(&screen("\x1b[39m❯\u{a0}go ahead and add those")),
            InputBox::Holding("go ahead and add those".into())
        );
        // A dimmed or grey suggestion is not typed text.
        assert_eq!(
            read_input_box(&screen(
                "\x1b[39m❯\u{a0}\x1b[2mTry \"fix lint errors\"\x1b[22m"
            )),
            InputBox::Empty
        );
        assert_eq!(
            read_input_box(&screen(
                "\x1b[39m❯\u{a0}\x1b[38;5;246mrun the tests\x1b[39m"
            )),
            InputBox::Empty
        );
        assert_eq!(
            read_input_box(&screen(
                "\x1b[39m❯\u{a0}\x1b[38;2;153;153;153mrun the tests\x1b[39m"
            )),
            InputBox::Empty
        );
    }

    #[test]
    fn a_menu_or_a_turn_is_no_input_box() {
        assert_eq!(
            read_input_box("Allow this?\n❯ 1. Yes\n  2. No\n"),
            InputBox::Unseen
        );
        assert_eq!(read_input_box("✻ Working… (12s)\n"), InputBox::Unseen);
    }
}
