//! The review board card: heading, file tree and diff.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{card_header, diffview, tree};
use crate::app::App;
use crate::git::DiffLine;
use crate::highlight::Highlighter;
use crate::model::Card;
use crate::review::{ReviewCard, lines};
use crate::theme::Theme;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    card: &Card,
    rc: &ReviewCard,
    hl: &mut Highlighter,
) {
    let t = app.theme();
    let mut head = card_header(app, card);
    head[0].spans.push(Span::styled(
        format!("  round {}", rc.round),
        Style::new().fg(t.hi).add_modifier(Modifier::BOLD),
    ));
    head.push(stats(rc, t));
    let [head_area, body] =
        Layout::vertical([Constraint::Length(head.len() as u16), Constraint::Fill(1)]).areas(area);
    frame.render_widget(Paragraph::new(head), head_area.inner(Margin::new(1, 0)));
    let tree_w = (body.width / 4).clamp(12, 30);
    let [tree_area, diff_area] =
        Layout::horizontal([Constraint::Length(tree_w), Constraint::Fill(1)]).areas(body);
    tree::render(frame, tree_area, rc, t);
    diffview::render(frame, diff_area, rc, t, hl);
}

/// "3 files · +12 −4 · 1 comment", and which changes are shown.
fn stats(rc: &ReviewCard, t: &Theme) -> Line<'static> {
    let (mut adds, mut dels) = (0, 0);
    for l in rc.files.iter().flat_map(lines) {
        match l {
            DiffLine::Add { .. } => adds += 1,
            DiffLine::Del { .. } => dels += 1,
            DiffLine::Context { .. } => {}
        }
    }
    let plural = |n: usize, word: &str| {
        if n == 1 {
            format!("{n} {word}")
        } else {
            format!("{n} {word}s")
        }
    };
    let dim = Style::new().fg(t.dim);
    let mut spans = vec![
        Span::styled(plural(rc.files.len(), "file"), dim),
        Span::styled(" · ", dim),
        Span::styled(format!("+{adds}"), Style::new().fg(t.add)),
        Span::styled(format!(" −{dels}"), Style::new().fg(t.del)),
        Span::styled(" · ", dim),
        Span::styled(plural(rc.current_comments().count(), "comment"), dim),
    ];
    if rc.round > 1 {
        let scope = if rc.show_full_branch {
            "whole branch".to_string()
        } else {
            format!("changes since round {}", rc.round - 1)
        };
        spans.push(Span::styled(format!(" · {scope}"), dim));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::App;
    use crate::app::tests::agent;
    use crate::git::{DiffLine, FileDiff, FileStatus, Hunk};
    use crate::highlight::Highlighter;
    use crate::model::CardKind;
    use crate::queue::tests::card;
    use crate::review::tests::{comment, fixture_card};
    use crate::review::{DiffView, ReviewCard};
    use crate::ui::tests::{app, render};

    fn review_app(rc: ReviewCard) -> App {
        let mut a = app();
        a.agents = vec![agent("%1", 1, "api", "/w/api", true)];
        let mut c = card(1, "/w/api", CardKind::Review(rc), 0);
        c.agent = a.agents[0].id.clone();
        a.queue.push(c);
        a
    }

    #[test]
    fn hidden_characters_in_a_diff_line_and_comment_are_spelled_out() {
        for view in [DiffView::Unified, DiffView::Split] {
            let mut rc = fixture_card(1);
            rc.view = view;
            for l in rc.files[0].hunks[0].lines.iter_mut() {
                if let DiffLine::Add { text, .. } = l {
                    *text = format!("let ok = 1; // \u{202e}esrever\u{202c}{text}");
                    break;
                }
            }
            rc.comments = vec![comment("src/limit.rs", 40, 40, "see\u{200b}this", 1)];
            let screen = render(&review_app(rc), 160, 30);
            assert!(screen.contains("\\u{202e}esrever\\u{202c}"), "{screen}");
            assert!(screen.contains("see\\u{200b}this"), "{screen}");
            assert!(!screen.contains('\u{202e}'));
        }
    }

    fn fixture_with_comments(view: DiffView) -> ReviewCard {
        let mut rc = fixture_card(2);
        rc.view = view;
        rc.comments = vec![
            comment(
                "src/limit.rs",
                40,
                40,
                "what if the store is down?\nThis hides it.",
                2,
            ),
            comment("src/limit.rs", 43, 43, "closing brace moved", 1),
        ];
        rc
    }

    #[test]
    fn split_view() {
        let a = review_app(fixture_with_comments(DiffView::Split));
        insta::assert_snapshot!(render(&a, 80, 30));
    }

    #[test]
    fn unified_view() {
        let a = review_app(fixture_with_comments(DiffView::Unified));
        insta::assert_snapshot!(render(&a, 80, 30));
    }

    /// One file of 5,000 diff lines: runs of context, deletions and additions.
    fn big_card() -> ReviewCard {
        let mut lines = Vec::new();
        let (mut old, mut new) = (1, 1);
        for i in 0..5000u32 {
            let text = format!("    let value_{i} = compute({i}, \"text\"); // note {i}");
            lines.push(match i % 10 {
                0..=5 => {
                    let l = DiffLine::Context { old, new, text };
                    old += 1;
                    new += 1;
                    l
                }
                6 | 7 => {
                    old += 1;
                    DiffLine::Del { old: old - 1, text }
                }
                _ => {
                    new += 1;
                    DiffLine::Add { new: new - 1, text }
                }
            });
        }
        let file = FileDiff {
            old_path: Some("src/big.rs".into()),
            new_path: Some("src/big.rs".into()),
            status: FileStatus::Modified,
            binary: false,
            hunks: vec![Hunk {
                old_start: 1,
                old_len: old - 1,
                new_start: 1,
                new_len: new - 1,
                lines,
            }],
        };
        ReviewCard::new(1, "a".into(), "b".into(), vec![file], Vec::new(), false)
    }

    #[test]
    #[ignore = "timing; run in release: cargo test --release -- --ignored bench_render_5000_lines"]
    fn bench_render_5000_lines() {
        for view in [DiffView::Split, DiffView::Unified] {
            let mut rc = big_card();
            rc.view = view;
            let mut a = review_app(rc);
            let mut hl = Highlighter::new(a.theme());
            let mut term = Terminal::new(TestBackend::new(200, 60)).unwrap();
            let frames = 200;
            let mut worst = 0.0f64;
            let start = Instant::now();
            for i in 0..frames {
                // Scroll a page per frame, so most frames highlight unseen lines.
                if let Some(c) = a.queue.current_mut()
                    && let CardKind::Review(rc) = &mut c.kind
                {
                    rc.cursor.line = (i * 50) % 5000;
                }
                let t = Instant::now();
                term.draw(|f| crate::ui::draw(f, &a, &mut hl)).unwrap();
                worst = worst.max(t.elapsed().as_secs_f64() * 1000.0);
            }
            let mean = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
            println!("{view:?}: mean {mean:.2} ms, worst {worst:.2} ms per frame");
            assert!(mean < 5.0, "{view:?} mean frame {mean:.2} ms");
        }
    }
}
