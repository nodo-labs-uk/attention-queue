use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::theme::Theme;
use crate::tour::{PAGES, STEPS};

/// Width of the key column on a page.
const KEY_W: usize = 12;

/// Page `page` of the tour in a panel of fixed size over `area`, so the
/// hints along its bottom stay put from page to page.
pub fn render(frame: &mut Frame, area: Rect, page: usize, t: &Theme) {
    let Some(p) = PAGES.get(page) else {
        return;
    };
    let [h] = Layout::horizontal([Constraint::Length(80.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [panel] = Layout::vertical([Constraint::Length(20.min(area.height))])
        .flex(Flex::Center)
        .areas(h);
    frame.render_widget(Clear, panel);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let last = page + 1 == PAGES.len();
    let hints = Line::from(vec![
        Span::styled(" →", key),
        Span::styled(
            match p.practice {
                Some(_) => " start  ",
                None if last => " finish  ",
                None => " next  ",
            },
            dim,
        ),
        Span::styled("←", key),
        Span::styled(" back  ", dim),
        Span::styled("Esc", key),
        Span::styled(" skip ", dim),
    ]);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.acc))
        .title(Span::styled(
            format!(" Tour · {} of {} ", page + 1, PAGES.len()),
            Style::new().fg(t.acc).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(hints.right_aligned())
        .padding(Padding::new(2, 2, 1, 0))
        .style(Style::new().bg(t.panel).fg(t.fg));
    let mut lines = vec![
        Line::styled(p.title, Style::new().fg(t.hi).add_modifier(Modifier::BOLD)),
        Line::raw(""),
    ];
    for l in p.lines {
        lines.push(
            match l.strip_prefix("key:").and_then(|k| k.split_once('\t')) {
                Some((k, what)) => Line::from(vec![
                    Span::styled(format!("{k:<KEY_W$}"), key),
                    Span::styled(what, Style::new().fg(t.fg)),
                ]),
                None => Line::styled(*l, Style::new().fg(t.fg)),
            },
        );
    }
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    frame.render_widget(Paragraph::new(lines), inner);
    // Where you are, along the bottom of the text.
    let dots: Vec<Span> = (0..PAGES.len())
        .map(|i| {
            if i == page {
                Span::styled("● ", Style::new().fg(t.acc))
            } else {
                Span::styled("○ ", dim)
            }
        })
        .collect();
    if inner.height > 0 {
        let row = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        frame.render_widget(Paragraph::new(Line::from(dots)), row);
    }
}

/// The practice's ask, in a bar along the bottom of `area`: which key to
/// press now and what it does. The card stays in view above it.
pub fn render_coach(frame: &mut Frame, area: Rect, step: usize, t: &Theme) {
    let Some(s) = STEPS.get(step) else {
        return;
    };
    let h = 3.min(area.height);
    let bar = Rect::new(area.x, area.bottom() - h, area.width, h);
    frame.render_widget(Clear, bar);
    let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(t.dim);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.hi))
        .title(Span::styled(
            format!(" Tour · try it · {} of {} ", step + 1, STEPS.len()),
            Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(vec![
                Span::styled(" Esc", key),
                Span::styled(" leave the tour ", dim),
            ])
            .right_aligned(),
        )
        .padding(Padding::horizontal(1))
        .style(Style::new().bg(t.panel).fg(t.fg));
    let line = Line::from(vec![
        Span::raw("Press "),
        Span::styled(
            format!(" {} ", s.key),
            Style::new().fg(t.bg).bg(t.hi).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {}", s.what)),
    ]);
    let inner = block.inner(bar);
    frame.render_widget(block, bar);
    frame.render_widget(Paragraph::new(line), inner);
}
