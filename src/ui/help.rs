use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph, Wrap};

use crate::theme::Theme;

/// The help page F1 opens. Lines starting with `# ` are headings.
pub const TEXT: &str = "\
# What aq does
aq collects what your Claude sessions in tmux need from you and shows it
one card at a time. The top bar counts what is waiting behind the card.

# Modes
Each project (git checkout) has one mode, shown on its tab when it is not
review. M on a card from that project, or on its tab when nothing waits,
moves it to the next mode. A new mode applies from the session's next
finished turn; the card on screen stays as it is.
  review board   when a session finishes, review its whole diff and
                 send line comments back as one message
  step by step   each edit waits for you before it is applied; needs
                 the aq hooks, so restart sessions started before them
  vibe           nothing is checked: no cards when a session finishes
  quiz           when a session finishes, explain pieces of its diff in
                 your own words: what the code does, or why it was done
                 that way; one question on the whole change comes last.
                 A gap the judge finds becomes keep-as-is or send-a-fix

# Every card
  ?    ask the agent a side question      Space  skip this card
  W    where are we: a short summary of what is done and what is left
  N    a new Claude session in this project, in a tmux window of its own;
       what you type is its first prompt, and Tab gives it a git worktree
       of its own (claude --worktree). Ctrl-O there picks another folder:
       type a path (Tab completes, arrows choose a match), and one that
       is not there yet is made when the session starts
  !    a shell in a popup, in this project; for sudo, logins, or commands
       Claude may not run. Claude's last suggested `! command` is copied
       and, in zsh, typed in ready to run. It closes when the shell exits
  M    next mode for this project         t    next theme
  F1   this help                          Q    quit aq
  F2   the tour again

# Moving around, vim style
  j/k        line                 Ctrl-d/Ctrl-u  half a page
  gg/G       first/last line      Ctrl-f/Ctrl-b  whole page
  {/} or [c/]c   previous/next change
  H/L or [[/]]   previous/next file; tab also goes to the next one
                 (on a step card, H/L and tab move between changes)

# Review card
  c  comment     x  delete comment   s  send comments   a  approve all
  d  mark the file done: it is ticked and skipped   u  show done files again
  j/k line   V range   ]c/[c change   H/L or tab file
  v  split or unified view           r  whole branch or this round
  m  Markdown as written, or as read (the default)
  f  hide or show the file list

# Step card
  y  allow   n  reject with a reason   e  ask for a different change
  j/k move   H/L or tab change   za fold   zR unfold all
  w/b word   gd find the word in the project   m raw input

# Shell change card
  y  keep the change   u  ask Claude to undo it
  j/k line   ]c/[c change   H/L or tab file   v view   f file list

# Permission card
  y  allow (once you have scrolled to the end)   n  deny with a reason
  j/k scroll   PgUp/PgDn page   G end   g  go to the agent's window

# Question card
When a session ends its turn by asking you something (to approve a
draft, to choose between two ways), its message becomes a card.
  a  approve (sends: Approved, carry on.)   Enter  reply in your own words
     a is there only when the question asks for a go-ahead
  x  put the card away   g  go to the session's window   j/k scroll

When Claude asks with its own menu of options, the card lists them:
  1-9  pick   j/k + Enter  the same
  where several apply: Space or 1-9 ticks, Enter answers
  o  answer in your own words   u  back a question
  x  let the menu show in the session's window instead   g  and go there
This needs AskUserQuestion in the PreToolUse hook's matcher.

# Quiz card
Each question opens with its piece of the diff and a box for your answer.
  Ctrl-S or Enter on an empty line  submit    Esc  put the box away
  Enter  answer again after Esc      m  show the whole file
  y  fine as is   c  ask for a fix
An answer that is not right gets a leading question and another try,
twice at most, before the answer is shown.   e  show the answer now
  r  judge or generate again   x  dismiss a failed quiz

# Nothing waiting
A tab per session along the top (● waiting, ○ working); the picked
one's screen fills the rest. After you send or approve a review, that
session is the one picked.
  h/l    pick a session (j/k too)   g  go to its window
  Enter  message it: sent now, or once it is idle (Esc cancels)
  p      ask an idle session to open a PR (/create-pr) and go there
  m      switch an idle session's model: f fable, o opus, s sonnet, h haiku
         (/model, which also makes it the default for new sessions)
  ?  W  ask it a side question, or where it is up to; a copy of the
         session answers, so the session itself never sees it
  x      close the session's pane, after a yes (claude --resume reopens it)
  Esc Esc  stop what it is doing, as Esc in Claude Code; also drops a
         message queued for it
  M      next mode for its project; the only way back from vibe mode
  Ctrl-U/Ctrl-D, PgUp/PgDn or the wheel   scroll its screen back
  Home   its oldest line     G or End   back to the latest

# Typing in a box
  Ctrl-U  delete to the line start     Ctrl-K  delete to the line end
  Ctrl-W  delete the word before       Alt-Backspace  same, up to punctuation
  Ctrl-A/Ctrl-E  line start/end        Alt-B/Alt-F  word back/forward

# Good to know
A card that takes the screen by itself ignores deciding keys for 1.5 s.
Messages wait until their session is idle at its prompt.
Going to a window leaves aq running; come back with your tmux keys.";

/// The help page over `area`, scrolled down by `scroll` lines.
pub fn render(frame: &mut Frame, area: Rect, scroll: u16, t: &Theme) {
    let lines: Vec<Line> = TEXT
        .lines()
        .map(|l| match l.strip_prefix("# ") {
            Some(h) => Line::from(Span::styled(
                h.to_string(),
                Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
            )),
            None => Line::styled(l.to_string(), Style::new().fg(t.fg)),
        })
        .collect();
    let block = Block::bordered()
        .border_style(Style::new().fg(t.dim))
        .title(Span::styled(
            " Help ",
            Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::new().bg(t.bg))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}
