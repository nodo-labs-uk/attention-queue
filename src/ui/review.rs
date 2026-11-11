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
    let (tree_area, diff_area) = tree::split(body, app.config.hide_files);
    if let Some(tree_area) = tree_area {
        tree::render(frame, tree_area, rc, t);
    }
    if rc.all_done() {
        let key = Style::new().fg(t.key).add_modifier(Modifier::BOLD);
        let dim = Style::new().fg(t.dim);
        let note = vec![
            Line::styled(
                "Every file is marked done.",
                Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
            ),
            Line::from(vec![
                Span::styled("a", key),
                Span::styled(" approves, ", dim),
                Span::styled("s", key),
                Span::styled(" sends the comments, ", dim),
                Span::styled("u", key),
                Span::styled(" shows the files again.", dim),
            ]),
        ];
        frame.render_widget(Paragraph::new(note), diff_area.inner(Margin::new(2, 1)));
    } else {
        diffview::render(frame, diff_area, rc, t, hl);
    }
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
    let done = (0..rc.files.len()).filter(|&i| rc.is_done(i)).count();
    if done > 0 {
        spans.push(Span::styled(format!(" · {done} done"), dim));
    }
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

    use crate::app::tests::agent;
    use crate::app::{App, Msg};
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

    #[test]
    fn f_hides_the_file_list_and_brings_it_back() {
        let mut a = review_app(fixture_card(2));
        assert!(render(&a, 100, 20).contains("http.rs"));
        let f = || {
            Msg::Key(
                crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('f')),
                std::time::Instant::now(),
            )
        };
        assert_eq!(a.update(f()), vec![crate::app::Effect::SaveConfig]);
        assert!(a.config.hide_files, "kept in the config across restarts");
        let screen = render(&a, 100, 20);
        assert!(!screen.contains("http.rs"), "{screen}");
        assert!(screen.contains("src/limit.rs · before"), "{screen}");
        a.update(f());
        assert!(render(&a, 100, 20).contains("http.rs"));
    }

    #[test]
    fn e_opens_the_file_at_the_cursor_in_an_editor() {
        let mut a = review_app(fixture_card(2));
        let e = Msg::Key(
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('e')),
            std::time::Instant::now(),
        );
        // The cursor starts on the first change, new line 40.
        let effects = a.update(e);
        assert!(
            matches!(&effects[..], [crate::app::Effect::Edit { path, line: 40, .. }]
                if path == std::path::Path::new("src/limit.rs")),
            "{effects:?}"
        );
    }

    /// A Markdown file is shown as read: no heading hashes, bold marks or
    /// code ticks, and lines inside a code fence left alone. `m` shows it as
    /// written.
    #[test]
    fn markdown_reads_rendered_and_m_shows_the_source() {
        let text = [
            "# Plan",
            "Keep **one** bucket per `key`.",
            "- first",
            "```sh",
            "# not a heading",
            "```",
        ];
        let lines = text
            .iter()
            .enumerate()
            .map(|(i, t)| DiffLine::Add {
                new: i as u32 + 1,
                text: (*t).into(),
            })
            .collect();
        let file = FileDiff {
            old_path: None,
            new_path: Some("docs/plan.md".into()),
            status: FileStatus::Added,
            binary: false,
            hunks: vec![Hunk {
                old_start: 0,
                old_len: 0,
                new_start: 1,
                new_len: text.len() as u32,
                lines,
            }],
        };
        let mut rc = ReviewCard::new(1, "a".into(), "b".into(), vec![file], Vec::new(), false);
        rc.view = DiffView::Unified;
        let screen = render(&review_app(rc.clone()), 100, 20);
        for want in [
            "+Plan",
            "Keep one bucket per key.",
            "• first",
            "# not a heading",
        ] {
            assert!(screen.contains(want), "{want} in\n{screen}");
        }
        assert!(!screen.contains("# Plan"), "{screen}");
        rc.markdown_source = true;
        let screen = render(&review_app(rc), 100, 20);
        assert!(
            screen.contains("Keep **one** bucket per `key`."),
            "{screen}"
        );
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
