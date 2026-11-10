//! The quiz card: the last answered question with its verdict, then the
//! current one under the piece of the diff it asks about. The answer is
//! typed in a modal showing that piece again. And the code view `m` opens.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::card_header;
use super::input;
use crate::app::App;
use crate::git::DiffLine;
use crate::highlight::Highlighter;
use crate::model::{Card, CardKind};
use crate::quiz::{AnswerDraft, Code, JudgeVerdict, QuizCard, QuizState, Scope, Snippet, Verdict};
use crate::review::file_path;
use crate::theme::Theme;

/// Lines either side of the piece, when it is shown from the file because
/// the diff does not have it.
const PIECE_CONTEXT: u32 = 1;
/// Rows of the answer box in the modal.
const ANSWER_ROWS: u16 = 9;

/// For the top bar: which questions are on screen, and the score.
pub fn progress(qc: &QuizCard) -> Option<(String, String)> {
    let QuizState::Asking(i) = qc.state else {
        return None;
    };
    let n = qc.questions.len();
    let which = if i > 0 {
        format!("question {i} and {} of {n}", i + 1)
    } else {
        format!("question {} of {n}", i + 1)
    };
    let (right, total) = qc.score();
    Some((which, format!("score so far {right}/{total}")))
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    card: &Card,
    qc: &QuizCard,
    hl: &mut Highlighter,
) {
    let t = app.theme();
    let inner = area.inner(Margin::new(1, 0));
    let width = inner.width.saturating_sub(2).max(10) as usize;
    let mut lines = card_header(app, card);
    lines.push(Line::raw(""));
    let dim = Style::new().fg(t.dim);
    match &qc.state {
        QuizState::Generating => {
            lines.push(Line::styled("Writing questions about this change...", dim))
        }
        QuizState::Failed(e) => {
            push_wrapped(
                &mut lines,
                &format!("Could not write the questions: {e}"),
                width,
                Style::new().fg(t.del),
                "",
            );
            lines.push(Line::styled("r tries again; x dismisses the quiz.", dim));
        }
        QuizState::Asking(i) => {
            let i = *i;
            if i > 0 {
                question_block(&mut lines, qc, i - 1, width, t);
                lines.push(Line::raw(""));
            }
            lines.extend(piece_lines(qc, i, t, hl));
            lines.push(Line::raw(""));
            question_block(&mut lines, qc, i, width, t);
        }
        QuizState::Done => {
            let (right, total) = qc.score();
            lines.push(Line::styled(
                format!("Quiz done. Score {right}/{total}."),
                Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
            ));
            if qc.fixes.is_empty() {
                lines.push(Line::styled("Approving the change.", dim));
            } else {
                lines.push(Line::styled("Asking the agent to fix:", dim));
                for f in &qc.fixes {
                    push_wrapped(&mut lines, f, width, Style::new().fg(t.fg), "- ");
                }
            }
            if !app.sending.contains_key(&card.id) {
                lines.push(Line::styled("Enter sends it.", dim));
            }
        }
    }
    // Keep the end, the current question, in view.
    let scroll = (lines.len() as u16).saturating_sub(inner.height);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}

/// The answer modal: the piece of the diff, the question, and a box for
/// the answer in the developer's own words.
pub fn render_answer(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    draft: &AnswerDraft,
    hl: &mut Highlighter,
) {
    let t = app.theme();
    let Some(CardKind::Quiz(qc)) = app
        .queue
        .iter()
        .find(|c| c.id == draft.card)
        .map(|c| &c.kind)
    else {
        return;
    };
    let Some(q) = qc.questions.get(draft.question) else {
        return;
    };
    let [h] = Layout::horizontal([Constraint::Length(area.width.min(120))])
        .flex(Flex::Center)
        .areas(area);
    let [panel] = Layout::vertical([Constraint::Percentage(92)])
        .flex(Flex::Center)
        .areas(h);
    frame.render_widget(Clear, panel);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(format!(
            " Question {} of {} ",
            draft.question + 1,
            qc.questions.len()
        ))
        .style(Style::new().bg(t.panel).fg(t.fg));
    let inner = block.inner(panel).inner(Margin::new(1, 0));
    frame.render_widget(block, panel);
    let width = inner.width.max(10) as usize;
    let mut question = Vec::new();
    question_text(&mut question, q.scope, &q.question, width, t);
    let [piece, ask, answer] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(question.len() as u16 + 1),
        Constraint::Length(ANSWER_ROWS),
    ])
    .areas(inner);
    let piece_rows = piece_lines(qc, draft.question, t, hl);
    let cut = piece_rows.len() > piece.height as usize;
    let mut shown: Vec<Line> = piece_rows.into_iter().take(piece.height as usize).collect();
    if cut && let Some(last) = shown.last_mut() {
        *last = Line::styled(
            "… more below: m shows the whole file",
            Style::new().fg(t.dim),
        );
    }
    frame.render_widget(Paragraph::new(shown), piece);
    frame.render_widget(Paragraph::new(question), ask.inner(Margin::new(0, 0)));
    input::render_sized(
        frame,
        answer,
        &draft.input,
        "Your answer",
        "submit",
        t,
        (answer.width, answer.height),
    );
}

/// What question `i` is about: its piece of the diff, or the piece from the
/// file when the diff does not have it, or the files of a whole change.
fn piece_lines(qc: &QuizCard, i: usize, t: &Theme, hl: &mut Highlighter) -> Vec<Line<'static>> {
    let q = &qc.questions[i];
    let dim = Style::new().fg(t.dim);
    if q.scope == Scope::Change {
        let mut lines = vec![Line::styled("The whole change:", dim)];
        for f in &qc.files {
            let (mut add, mut del) = (0, 0);
            for l in f.hunks.iter().flat_map(|h| &h.lines) {
                match l {
                    DiffLine::Add { .. } => add += 1,
                    DiffLine::Del { .. } => del += 1,
                    DiffLine::Context { .. } => {}
                }
            }
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}  ", file_path(f).display()),
                    Style::new().fg(t.fg),
                ),
                Span::styled(format!("+{add} "), Style::new().fg(t.add)),
                Span::styled(format!("-{del}"), Style::new().fg(t.del)),
            ]));
        }
        return lines;
    }
    let mut lines = vec![Line::styled(q.code_ref.cite(), dim)];
    let excerpt = qc.excerpt(i);
    if !excerpt.is_empty() {
        lines.extend(diff_lines(&q.code_ref.path, &excerpt, t, hl));
        return lines;
    }
    match qc.code.get(&i) {
        Some(Code::Loaded(s)) => {
            let near = s.target.saturating_sub(PIECE_CONTEXT)..=s.end + PIECE_CONTEXT;
            lines.extend(code_lines(s, Some(near), t, hl));
        }
        Some(Code::Failed(e)) => lines.push(Line::styled(e.clone(), Style::new().fg(t.del))),
        _ => lines.push(Line::styled("Reading the code...", dim)),
    }
    lines
}

/// Diff lines numbered, marked `+`/`-` and coloured as in the review card.
fn diff_lines(
    path: &std::path::Path,
    excerpt: &[&DiffLine],
    t: &Theme,
    hl: &mut Highlighter,
) -> Vec<Line<'static>> {
    excerpt
        .iter()
        .map(|l| {
            let (no, sign, bg, text) = match l {
                DiffLine::Context { new, text, .. } => (*new, " ", None, text),
                DiffLine::Add { new, text } => (*new, "+", Some((t.add, t.add_bg)), text),
                DiffLine::Del { old, text } => (*old, "-", Some((t.del, t.del_bg)), text),
            };
            let mut spans = vec![
                Span::styled(format!("{no:>5} "), Style::new().fg(t.dim)),
                Span::styled(
                    sign.to_string(),
                    Style::new().fg(bg.map_or(t.dim, |(fg, _)| fg)),
                ),
            ];
            for (style, piece) in hl.highlight_line(path, no, text) {
                let style = match bg {
                    Some((_, b)) => style.bg(b),
                    None => style,
                };
                spans.push(Span::styled(crate::visible::visible_code(&piece), style));
            }
            Line::from(spans)
        })
        .collect()
}

/// The label and the question, wrapped to `width`.
fn question_text(
    lines: &mut Vec<Line<'static>>,
    scope: Scope,
    question: &str,
    width: usize,
    t: &Theme,
) {
    if scope == Scope::Change {
        lines.push(Line::styled(
            "About the whole change",
            Style::new().fg(t.dim),
        ));
    }
    push_wrapped(
        lines,
        question,
        width,
        Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        "",
    );
}

fn question_block(
    lines: &mut Vec<Line<'static>>,
    qc: &QuizCard,
    i: usize,
    width: usize,
    t: &Theme,
) {
    let q = &qc.questions[i];
    question_text(lines, q.scope, &q.question, width, t);
    match qc.answers.get(i) {
        None => lines.push(Line::from(vec![
            Span::styled("Enter", Style::new().fg(t.key).add_modifier(Modifier::BOLD)),
            Span::styled(" answer in your own words", Style::new().fg(t.dim)),
        ])),
        Some(a) => {
            for l in a.text.lines() {
                push_wrapped(lines, l, width, Style::new().fg(t.acc), "  ");
            }
            verdict_block(lines, &a.verdict, width, t);
        }
    }
}

fn verdict_block(lines: &mut Vec<Line<'static>>, verdict: &Verdict, width: usize, t: &Theme) {
    let dim = Style::new().fg(t.dim);
    let fg = Style::new().fg(t.fg);
    let (colour, text, rest, code_gap): (Color, String, String, bool) = match verdict {
        Verdict::Judging => (t.dim, "Judging your answer...".into(), String::new(), false),
        Verdict::JudgeFailed(e) => (
            t.del,
            format!("Could not judge the answer: {e}"),
            "r tries again, Enter skips the question.".into(),
            false,
        ),
        Verdict::Judged(j) => {
            let head = match j.verdict {
                JudgeVerdict::Right => "✓ right.".to_string(),
                JudgeVerdict::Partial if j.gap.is_empty() => "Mostly right.".into(),
                JudgeVerdict::Partial => format!("Mostly right. One gap: {}", j.gap),
                JudgeVerdict::Wrong if j.gap.is_empty() => "✗ not quite.".into(),
                JudgeVerdict::Wrong => format!("✗ not quite. {}", j.gap),
            };
            let colour = match j.verdict {
                JudgeVerdict::Right => t.ok,
                JudgeVerdict::Partial => t.hi,
                JudgeVerdict::Wrong => t.del,
            };
            let code_gap = j.gap_in == crate::quiz::GapIn::Code;
            (colour, head, j.explanation.clone(), code_gap)
        }
    };
    let bar = "│ ";
    let bar_style = Style::new().fg(colour);
    push_barred(lines, &text, width, fg, bar, bar_style);
    if !rest.is_empty() {
        push_barred(lines, &rest, width, dim, bar, bar_style);
    }
    if code_gap {
        let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
        lines.push(Line::from(vec![
            Span::styled(bar, bar_style),
            Span::styled("y", key),
            Span::styled(" fine as is   ", dim),
            Span::styled("c", key),
            Span::styled(" fix it   ", dim),
            Span::styled("m", key),
            Span::styled(" show the code", dim),
        ]));
    }
}

/// Numbered, highlighted lines of `s` (those in `range`, or all), the
/// `code_ref` piece marked.
fn code_lines(
    s: &Snippet,
    range: Option<std::ops::RangeInclusive<u32>>,
    t: &Theme,
    hl: &mut Highlighter,
) -> Vec<Line<'static>> {
    (s.first..)
        .zip(&s.lines)
        .filter(|(n, _)| range.as_ref().is_none_or(|r| r.contains(n)))
        .map(|(n, text)| {
            let target = (s.target..=s.end).contains(&n);
            let num_style = if target {
                Style::new().fg(t.acc).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(t.dim)
            };
            let mut spans = vec![Span::styled(format!("{n:>5} "), num_style)];
            for (style, piece) in hl.highlight_line(&s.path, n, text) {
                let style = if target { style.bg(t.sel) } else { style };
                spans.push(Span::styled(crate::visible::visible_code(&piece), style));
            }
            Line::from(spans)
        })
        .collect()
}

/// The code view: the lines around a question's `code_ref`.
pub fn render_code(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    card: u64,
    question: usize,
    hl: &mut Highlighter,
) {
    let t = app.theme();
    let Some(CardKind::Quiz(qc)) = app.queue.iter().find(|c| c.id == card).map(|c| &c.kind) else {
        return;
    };
    let Some(q) = qc.questions.get(question) else {
        return;
    };
    let [h] = Layout::horizontal([Constraint::Percentage(90)])
        .flex(Flex::Center)
        .areas(area);
    let [panel] = Layout::vertical([Constraint::Percentage(90)])
        .flex(Flex::Center)
        .areas(h);
    frame.render_widget(Clear, panel);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(format!(" {} ", q.code_ref.cite()))
        .style(Style::new().bg(t.panel).fg(t.fg));
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    let lines = match qc.code.get(&question) {
        Some(Code::Loaded(s)) => code_lines(s, None, t, hl),
        Some(Code::Failed(e)) => vec![Line::styled(e.clone(), Style::new().fg(t.del))],
        _ => vec![Line::styled("Loading...", Style::new().fg(t.dim))],
    };
    // The marked line in the middle.
    let target = match qc.code.get(&question) {
        Some(Code::Loaded(s)) => s.target.saturating_sub(s.first) as u16,
        _ => 0,
    };
    let scroll = target
        .saturating_sub(inner.height / 2)
        .min((lines.len() as u16).saturating_sub(inner.height));
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}

/// Word-wraps `text` to `width`, each line starting with `prefix`.
fn wrap(text: &str, width: usize, prefix: &str) -> Vec<String> {
    let room = width.saturating_sub(prefix.chars().count()).max(10);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > room {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push(line);
    out.into_iter().map(|l| format!("{prefix}{l}")).collect()
}

fn push_wrapped(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    width: usize,
    style: Style,
    prefix: &str,
) {
    lines.extend(
        wrap(text, width, prefix)
            .into_iter()
            .map(|l| Line::styled(l, style)),
    );
}

fn push_barred(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    width: usize,
    style: Style,
    bar: &'static str,
    bar_style: Style,
) {
    for l in wrap(text, width, "")
        .into_iter()
        .map(|l| Line::from(vec![Span::styled(bar, bar_style), Span::styled(l, style)]))
    {
        lines.push(l);
    }
}

#[cfg(test)]
mod tests {
    use crate::quiz::tests::{CODE_GAP, RIGHT, answered, asking};
    use crate::ui::tests::render;

    #[test]
    fn quiz_question() {
        let a = asking();
        insta::assert_snapshot!(render(&a, 90, 24));
    }

    #[test]
    fn quiz_verdict() {
        let mut a = asking();
        answered(&mut a, RIGHT);
        answered(&mut a, CODE_GAP);
        insta::assert_snapshot!(render(&a, 90, 24));
    }
}
