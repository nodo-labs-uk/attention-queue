//! Recognises risky commands (pushes, PR and issue writes, releases) in a Bash
//! command line or a GitHub MCP tool call. It does no I/O, so the hook process
//! can call it directly.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrVerb {
    Merge,
    Close,
    Comment,
    Review,
    Edit,
    Ready,
    Reopen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IssueVerb {
    Close,
    Comment,
    Edit,
    Reopen,
    Delete,
    Transfer,
}

/// One risky action found in a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Push {
        dir: Option<PathBuf>,
        remote: Option<String>,
        refspecs: Vec<String>,
        force: bool,
        delete: bool,
        tags: bool,
    },
    BranchForceDelete {
        dir: Option<PathBuf>,
        branch: String,
    },
    Pr {
        verb: PrVerb,
        number: Option<u64>,
        repo: Option<String>,
        /// A PR named by something other than a number, such as a branch,
        /// passed to `gh pr view` as it is.
        selector: Option<String>,
        speaks: bool,
    },
    Issue {
        verb: IssueVerb,
        number: Option<u64>,
        repo: Option<String>,
        speaks: bool,
    },
    Release {
        repo: Option<String>,
    },
    RepoAdmin {
        repo: Option<String>,
    },
    ApiWrite {
        method: String,
        path: String,
    },
    McpWrite {
        tool: String,
        input: Value,
    },
    /// A line nested too deeply to scan. Never cleared: Haiku judges it.
    TooDeep,
}

/// MCP tool name prefixes (after the last `__`) that only read from GitHub.
/// Every other GitHub MCP tool is a candidate.
const MCP_READ_PREFIXES: [&str; 3] = ["get_", "list_", "search_"];

/// `gh` flags that take a separate value, so the value is not read as a word.
const GH_VALUE_FLAGS: &[&str] = &[
    "-R",
    "--repo",
    "-b",
    "--body",
    "-F",
    "--body-file",
    "-c",
    "--comment",
    "-t",
    "--title",
    "-X",
    "--method",
    "-f",
    "--field",
    "--raw-field",
    "-H",
    "--header",
    "--input",
    "-q",
    "--jq",
    "--template",
    "--hostname",
    "--cache",
    "--add-label",
    "--remove-label",
    "-l",
    "--label",
    "--assignee",
    "--add-assignee",
    "--remove-assignee",
    "--reviewer",
    "--add-reviewer",
    "--remove-reviewer",
    "--milestone",
    "--project",
    "--add-project",
    "--remove-project",
    "-B",
    "--base",
    "--head",
    "-A",
    "--author-email",
    "--subject",
    "--match-head-commit",
    "--reason",
    "-n",
    "--notes",
    "--notes-file",
    "--target",
    "--discussion-category",
];

/// Shell keywords that start a command after them.
const CONTROL_WORDS: [&str; 9] = [
    "do", "then", "else", "elif", "if", "while", "until", "!", "{",
];

/// Wrapper words that run the command after them.
const WRAPPERS: [&str; 9] = [
    "env", "sudo", "command", "exec", "time", "nohup", "nice", "timeout", "xargs",
];

/// The flags of a wrapper that take a separate value. Any other flag is boolean.
fn wrapper_value_flags(wrapper: &str) -> &'static [&'static str] {
    match wrapper {
        "sudo" => &[
            "-u",
            "-g",
            "-C",
            "-h",
            "-p",
            "-r",
            "-t",
            "-U",
            "-D",
            "--user",
            "--group",
            "--close-from",
            "--host",
            "--prompt",
            "--role",
            "--type",
            "--other-user",
            "--chdir",
        ],
        "env" => &["-u", "-C", "--unset", "--chdir"],
        "exec" => &["-a"],
        "nice" => &["-n", "--adjustment"],
        "timeout" => &["-s", "-k", "--signal", "--kill-after"],
        "xargs" => &["-I", "-P", "-d", "-L", "-n", "-s", "-E"],
        _ => &[],
    }
}

/// True for tools whose calls can be candidates: Bash and GitHub MCP tools.
pub fn is_flag_tool(tool_name: &str) -> bool {
    tool_name == "Bash"
        || (tool_name.starts_with("mcp__") && tool_name.to_ascii_lowercase().contains("github"))
}

/// The risky actions in one tool call. Empty when the call is not a candidate.
pub fn candidates(tool_name: &str, input: &Value) -> Vec<Action> {
    if tool_name == "Bash" {
        let mut out = Vec::new();
        if let Some(line) = input.get("command").and_then(Value::as_str) {
            scan_line(line, &mut None, &mut out, 0);
        }
        out
    } else if is_flag_tool(tool_name) {
        mcp_action(tool_name, input).into_iter().collect()
    } else {
        Vec::new()
    }
}

/// True for an `owner/name` repo that is safe to hand to `gh`: letters,
/// digits, `_`, `.` and `-` on each side of one `/`, and no leading `-`.
pub fn valid_repo(repo: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && !p.starts_with('-')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    repo.split_once('/')
        .is_some_and(|(owner, name)| part(owner) && part(name))
}

/// True if the shell would expand `word` (a `$` or a backtick), so its
/// written text is not the name the command will use.
pub fn expands(word: &str) -> bool {
    word.contains(['$', '`'])
}

/// The action of a GitHub MCP tool call. Read tools are not candidates;
/// opening a PR or an issue is not an answer, so it is not one either.
/// Tools that are mapped become the action they perform, so the rules
/// decide; any other tool is a write the rules cannot place (grey).
fn mcp_action(tool_name: &str, input: &Value) -> Option<Action> {
    let name = tool_name.rsplit("__").next().unwrap_or(tool_name);
    if MCP_READ_PREFIXES.iter().any(|p| name.starts_with(p)) || name.ends_with("_read") {
        return None;
    }
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    let has = |key: &str| !text(key).trim().is_empty();
    let number = |keys: &[&str]| {
        keys.iter().find_map(|k| match input.get(*k)? {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        })
    };
    let repo = Some(format!("{}/{}", text("owner"), text("repo"))).filter(|r| valid_repo(r));
    let pr_number = number(&["pullNumber", "pull_number"]);
    let issue_number = number(&["issue_number", "issueNumber"]);
    let method = text("method");
    let state = text("state");
    // Speaking needs no facts: without a repo and number, it is still `AS YOU`.
    let speaks_somewhere = || Action::Issue {
        verb: IssueVerb::Comment,
        number: None,
        repo: None,
        speaks: true,
    };
    let pr = |verb, speaks| match (&repo, pr_number) {
        (Some(_), Some(_)) => Action::Pr {
            verb,
            number: pr_number,
            repo: repo.clone(),
            selector: None,
            speaks,
        },
        _ if speaks => speaks_somewhere(),
        _ => grey(tool_name, input),
    };
    let issue = |verb, speaks| match (&repo, issue_number) {
        (Some(_), Some(_)) => Action::Issue {
            verb,
            number: issue_number,
            repo: repo.clone(),
            speaks,
        },
        _ if speaks => speaks_somewhere(),
        _ => grey(tool_name, input),
    };
    let push = || match (&repo, text("branch")) {
        (Some(repo), branch) if !branch.is_empty() && !branch.starts_with('-') => Action::Push {
            dir: None,
            remote: Some(format!("https://github.com/{repo}")),
            refspecs: vec![format!("refs/heads/{branch}")],
            force: false,
            delete: false,
            tags: false,
        },
        _ => grey(tool_name, input),
    };
    Some(match name {
        "create_pull_request" | "create_issue" => return None,
        "issue_write" if method == "create" => return None,
        "merge_pull_request" => pr(PrVerb::Merge, false),
        "add_issue_comment" => issue(IssueVerb::Comment, true),
        "add_comment_to_pending_review" | "add_reply_to_pull_request_comment" => {
            pr(PrVerb::Comment, true)
        }
        "pull_request_review_write" if method == "delete_pending" => pr(PrVerb::Edit, false),
        "create_pending_pull_request_review"
        | "submit_pending_pull_request_review"
        | "create_and_submit_pull_request_review"
        | "pull_request_review_write" => pr(PrVerb::Review, true),
        "update_pull_request" => {
            let verb = match state {
                "closed" => PrVerb::Close,
                "open" => PrVerb::Reopen,
                _ => PrVerb::Edit,
            };
            pr(verb, has("body") || has("comment"))
        }
        "update_issue" | "issue_write" => {
            let verb = match state {
                "closed" => IssueVerb::Close,
                "open" => IssueVerb::Reopen,
                _ => IssueVerb::Edit,
            };
            issue(verb, has("body") || has("comment"))
        }
        "push_files" | "create_or_update_file" | "delete_file" => push(),
        _ => grey(tool_name, input),
    })
}

/// A GitHub MCP write the rules cannot place.
fn grey(tool_name: &str, input: &Value) -> Action {
    Action::McpWrite {
        tool: tool_name.to_string(),
        input: input.clone(),
    }
}

/// How many substitutions or `bash -c` scripts deep a line is scanned.
/// Deeper than this, the hook could run out of stack, and a crashed hook
/// lets the command run.
const MAX_DEPTH: usize = 8;

/// Scans every simple command of a line, and the commands inside its
/// `$(...)` and backtick substitutions. `cwd` carries a `cd` across them.
fn scan_line(line: &str, cwd: &mut Option<PathBuf>, out: &mut Vec<Action>, depth: usize) {
    if depth > MAX_DEPTH {
        out.push(Action::TooDeep);
        return;
    }
    let (commands, substitutions) = split_commands(line);
    for inner in substitutions {
        scan_line(&inner, &mut cwd.clone(), out, depth + 1);
    }
    for words in commands {
        let words = strip_wrappers(&words);
        match words.first().map(String::as_str) {
            Some("cd") => *cwd = cd_target(&words[1..]),
            Some("git") => out.extend(git_actions(&words[1..], cwd.as_deref())),
            Some("gh") => out.extend(gh_action(&words[1..])),
            Some("bash" | "sh" | "zsh") => {
                if let Some(inner) = shell_script(&words[1..]) {
                    scan_line(inner, cwd, out, depth + 1);
                }
            }
            _ => {}
        }
    }
}

/// Drops leading `VAR=x` words, shell keywords such as `do` and `then`, and
/// wrappers such as `env`, `sudo`, `time` and `timeout`.
fn strip_wrappers(mut words: &[String]) -> &[String] {
    while let Some(first) = words.first() {
        if is_assignment(first) || CONTROL_WORDS.contains(&first.as_str()) {
            words = &words[1..];
        } else if WRAPPERS.contains(&first.as_str()) {
            let wrapper = first.as_str();
            let value_flags = wrapper_value_flags(wrapper);
            let timeout = wrapper == "timeout";
            words = &words[1..];
            while let Some(flag) = words.first().filter(|w| w.starts_with('-')) {
                let skip = 1 + usize::from(value_flags.contains(&flag.as_str()));
                words = &words[skip.min(words.len())..];
            }
            if timeout {
                words = words.get(1..).unwrap_or_default();
            }
        } else {
            break;
        }
    }
    words
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

fn cd_target(args: &[String]) -> Option<PathBuf> {
    args.iter().find(|a| !a.starts_with('-')).map(PathBuf::from)
}

/// The script of `bash -c "..."`, when the shell flags include `-c`.
fn shell_script(args: &[String]) -> Option<&str> {
    let at = args
        .iter()
        .take_while(|a| a.starts_with('-'))
        .position(|a| !a.starts_with("--") && a.contains('c'))?;
    args.get(at + 1).map(String::as_str)
}

/// Splits a line into simple commands, each a list of words. Honours quotes and
/// backslashes, breaks on `&&`, `||`, `;`, `|`, `&`, parentheses and newlines
/// outside quotes, drops comments and skips heredoc bodies. Also returns the
/// text of each backtick substitution, and of each `$(...)` inside double
/// quotes, which the shell runs as commands. (An unquoted `$(...)` splits on
/// its parentheses like any other command.)
fn split_commands(line: &str) -> (Vec<Vec<String>>, Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut cmds = Vec::new();
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut redirect_target = false;
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut subs = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '`' => {
                in_word = true;
                i = substitution(&chars, i, &mut cur, &mut subs);
            }
            '\'' => {
                in_word = true;
                i += 1;
                while i < n && chars[i] != '\'' {
                    cur.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            '"' => {
                in_word = true;
                i += 1;
                while i < n && chars[i] != '"' {
                    if chars[i] == '`' || chars[i..].starts_with(&['$', '(']) {
                        i = substitution(&chars, i, &mut cur, &mut subs);
                        continue;
                    }
                    let escaped = chars[i] == '\\'
                        && i + 1 < n
                        && matches!(chars[i + 1], '"' | '\\' | '$' | '`');
                    if escaped {
                        i += 1;
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                if i + 1 < n && chars[i + 1] != '\n' {
                    cur.push(chars[i + 1]);
                    in_word = true;
                }
                i += 2;
            }
            ' ' | '\t' => {
                flush_word(&mut words, &mut cur, &mut in_word, &mut redirect_target);
                i += 1;
            }
            '#' if !in_word => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\n' => {
                flush_word(&mut words, &mut cur, &mut in_word, &mut redirect_target);
                flush_command(&mut cmds, &mut words, &mut redirect_target);
                i += 1;
                for (delim, strip_tabs) in heredocs.drain(..) {
                    i = skip_heredoc(&chars, i, &delim, strip_tabs);
                }
            }
            '&' if chars.get(i + 1) == Some(&'>') => {
                flush_word(&mut words, &mut cur, &mut in_word, &mut redirect_target);
                i = skip_redirect_operator(&chars, i);
                redirect_target = true;
            }
            ';' | '&' | '|' | '(' | ')' => {
                flush_word(&mut words, &mut cur, &mut in_word, &mut redirect_target);
                flush_command(&mut cmds, &mut words, &mut redirect_target);
                i += 1;
            }
            '<' if chars[i..].starts_with(&['<', '<']) && chars.get(i + 2) != Some(&'<') => {
                flush_word(&mut words, &mut cur, &mut in_word, &mut redirect_target);
                i += 2;
                let strip_tabs = chars.get(i) == Some(&'-');
                if strip_tabs {
                    i += 1;
                }
                while matches!(chars.get(i), Some(' ' | '\t')) {
                    i += 1;
                }
                let mut delim = String::new();
                while i < n && !" \t\n;&|()<>".contains(chars[i]) {
                    if !matches!(chars[i], '\'' | '"' | '\\') {
                        delim.push(chars[i]);
                    }
                    i += 1;
                }
                heredocs.push((delim, strip_tabs));
            }
            '<' | '>' => {
                drop_fd_number(&mut words, &mut cur, &mut in_word, &mut redirect_target);
                i = skip_redirect_operator(&chars, i);
                redirect_target = true;
            }
            _ => {
                cur.push(c);
                in_word = true;
                i += 1;
            }
        }
    }
    flush_word(&mut words, &mut cur, &mut in_word, &mut redirect_target);
    flush_command(&mut cmds, &mut words, &mut redirect_target);
    (cmds, subs)
}

/// Reads the substitution that starts at `i` (a backtick or `$(`), adds its
/// text to `cur` and its command line to `subs`, and returns the index after
/// it. Quotes and nested parentheses inside `$(...)` are honoured.
fn substitution(chars: &[char], mut i: usize, cur: &mut String, subs: &mut Vec<String>) -> usize {
    let start = i;
    let inner_start;
    if chars[i] == '`' {
        i += 1;
        inner_start = i;
        while i < chars.len() && chars[i] != '`' {
            i += if chars[i] == '\\' { 2 } else { 1 };
        }
    } else {
        i += 2;
        inner_start = i;
        let mut depth = 1;
        while i < chars.len() {
            match chars[i] {
                '\\' => i += 1,
                '\'' | '"' => {
                    let q = chars[i];
                    i += 1;
                    while i < chars.len() && chars[i] != q {
                        i += if q == '"' && chars[i] == '\\' { 2 } else { 1 };
                    }
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    let end = i.min(chars.len());
    subs.push(chars[inner_start.min(end)..end].iter().collect());
    let after = (end + 1).min(chars.len());
    cur.extend(&chars[start..after]);
    after
}

fn flush_word(
    words: &mut Vec<String>,
    cur: &mut String,
    in_word: &mut bool,
    redirect_target: &mut bool,
) {
    if *in_word {
        let word = std::mem::take(cur);
        if !std::mem::take(redirect_target) {
            words.push(word);
        }
        *in_word = false;
    }
}

/// Ends the word before a redirection. A bare file descriptor number such as
/// the `2` in `2>&1` belongs to the redirection, so it is dropped.
fn drop_fd_number(
    words: &mut Vec<String>,
    cur: &mut String,
    in_word: &mut bool,
    redirect_target: &mut bool,
) {
    if *in_word && cur.chars().all(|c| c.is_ascii_digit()) {
        cur.clear();
        *in_word = false;
    } else {
        flush_word(words, cur, in_word, redirect_target);
    }
}

/// Returns the index after a redirection operator (`>`, `>>`, `<`, `2>&`, `&>`).
fn skip_redirect_operator(chars: &[char], mut i: usize) -> usize {
    if chars[i] == '&' {
        i += 1;
    }
    while matches!(chars.get(i), Some('<' | '>')) {
        i += 1;
    }
    if matches!(chars.get(i), Some('&' | '|')) {
        i += 1;
    }
    i
}

fn flush_command(cmds: &mut Vec<Vec<String>>, words: &mut Vec<String>, redirect_target: &mut bool) {
    *redirect_target = false;
    if !words.is_empty() {
        cmds.push(std::mem::take(words));
    }
}

/// Returns the index after the heredoc body that starts at `i`.
fn skip_heredoc(chars: &[char], mut i: usize, delim: &str, strip_tabs: bool) -> usize {
    while i < chars.len() {
        let end = chars[i..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(chars.len(), |p| i + p);
        let line: String = chars[i..end].iter().collect();
        i = (end + 1).min(chars.len());
        let line = if strip_tabs {
            line.trim_start_matches('\t')
        } else {
            &line
        };
        if line == delim {
            break;
        }
    }
    i
}

/// A short-flag cluster such as `-fd`, as the letters after the dash.
fn short_flags(arg: &str) -> Option<&str> {
    arg.strip_prefix('-')
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
}

fn git_actions(args: &[String], cwd: Option<&Path>) -> Vec<Action> {
    let mut c_dir: Option<&str> = None;
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        match args[i].as_str() {
            "-C" => {
                c_dir = args.get(i + 1).map(String::as_str);
                i += 1;
            }
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" => i += 1,
            _ => {}
        }
        i += 1;
    }
    let dir = match (cwd, c_dir) {
        (Some(base), Some(d)) => Some(base.join(d)),
        (base, d) => d.map(PathBuf::from).or_else(|| base.map(Path::to_path_buf)),
    };
    let rest = args.get(i + 1..).unwrap_or_default();
    match args.get(i).map(String::as_str) {
        Some("push") => vec![git_push(rest, dir)],
        Some("branch") => git_branch_force_delete(rest, dir),
        _ => Vec::new(),
    }
}

fn git_push(args: &[String], dir: Option<PathBuf>) -> Action {
    let (mut force, mut delete, mut tags, mut all) = (false, false, false, false);
    let mut positionals = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-o" | "--push-option" | "--receive-pack" | "--exec" | "--repo" => {
                iter.next();
            }
            "--force" => force = true,
            a if a.starts_with("--force-with-lease") => force = true,
            // A mirror or prune push deletes remote refs that are gone here.
            "--mirror" => (delete, all) = (true, true),
            "--delete" | "--prune" => delete = true,
            "--tags" | "--follow-tags" => tags = true,
            // Every branch: a glob refspec, which `aq` does not judge.
            "--all" | "--branches" => all = true,
            a if short_flags(a).is_some() => {
                let letters = short_flags(a).unwrap_or_default();
                force |= letters.contains('f');
                delete |= letters.contains('d');
            }
            a if !a.starts_with('-') => positionals.push(a.to_string()),
            _ => {}
        }
    }
    let mut positionals = positionals.into_iter();
    let remote = positionals.next();
    let mut refspecs: Vec<String> = positionals.collect();
    if all {
        refspecs.push("refs/heads/*".into());
    }
    force |= refspecs.iter().any(|r| r.starts_with('+'));
    delete |= refspecs.iter().any(|r| r.starts_with(':'));
    tags |= refspecs.iter().any(|r| {
        r.rsplit(':')
            .next()
            .is_some_and(|d| d.starts_with("refs/tags/"))
    });
    Action::Push {
        dir,
        remote,
        refspecs,
        force,
        delete,
        tags,
    }
}

fn git_branch_force_delete(args: &[String], dir: Option<PathBuf>) -> Vec<Action> {
    let (mut delete, mut force) = (false, false);
    let mut branches = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--delete" => delete = true,
            "--force" => force = true,
            a if short_flags(a).is_some() => {
                let letters = short_flags(a).unwrap_or_default();
                delete |= letters.contains(['d', 'D']);
                force |= letters.contains(['f', 'D']);
            }
            a if !a.starts_with('-') => branches.push(a.to_string()),
            _ => {}
        }
    }
    if !(delete && force) {
        return Vec::new();
    }
    branches
        .into_iter()
        .map(|branch| Action::BranchForceDelete {
            dir: dir.clone(),
            branch,
        })
        .collect()
}

/// The words and flags of a `gh` command.
struct GhArgs {
    positionals: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl GhArgs {
    fn parse(args: &[String]) -> Self {
        let mut positionals = Vec::new();
        let mut flags = Vec::new();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            if let Some(long) = arg.strip_prefix("--").filter(|l| !l.is_empty()) {
                if let Some((name, value)) = long.split_once('=') {
                    flags.push((format!("--{name}"), Some(value.to_string())));
                } else if GH_VALUE_FLAGS.contains(&arg.as_str()) {
                    flags.push((arg.clone(), iter.next().cloned()));
                } else {
                    flags.push((arg.clone(), None));
                }
            } else if arg.starts_with('-') && arg.len() > 1 {
                let (name, attached) = match arg.get(..2) {
                    Some(name) => (name, &arg[2..]),
                    None => (arg.as_str(), ""),
                };
                if matches!(name, "-R" | "-X") && !attached.is_empty() {
                    flags.push((name.to_string(), Some(attached.to_string())));
                } else if GH_VALUE_FLAGS.contains(&arg.as_str()) {
                    flags.push((arg.clone(), iter.next().cloned()));
                } else {
                    flags.push((arg.clone(), None));
                }
            } else {
                positionals.push(arg.clone());
            }
        }
        Self { positionals, flags }
    }

    fn has(&self, names: &[&str]) -> bool {
        self.flags.iter().any(|(n, _)| names.contains(&n.as_str()))
    }

    fn value(&self, names: &[&str]) -> Option<&str> {
        self.flags
            .iter()
            .find(|(n, _)| names.contains(&n.as_str()))
            .and_then(|(_, v)| v.as_deref())
    }
}

fn gh_action(args: &[String]) -> Option<Action> {
    let a = GhArgs::parse(args);
    let mut repo = a.value(&["-R", "--repo"]).map(String::from);
    let [noun, verb, rest @ ..] = a.positionals.as_slice() else {
        return None;
    };
    // The PR or issue: a number, a GitHub link (whose repo it is on), or
    // for a PR anything else `gh` reads, such as a branch name.
    let (number, selector) = match rest.first() {
        None => (None, None),
        Some(word) => match (word.trim_start_matches('#').parse().ok(), url_target(word)) {
            (Some(n), _) => (Some(n), None),
            (None, Some((url_repo, n))) => {
                // `gh` acts on the link's repo, whatever `-R` says.
                repo = Some(url_repo);
                (Some(n), None)
            }
            (None, None) => (None, Some(word.clone())),
        },
    };
    let speaks = match verb.as_str() {
        "comment" | "review" => true,
        "merge" | "close" | "reopen" => a.has(&["--comment", "-c", "--body", "-b"]),
        "edit" => a.has(&["--body", "-b", "--body-file", "-F"]),
        _ => false,
    };
    match noun.as_str() {
        "pr" => Some(Action::Pr {
            verb: pr_verb(verb)?,
            number,
            repo,
            selector,
            speaks,
        }),
        "issue" => Some(Action::Issue {
            verb: issue_verb(verb)?,
            number,
            repo,
            speaks,
        }),
        "release" => matches!(
            verb.as_str(),
            "create" | "delete" | "edit" | "upload" | "delete-asset"
        )
        .then_some(Action::Release { repo }),
        "repo" => matches!(
            verb.as_str(),
            "delete" | "archive" | "unarchive" | "rename" | "edit"
        )
        .then(|| Action::RepoAdmin {
            repo: repo.or_else(|| rest.first().filter(|r| r.contains('/')).cloned()),
        }),
        "api" => api_write(&a),
        _ => None,
    }
}

/// The repo and number of a `github.com` link to a PR or issue
/// (`https://github.com/o/r/pull/5`, `.../issues/5#issuecomment-1`).
fn url_target(word: &str) -> Option<(String, u64)> {
    let rest = word
        .strip_prefix("https://")
        .or_else(|| word.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let mut parts = rest.split('/');
    if !parts.next()?.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let repo = format!("{}/{}", parts.next()?, parts.next()?);
    if !valid_repo(&repo) || !matches!(parts.next()?, "pull" | "issues") {
        return None;
    }
    let n = parts.next()?;
    let end = n.find(|c: char| !c.is_ascii_digit()).unwrap_or(n.len());
    Some((repo, n[..end].parse().ok()?))
}

fn pr_verb(verb: &str) -> Option<PrVerb> {
    Some(match verb {
        "merge" => PrVerb::Merge,
        "close" => PrVerb::Close,
        "comment" => PrVerb::Comment,
        "review" => PrVerb::Review,
        "edit" => PrVerb::Edit,
        "ready" => PrVerb::Ready,
        "reopen" => PrVerb::Reopen,
        _ => return None,
    })
}

fn issue_verb(verb: &str) -> Option<IssueVerb> {
    Some(match verb {
        "close" => IssueVerb::Close,
        "comment" => IssueVerb::Comment,
        "edit" => IssueVerb::Edit,
        "reopen" => IssueVerb::Reopen,
        "delete" => IssueVerb::Delete,
        "transfer" => IssueVerb::Transfer,
        _ => return None,
    })
}

/// A `gh api` call that is not a read: an explicit non-GET method, or fields
/// (which make `gh` send a POST). A GraphQL call is one only when its query
/// is a mutation, or `aq` cannot read its query. REST paths that comment,
/// review, merge or delete a branch become those actions.
fn api_write(a: &GhArgs) -> Option<Action> {
    let path = a.positionals.get(1)?.clone();
    if path.trim_start_matches('/') == "graphql" {
        return graphql_write(a).then_some(Action::ApiWrite {
            method: "POST".into(),
            path,
        });
    }
    let method = match a.value(&["-X", "--method"]) {
        Some(m) => m.to_ascii_uppercase(),
        None if a.has(&["-f", "-F", "--field", "--raw-field", "--input"]) => "POST".to_string(),
        None => return None,
    };
    if matches!(method.as_str(), "GET" | "HEAD") {
        return None;
    }
    Some(rest_action(&method, &path).unwrap_or(Action::ApiWrite { method, path }))
}

/// Whether a `gh api graphql` call may write: its query is a mutation, it
/// comes from a file or stdin, or there is no query to read.
fn graphql_write(a: &GhArgs) -> bool {
    if a.has(&["--input"]) {
        return true;
    }
    let query = a.flags.iter().find_map(|(name, value)| {
        let value = value.as_deref()?;
        matches!(name.as_str(), "-f" | "-F" | "--field" | "--raw-field")
            .then(|| value.strip_prefix("query="))
            .flatten()
    });
    match query {
        Some(q) if q.starts_with('@') => true,
        Some(q) => q.contains("mutation"),
        None => true,
    }
}

/// The action of a REST write that `aq` knows, from its method and path.
fn rest_action(method: &str, path: &str) -> Option<Action> {
    let path = path
        .strip_prefix("https://api.github.com/")
        .unwrap_or(path)
        .trim_start_matches('/');
    let path = path.split('?').next().unwrap_or(path);
    let parts: Vec<&str> = path.split('/').collect();
    let ["repos", owner, name, rest @ ..] = parts.as_slice() else {
        return None;
    };
    // `gh` fills in `{owner}` and `{repo}` from the current directory's repo.
    let repo = if *owner == "{owner}" && *name == "{repo}" {
        None
    } else {
        Some(format!("{owner}/{name}"))
    };
    if repo.as_deref().is_some_and(|r| !valid_repo(r)) {
        return None;
    }
    let number = |n: &str| n.parse::<u64>().ok();
    match (method, rest) {
        ("POST", ["issues", n, "comments"]) => Some(Action::Issue {
            verb: IssueVerb::Comment,
            number: Some(number(n)?),
            repo,
            speaks: true,
        }),
        ("POST", ["pulls", n, what @ ("reviews" | "comments")]) => Some(Action::Pr {
            verb: if *what == "reviews" {
                PrVerb::Review
            } else {
                PrVerb::Comment
            },
            number: Some(number(n)?),
            repo,
            selector: None,
            speaks: true,
        }),
        ("PUT", ["pulls", n, "merge"]) => Some(Action::Pr {
            verb: PrVerb::Merge,
            number: Some(number(n)?),
            repo,
            selector: None,
            speaks: false,
        }),
        ("DELETE", ["git", "refs", "heads", branch @ ..]) if !branch.is_empty() => {
            Some(Action::Push {
                dir: None,
                remote: Some(repo.map_or_else(
                    || "origin".to_string(),
                    |r| format!("https://github.com/{r}"),
                )),
                refspecs: vec![format!(":refs/heads/{}", branch.join("/"))],
                force: false,
                delete: true,
                tags: false,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(cmd: &str) -> Vec<Action> {
        candidates("Bash", &json!({"command": cmd}))
    }

    #[test]
    fn ordinary_commands_are_not_candidates() {
        for c in [
            "ls -la",
            "cargo test",
            "git status",
            "git commit -m \"gh pr merge later\"",
            "echo \"git push\"",
            "grep -r 'git push' .",
            "git branch -d old",
            "git tag v1",
            "gh pr view 5",
            "gh pr create --fill",
            "gh issue create -t x",
            "gh api repos/o/r",
        ] {
            assert!(bash(c).is_empty(), "{c}");
        }
    }

    #[test]
    fn pushes_are_candidates_however_they_are_written() {
        assert!(matches!(
            &bash("git push")[..],
            [Action::Push { force: false, .. }]
        ));
        assert!(matches!(
            &bash("cd ../x && git push -f origin b")[..],
            [Action::Push { force: true, dir: Some(d), .. }] if d == Path::new("../x")
        ));
        assert!(matches!(
            &bash("git -C ../x push --force-with-lease")[..],
            [Action::Push { force: true, dir: Some(d), .. }] if d == Path::new("../x")
        ));
        assert!(matches!(
            &bash("git push origin +main")[..],
            [Action::Push { force: true, .. }]
        ));
        assert!(matches!(
            &bash("git push origin :old")[..],
            [Action::Push { delete: true, .. }]
        ));
        assert!(matches!(
            &bash("git push --delete origin old")[..],
            [Action::Push { delete: true, remote: Some(r), .. }] if r == "origin"
        ));
        assert!(matches!(
            &bash("git push origin refs/tags/v1.2")[..],
            [Action::Push { tags: true, .. }]
        ));
        assert!(matches!(
            &bash("git push origin HEAD:refs/tags/v1.2")[..],
            [Action::Push { tags: true, .. }]
        ));
        assert!(matches!(
            &bash("git push origin fix")[..],
            [Action::Push { tags: false, .. }]
        ));
        assert!(matches!(
            &bash("git push --tags")[..],
            [Action::Push { tags: true, .. }]
        ));
        assert!(matches!(
            &bash("env GIT_SSH_COMMAND=ssh git push")[..],
            [Action::Push { .. }]
        ));
    }

    #[test]
    fn gh_writes_are_candidates() {
        assert!(matches!(
            &bash("gh -R o/r pr merge 5 --squash")[..],
            [Action::Pr { verb: PrVerb::Merge, number: Some(5), repo: Some(r), speaks: false, .. }]
                if r == "o/r"
        ));
        assert!(matches!(
            &bash("gh pr comment 5 -b hi")[..],
            [Action::Pr { speaks: true, .. }]
        ));
        assert!(matches!(
            &bash("gh pr review 5 --approve")[..],
            [Action::Pr {
                verb: PrVerb::Review,
                speaks: true,
                ..
            }]
        ));
        assert!(matches!(
            &bash("gh pr close 5 --comment bye")[..],
            [Action::Pr { speaks: true, .. }]
        ));
        assert!(matches!(
            &bash("gh pr edit 5 --add-label x")[..],
            [Action::Pr {
                speaks: false,
                number: Some(5),
                ..
            }]
        ));
        assert!(matches!(
            &bash("gh issue comment 9 -b hi")[..],
            [Action::Issue {
                speaks: true,
                number: Some(9),
                ..
            }]
        ));
        assert!(matches!(
            &bash("sudo gh release create v1")[..],
            [Action::Release { .. }]
        ));
        assert!(matches!(
            &bash("gh api -X POST repos/o/r/labels -f name=x")[..],
            [Action::ApiWrite { .. }]
        ));
        assert!(matches!(
            &bash("gh api repos/o/r/issues/5/comments -f body=x")[..],
            [Action::Issue {
                verb: IssueVerb::Comment,
                speaks: true,
                ..
            }]
        ));
        assert!(matches!(
            &bash("git branch -D old")[..],
            [Action::BranchForceDelete { branch, .. }] if branch == "old"
        ));
    }

    #[test]
    fn one_line_can_hold_several() {
        assert_eq!(
            bash("git push origin fix && gh pr merge 5 --squash").len(),
            2
        );
    }

    #[test]
    fn github_mcp_writes_are_candidates_and_reads_are_not() {
        assert!(is_flag_tool(
            "mcp__plugin_github_github__merge_pull_request"
        ));
        assert!(!is_flag_tool("Edit"));
        assert_eq!(
            candidates("mcp__plugin_github_github__merge_pull_request", &json!({})).len(),
            1
        );
        for read in [
            "get_pull_request",
            "list_issues",
            "search_code",
            "pull_request_read",
            "issue_read",
        ] {
            let tool = format!("mcp__github__{read}");
            assert!(candidates(&tool, &json!({})).is_empty(), "{tool}");
        }
    }

    fn mcp(tool: &str, input: Value) -> Vec<Action> {
        candidates(&format!("mcp__plugin_github_github__{tool}"), &input)
    }

    #[test]
    fn an_unknown_github_mcp_tool_that_may_write_is_grey() {
        for tool in [
            "fork_repository",
            "assign_copilot_to_issue",
            "star_repository",
            "run_workflow",
        ] {
            assert!(
                matches!(
                    &mcp(tool, json!({"owner": "o", "repo": "r"}))[..],
                    [Action::McpWrite { .. }]
                ),
                "{tool}"
            );
        }
    }

    #[test]
    fn github_mcp_tools_map_to_their_actions() {
        let pr = |verb: PrVerb, speaks: bool| {
            vec![Action::Pr {
                verb,
                number: Some(5),
                repo: Some("o/r".into()),
                selector: None,
                speaks,
            }]
        };
        let issue = |verb: IssueVerb, speaks: bool| {
            vec![Action::Issue {
                verb,
                number: Some(9),
                repo: Some("o/r".into()),
                speaks,
            }]
        };
        let on_pr = |extra: Value| {
            let mut v = json!({"owner": "o", "repo": "r", "pullNumber": 5});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            v
        };
        let on_issue = |extra: Value| {
            let mut v = json!({"owner": "o", "repo": "r", "issue_number": 9});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            v
        };
        assert_eq!(
            mcp(
                "merge_pull_request",
                on_pr(json!({"merge_method": "squash"}))
            ),
            pr(PrVerb::Merge, false)
        );
        assert_eq!(
            mcp(
                "merge_pull_request",
                json!({"owner": "o", "repo": "r", "pull_number": "5"})
            ),
            pr(PrVerb::Merge, false)
        );
        assert_eq!(
            mcp("add_issue_comment", on_issue(json!({"body": "hi"}))),
            issue(IssueVerb::Comment, true)
        );
        for tool in [
            "add_comment_to_pending_review",
            "add_reply_to_pull_request_comment",
        ] {
            assert_eq!(
                mcp(tool, on_pr(json!({"body": "hi"}))),
                pr(PrVerb::Comment, true),
                "{tool}"
            );
        }
        for tool in [
            "create_pending_pull_request_review",
            "submit_pending_pull_request_review",
            "create_and_submit_pull_request_review",
        ] {
            assert_eq!(
                mcp(tool, on_pr(json!({"event": "APPROVE"}))),
                pr(PrVerb::Review, true),
                "{tool}"
            );
        }
        assert_eq!(
            mcp(
                "pull_request_review_write",
                on_pr(json!({"method": "create", "event": "APPROVE"}))
            ),
            pr(PrVerb::Review, true)
        );
        assert_eq!(
            mcp("update_pull_request", on_pr(json!({"title": "x"}))),
            pr(PrVerb::Edit, false)
        );
        assert_eq!(
            mcp("update_pull_request", on_pr(json!({"body": "new text"}))),
            pr(PrVerb::Edit, true)
        );
        assert_eq!(
            mcp("update_pull_request", on_pr(json!({"state": "closed"}))),
            pr(PrVerb::Close, false)
        );
        assert_eq!(
            mcp("update_issue", on_issue(json!({"state": "closed"}))),
            issue(IssueVerb::Close, false)
        );
        assert_eq!(
            mcp(
                "issue_write",
                on_issue(json!({"method": "update", "body": "x"}))
            ),
            issue(IssueVerb::Edit, true)
        );
        for tool in ["push_files", "create_or_update_file", "delete_file"] {
            assert_eq!(
                mcp(
                    tool,
                    json!({"owner": "o", "repo": "r", "branch": "fix", "path": "a"})
                ),
                vec![Action::Push {
                    dir: None,
                    remote: Some("https://github.com/o/r".into()),
                    refspecs: vec!["refs/heads/fix".into()],
                    force: false,
                    delete: false,
                    tags: false,
                }],
                "{tool}"
            );
        }
        // Opening is not an answer.
        for (tool, input) in [
            ("create_pull_request", json!({"owner": "o", "repo": "r"})),
            ("create_issue", json!({"owner": "o", "repo": "r"})),
            (
                "issue_write",
                json!({"method": "create", "owner": "o", "repo": "r"}),
            ),
        ] {
            assert!(mcp(tool, input).is_empty(), "{tool}");
        }
    }

    #[test]
    fn a_github_mcp_call_without_a_safe_target_is_grey_or_speaks_anyway() {
        // No branch: the default branch, which `aq` cannot name without a lookup.
        assert!(matches!(
            &mcp("create_or_update_file", json!({"owner": "o", "repo": "r"}))[..],
            [Action::McpWrite { .. }]
        ));
        assert!(matches!(
            &mcp(
                "merge_pull_request",
                json!({"owner": "-R", "repo": "r", "pullNumber": 5})
            )[..],
            [Action::McpWrite { .. }]
        ));
        assert!(matches!(
            &mcp("add_issue_comment", json!({"owner": "o", "body": "hi"}))[..],
            [Action::Issue {
                speaks: true,
                number: None,
                repo: None,
                ..
            }]
        ));
    }

    #[test]
    fn a_pr_named_by_branch_or_link_keeps_its_name_and_the_links_repo() {
        assert_eq!(
            bash("gh pr merge feature/12"),
            vec![Action::Pr {
                verb: PrVerb::Merge,
                number: None,
                repo: None,
                selector: Some("feature/12".into()),
                speaks: false,
            }]
        );
        assert_eq!(
            bash("gh pr comment https://github.com/a/b/pull/7 -b hi"),
            vec![Action::Pr {
                verb: PrVerb::Comment,
                number: Some(7),
                repo: Some("a/b".into()),
                selector: None,
                speaks: true,
            }]
        );
        assert!(matches!(
            &bash("gh issue close https://github.com/a/b/issues/9")[..],
            [Action::Issue { number: Some(9), repo: Some(r), .. }] if r == "a/b"
        ));
        // A link to another host is kept as it is.
        assert!(matches!(
            &bash("gh pr merge https://ghe.example/a/b/pull/7")[..],
            [Action::Pr { number: None, selector: Some(s), .. }] if s.contains("ghe.example")
        ));
        // The link's repo wins over `-R`, as it does for `gh`.
        assert!(matches!(
            &bash("gh pr merge -R x/y https://github.com/a/b/pull/7")[..],
            [Action::Pr { repo: Some(r), number: Some(7), .. }] if r == "a/b"
        ));
        assert!(matches!(
            &bash("for n in 1 2; do gh pr merge $n; done")[..],
            [Action::Pr { number: None, selector: Some(s), .. }] if s == "$n"
        ));
    }

    #[test]
    fn graphql_is_a_candidate_only_for_a_mutation_or_an_unread_query() {
        for c in [
            "gh api graphql -f query='query { viewer { login } }'",
            "gh api graphql --raw-field query='{ repository(owner:\"o\", name:\"r\") { id } }'",
            "gh api graphql -F owner=o -f query='query($owner:String!) { user(login:$owner) { id } }'",
        ] {
            assert!(bash(c).is_empty(), "{c}");
        }
        for c in [
            "gh api graphql -f query='mutation { addComment(input:{}) { clientMutationId } }'",
            "gh api graphql -F query=@q.graphql",
            "gh api graphql --input q.json",
        ] {
            assert!(matches!(&bash(c)[..], [Action::ApiWrite { .. }]), "{c}");
        }
    }

    #[test]
    fn rest_writes_that_aq_knows_become_their_actions() {
        assert!(matches!(
            &bash("gh api repos/o/r/issues/5/comments -f body=hi")[..],
            [Action::Issue { verb: IssueVerb::Comment, number: Some(5), repo: Some(r), speaks: true }]
                if r == "o/r"
        ));
        assert!(matches!(
            &bash("gh api -X POST /repos/{owner}/{repo}/issues/5/comments -f body=hi")[..],
            [Action::Issue {
                repo: None,
                speaks: true,
                ..
            }]
        ));
        assert!(matches!(
            &bash("gh api repos/o/r/pulls/5/reviews -f event=APPROVE")[..],
            [Action::Pr {
                verb: PrVerb::Review,
                number: Some(5),
                speaks: true,
                ..
            }]
        ));
        assert!(matches!(
            &bash("gh api repos/o/r/pulls/5/comments -f body=x")[..],
            [Action::Pr {
                verb: PrVerb::Comment,
                speaks: true,
                ..
            }]
        ));
        assert!(matches!(
            &bash("gh api -X PUT repos/o/r/pulls/5/merge")[..],
            [Action::Pr {
                verb: PrVerb::Merge,
                number: Some(5),
                speaks: false,
                ..
            }]
        ));
        assert_eq!(
            bash("gh api -X DELETE repos/o/r/git/refs/heads/feat/x"),
            vec![Action::Push {
                dir: None,
                remote: Some("https://github.com/o/r".into()),
                refspecs: vec![":refs/heads/feat/x".into()],
                force: false,
                delete: true,
                tags: false,
            }]
        );
        for c in [
            "gh api -X PATCH repos/o/r -f description=x",
            "gh api -X DELETE repos/o/r/git/refs/tags/v1",
            "gh api repos/o/r/pulls/x/reviews -f event=APPROVE",
        ] {
            assert!(matches!(&bash(c)[..], [Action::ApiWrite { .. }]), "{c}");
        }
    }

    #[test]
    fn mirror_prune_follow_tags_and_all_pushes() {
        assert!(matches!(
            &bash("git push --mirror origin")[..],
            [Action::Push { delete: true, refspecs, .. }] if refspecs.iter().any(|r| r.contains('*'))
        ));
        assert!(matches!(
            &bash("git push --prune origin")[..],
            [Action::Push { delete: true, .. }]
        ));
        assert!(matches!(
            &bash("git push --follow-tags")[..],
            [Action::Push {
                tags: true,
                delete: false,
                ..
            }]
        ));
        assert!(matches!(
            &bash("git push --all origin")[..],
            [Action::Push { delete: false, force: false, tags: false, refspecs, .. }]
                if refspecs == &["refs/heads/*"]
        ));
    }

    #[test]
    fn a_line_nested_too_deeply_is_never_cleared() {
        let mut line = "git push".to_string();
        for _ in 0..(MAX_DEPTH + 2) {
            line = format!("echo \"$({line})\"");
        }
        assert!(bash(&line).contains(&Action::TooDeep));
        // A deep line with no action in it at all is still not cleared.
        let mut harmless = "ls".to_string();
        for _ in 0..(MAX_DEPTH + 2) {
            harmless = format!("echo \"$({harmless})\"");
        }
        assert_eq!(bash(&harmless), [Action::TooDeep]);
        // Within the limit, scanning works as before.
        assert!(matches!(
            &bash("echo \"$(echo \"$(git push)\")\"")[..],
            [Action::Push { .. }]
        ));
    }

    #[test]
    fn commands_inside_substitutions_are_candidates() {
        for c in [
            "OUT=\"$(gh pr merge 5)\"",
            "echo `git push`",
            "echo \"result: `git push origin fix`\"",
            "echo \"$(cd x && git push)\"",
            "X=\"$(echo \"$(gh pr merge 5)\")\"",
            "echo \"$(printf '%s' ')'; git push)\"",
        ] {
            assert_eq!(bash(c).len(), 1, "{c}");
        }
        for c in [
            "echo \"git push\"",
            "git commit -m \"gh pr merge\"",
            "echo 'echo $(git push)'",
            "echo \"\\`git push\\`\"",
        ] {
            assert!(bash(c).is_empty(), "{c}");
        }
    }

    #[test]
    fn heredoc_bodies_and_comments_are_not_parsed() {
        assert!(bash("cat <<EOF\ngit push\ngh pr merge 5\nEOF").is_empty());
        assert!(bash("echo hi # git push").is_empty());
        assert_eq!(bash("cat <<'EOF' | sh\nx\nEOF\ngit push").len(), 1);
    }

    #[test]
    fn a_shell_script_argument_is_scanned() {
        assert_eq!(bash("bash -c 'cd x && git push'").len(), 1);
    }

    #[test]
    fn a_cd_carries_to_later_commands_in_the_line() {
        assert!(matches!(
            &bash("cd /a; ls; git push")[..],
            [Action::Push { dir: Some(d), .. }] if d == Path::new("/a")
        ));
    }

    #[test]
    fn shell_keywords_and_wrappers_do_not_hide_a_command() {
        for c in [
            "for n in 1 2; do gh pr merge $n; done",
            "if true; then git push; fi",
            "{ git push; }",
            "! git push",
            "time git push",
            "nohup git push",
            "timeout 10 git push",
            "timeout -s KILL 10 git push",
            "nice -n 5 git push",
            "echo 5 | xargs -I {} gh pr merge {}",
            "(cd x && git push)",
        ] {
            assert_eq!(bash(c).len(), 1, "{c}");
        }
    }

    #[test]
    fn redirections_are_not_arguments() {
        for c in [
            "git push 2>&1",
            "git push >/dev/null 2>&1",
            "git push &> out.log",
            "git push > out.log",
            "git push 2>/dev/null <<< hi",
        ] {
            assert!(
                matches!(
                    &bash(c)[..],
                    [Action::Push { remote: None, refspecs, .. }] if refspecs.is_empty()
                ),
                "{c}"
            );
        }
        assert!(matches!(
            &bash("git push origin main >/dev/null 2>&1")[..],
            [Action::Push { remote: Some(r), refspecs, .. }]
                if r == "origin" && refspecs == &["main"]
        ));
        assert!(matches!(
            &bash("gh pr merge 5 2>&1 | tail")[..],
            [Action::Pr {
                number: Some(5),
                ..
            }]
        ));
    }

    #[test]
    fn a_multibyte_flag_does_not_panic() {
        assert!(bash("gh -é pr merge 5").len() <= 1);
        assert!(bash("gh pr merge -é 5").len() <= 1);
    }

    #[test]
    fn gh_value_flags_do_not_become_words() {
        assert!(matches!(
            &bash("gh api --jq .x repos/o/r/issues -f a=b")[..],
            [Action::ApiWrite { path, .. }] if path == "repos/o/r/issues"
        ));
        assert!(matches!(
            &bash("gh pr merge --match-head-commit 123456 5")[..],
            [Action::Pr {
                number: Some(5),
                ..
            }]
        ));
        assert!(matches!(
            &bash("gh pr edit --add-reviewer bob 7")[..],
            [Action::Pr {
                number: Some(7),
                ..
            }]
        ));
    }

    #[test]
    fn a_number_comes_from_an_integer_or_a_url_only() {
        assert!(matches!(
            &bash("gh pr merge feature/12")[..],
            [Action::Pr { number: None, .. }]
        ));
        assert!(matches!(
            &bash("gh pr merge https://github.com/o/r/pull/12")[..],
            [Action::Pr {
                number: Some(12),
                ..
            }]
        ));
        assert!(matches!(
            &bash("gh issue close https://github.com/o/r/issues/9")[..],
            [Action::Issue {
                number: Some(9),
                ..
            }]
        ));
    }

    #[test]
    fn reopen_with_a_comment_speaks() {
        assert!(matches!(
            &bash("gh pr reopen 5 --comment x")[..],
            [Action::Pr { speaks: true, .. }]
        ));
        assert!(matches!(
            &bash("gh issue reopen 5 -c x")[..],
            [Action::Issue { speaks: true, .. }]
        ));
        assert!(matches!(
            &bash("gh pr reopen 5")[..],
            [Action::Pr { speaks: false, .. }]
        ));
    }

    #[test]
    fn wrapper_flags_take_a_value_only_where_the_wrapper_says_so() {
        for c in [
            "sudo -E git push",
            "sudo -n git push",
            "time -p git push",
            "sudo -u bob git push",
            "nice -n 5 git push",
            "env -i git push",
            "env -u HOME git push",
        ] {
            assert_eq!(bash(c).len(), 1, "{c}");
        }
    }

    #[test]
    fn short_flags_that_are_boolean_do_not_swallow_the_number() {
        assert!(matches!(
            &bash("gh pr merge -r 5")[..],
            [Action::Pr {
                number: Some(5),
                ..
            }]
        ));
        assert!(matches!(
            &bash("gh pr review -a 5")[..],
            [Action::Pr {
                number: Some(5),
                speaks: true,
                ..
            }]
        ));
    }

    #[test]
    fn a_dangling_redirect_does_not_swallow_the_next_command() {
        assert_eq!(bash("echo hi >\ngit push").len(), 1);
    }
}
