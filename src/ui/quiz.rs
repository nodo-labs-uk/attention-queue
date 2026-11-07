//! The quiz card: the last answered question with its verdict, then the
//! current one. And the code view `m` opens.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::card_header;
use crate::app::App;
use crate::highlight::Highlighter;
use crate::model::{Card, CardKind};
use crate::quiz::{
    Answer, Code, Given, JudgeVerdict, Question, QuestionKind, QuizCard, QuizState, Snippet,
    Verdict,
};
use crate::theme::Theme;

/// Lines of code either side of `code_ref` shown under a wrong answer.
const INLINE_CONTEXT: u32 = 3;

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
            lines.push(Line::styled("r tries again.", dim));
        }
        QuizState::Asking(i) => {
            let i = *i;
            if i > 0 {
                question_block(&mut lines, qc, i - 1, width, t, hl);
                lines.push(Line::raw(""));
            }
            question_block(&mut lines, qc, i, width, t, hl);
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

fn question_block(
    lines: &mut Vec<Line<'static>>,
    qc: &QuizCard,
    i: usize,
    width: usize,
    t: &Theme,
    hl: &mut Highlighter,
) {
    let q = &qc.questions[i];
    let answer = qc.answers.get(i);
    let label = match q.kind {
        QuestionKind::Choice => "multiple choice",
        QuestionKind::Free => "free text",
    };
    lines.push(Line::styled(label, Style::new().fg(t.dim)));
    push_wrapped(
        lines,
        &q.question,
        width,
        Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        "",
    );
    match q.kind {
        QuestionKind::Choice => {
            let picked = match answer.map(|a| &a.given) {
                Some(Given::Choice(p)) => Some(*p),
                _ => None,
            };
            for (n, c) in q.choices.iter().enumerate() {
                let (mark, style) = match picked {
                    Some(_) if q.answer_index == Some(n) => ("✓", Style::new().fg(t.ok)),
                    Some(p) if p == n => ("✗", Style::new().fg(t.del)),
                    Some(_) => (" ", Style::new().fg(t.dim)),
                    None => (" ", Style::new().fg(t.fg)),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {mark} "), style),
                    Span::styled(
                        format!("{} ", n + 1),
                        Style::new().fg(t.key).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(c.clone(), style),
                ]));
            }
        }
        QuestionKind::Free => {
            if let Some(Answer {
                given: Given::Free(text),
                ..
            }) = answer
            {
                for l in text.lines() {
                    push_wrapped(lines, l, width, Style::new().fg(t.acc), "  ");
                }
            }
        }
    }
    if let Some(a) = answer {
        verdict_block(lines, q, a, qc.code.get(&i), width, t, hl);
    }
}

fn verdict_block(
    lines: &mut Vec<Line<'static>>,
    q: &Question,
    a: &Answer,
    code: Option<&Code>,
    width: usize,
    t: &Theme,
    hl: &mut Highlighter,
) {
    let cite = format!("{}:{}", q.code_ref.path.display(), q.code_ref.line);
    let dim = Style::new().fg(t.dim);
    let fg = Style::new().fg(t.fg);
    let (colour, text, rest, code_gap): (Color, String, String, bool) = match &a.verdict {
        Verdict::Right => (
            t.ok,
            format!("✓ right. {}", q.explanation),
            cite.clone(),
            false,
        ),
        Verdict::Wrong => {
            let right = q.answer_index.map_or(0, |n| n + 1);
            (
                t.del,
                format!("✗ not quite: the answer is {right}. {}", q.explanation),
                String::new(),
                false,
            )
        }
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
    if a.verdict == Verdict::Wrong {
        match code {
            Some(Code::Loaded(s)) => {
                let near = s.target.saturating_sub(INLINE_CONTEXT)..=s.target + INLINE_CONTEXT;
                for l in code_lines(s, Some(near), t, hl) {
                    let mut spans = vec![Span::styled(bar, bar_style)];
                    spans.extend(l.spans);
                    lines.push(Line::from(spans));
                }
            }
            Some(Code::Failed(e)) => push_barred(lines, e, width, dim, bar, bar_style),
            _ => push_barred(lines, &format!("{cite} ..."), width, dim, bar, bar_style),
        }
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
/// `code_ref` line marked.
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
            let target = n == s.target;
            let num_style = if target {
                Style::new().fg(t.acc).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(t.dim)
            };
            let mut spans = vec![Span::styled(format!("{n:>5} "), num_style)];
            for (style, piece) in hl.highlight_line(&s.path, n, text) {
                let style = if target { style.bg(t.sel) } else { style };
                spans.push(Span::styled(piece, style));
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
        .title(format!(
            " {}:{} ",
            q.code_ref.path.display(),
            q.code_ref.line
        ))
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
    use crossterm::event::{KeyCode, KeyEvent};
    use std::time::Instant;

    use crate::app::Msg;
    use crate::llm::{FakeLlm, run_job};
    use crate::quiz::tests::{CODE_GAP, answer_free, asking};
    use crate::ui::tests::render;

    fn key(c: char) -> Msg {
        Msg::Key(KeyEvent::from(KeyCode::Char(c)), Instant::now())
    }

    #[test]
    fn quiz_choice() {
        let a = asking();
        insta::assert_snapshot!(render(&a, 90, 16));
    }

    #[test]
    fn quiz_free() {
        let mut a = asking();
        a.update(key('3'));
        let job = answer_free(&mut a, "so a client at zero doesn't go negative");
        let fake = FakeLlm::new(vec![Ok(CODE_GAP.into())]);
        a.update(Msg::Llm(run_job(&fake, job)));
        insta::assert_snapshot!(render(&a, 90, 22));
    }
}
