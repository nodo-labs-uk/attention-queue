//! The review board card: heading, file tree and diff.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{card_header, confirm, diffview, tree};
use crate::app::App;
use crate::git::DiffLine;
use crate::highlight::Highlighter;
use crate::model::Card;
use crate::pr::PrInfo;
use crate::pr::app::PrReview;
use crate::pr::place::{Placement, placement};
use crate::review::{ReviewCard, lines};
use crate::theme::Theme;
use crate::visible::visible_code;

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
    let pr = app.pr_reviews.get(&card.checkout.0);
    if let Some(pr) = pr {
        head.insert(0, pr_line(&pr.info, t));
    }
    head.push(stats(rc, t));
    // A blank row sets the summary apart from the diff.
    head.push(Line::raw(""));
    let [head_area, body] =
        Layout::vertical([Constraint::Length(head.len() as u16), Constraint::Fill(1)]).areas(area);
    frame.render_widget(Paragraph::new(head), head_area.inner(Margin::new(1, 0)));
    let (tree_area, diff_area) = tree::split(body, app.config.hide_files);
    tree::render(frame, tree_area, rc, t, app.config.hide_files);
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
        diffview::render(frame, diff_area, rc, &tags(pr, rc), t, hl);
    }
    if let Some(pr) = pr.filter(|_| app.closing_pr == Some(card.id)) {
        let folder = if pr.made {
            "Its worktree is removed if it is clean and no session runs in it."
        } else {
            "Its folder is kept."
        };
        confirm::render(
            frame,
            area,
            "Close PR review",
            &[
                format!(
                    "Close the review of PR #{}? Its comments go.",
                    pr.info.number
                ),
                folder.into(),
            ],
            &[("y", "yes"), ("n", "no")],
            t,
        );
    }
}

/// "PR #5 · Fix the parser · by alice", and for someone else's PR where
/// the comments go.
fn pr_line(info: &PrInfo, t: &Theme) -> Line<'static> {
    let dim = Style::new().fg(t.dim);
    let mut spans = vec![
        Span::styled(
            format!("PR #{}", info.number),
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", dim),
        Span::styled(visible_code(&info.title), Style::new().fg(t.fg)),
        Span::styled(format!(" · by {}", visible_code(&info.author)), dim),
    ];
    if !info.mine {
        spans.push(Span::styled(
            "  (not yours: comments go to GitHub)",
            Style::new().fg(t.hi),
        ));
    }
    Line::from(spans)
}

/// On someone else's PR, this round's comments that GitHub cannot take on
/// their lines: they go in the review's main text with their lines. `file`
/// when GitHub's diff has the file but not the lines, `top` when it has
/// not the file, or could not be read. Older rounds' comments are not
/// posted.
fn tags(pr: Option<&PrReview>, rc: &ReviewCard) -> Vec<Option<&'static str>> {
    let Some(pr) = pr.filter(|pr| !pr.info.mine) else {
        return Vec::new();
    };
    let gh = pr.gh_diff.as_ref().ok();
    rc.comments
        .iter()
        .map(|c| match placement(gh, c) {
            _ if c.round != rc.round => None,
            Placement::Line => None,
            Placement::File => Some("file"),
            Placement::Body => Some("top"),
        })
        .collect()
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
    let comments = rc.current_comments().count();
    // Comments waiting to be sent are what this line is most read for.
    let comment_style = if comments > 0 {
        Style::new().fg(t.hi)
    } else {
        dim
    };
    let mut spans = vec![
        Span::styled(plural(rc.files.len(), "file"), dim),
        Span::styled(" · ", dim),
        Span::styled(format!("+{adds}"), Style::new().fg(t.add)),
        Span::styled(format!(" −{dels}"), Style::new().fg(t.del)),
        Span::styled(" · ", dim),
        Span::styled(plural(comments, "comment"), comment_style),
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
    use crate::pr::PrInfo;
    use crate::pr::app::PrReview;
    use crate::pr::place::parse_gh_diff;
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

    #[test]
    fn a_review_card_draws_in_a_pane_a_few_rows_tall() {
        for view in [DiffView::Unified, DiffView::Split] {
            for h in 1..=4 {
                let mut rc = fixture_card(1);
                rc.view = view;
                render(&review_app(rc), 40, h);
            }
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
        // A mark per file stays: the file on screen, then one left.
        let marks = |screen: &str| -> String {
            screen.chars().filter(|c| "✓●○".contains(*c)).collect()
        };
        assert_eq!(marks(&screen), "●○", "{screen}");
        a.update(Msg::Key(
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('d')),
            std::time::Instant::now(),
        ));
        assert_eq!(marks(&render(&a, 100, 20)), "✓●");
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

    /// GitHub's diff of the PR shows new lines 38-41 of src/limit.rs only.
    const GH_DIFF: &str = "\
diff --git a/src/limit.rs b/src/limit.rs
--- a/src/limit.rs
+++ b/src/limit.rs
@@ -38,3 +38,4 @@
 pub fn check(store: &Store, key: &str)
     -> Result<(), LimitError> {
-    // TODO: real limits
+    let b = store.get(key).unwrap_or_default();
+    if b.tokens == 0 { return Err(TooMany) }
";

    /// The comments card, as PR 5 of o/api by alice or (`mine`) the user.
    fn pr_app(mine: bool, gh: Option<&str>) -> App {
        let mut a = review_app(fixture_with_comments(DiffView::Unified));
        let info = PrInfo {
            repo: "o/api".into(),
            number: 5,
            title: "Fix the parser".into(),
            body: "Body".into(),
            author: if mine { "smnai" } else { "alice" }.into(),
            head_ref: "fix".into(),
            base_ref: "dev".into(),
            head_sha: "abc".into(),
            url: "https://github.com/o/api/pull/5".into(),
            mine,
        };
        a.pr_reviews.insert(
            "/w/api".into(),
            PrReview {
                info,
                main: "/m".into(),
                made: !mine,
                base_ref: "origin/dev".into(),
                gh_diff: gh.map(parse_gh_diff).ok_or_else(|| "HTTP 502".to_string()),
            },
        );
        a
    }

    #[test]
    fn a_pr_card_names_the_pr() {
        let screen = render(&pr_app(false, None), 100, 30);
        assert!(
            screen
                .contains("PR #5 · Fix the parser · by alice  (not yours: comments go to GitHub)"),
            "{screen}"
        );
        let screen = render(&pr_app(true, None), 100, 30);
        assert!(
            screen.contains("PR #5 · Fix the parser · by smnai"),
            "{screen}"
        );
        assert!(!screen.contains("not yours"), "{screen}");
    }

    #[test]
    fn a_comment_outside_githubs_diff_is_tagged_file() {
        // Last round's comment is not posted, so it has no tag.
        let screen = render(&pr_app(false, Some(GH_DIFF)), 100, 30);
        assert!(screen.contains("old ✎ 43 closing brace moved"), "{screen}");
        let mut a = pr_app(false, Some(GH_DIFF));
        if let Some(c) = a.queue.current_mut()
            && let CardKind::Review(rc) = &mut c.kind
        {
            rc.comments[1].round = rc.round;
        }
        let screen = render(&a, 100, 30);
        let row = |text: &str| {
            screen
                .lines()
                .find(|l| l.contains(text))
                .unwrap_or_else(|| panic!("{text} in\n{screen}"))
                .to_string()
        };
        assert!(
            row("closing brace moved").contains("you ✎ file closing"),
            "{screen}"
        );
        assert!(!row("what if the store").contains("file"), "{screen}");
        // A file GitHub's diff does not have goes in the review's main text.
        let other = GH_DIFF.replace("src/limit.rs", "src/other.rs");
        let screen = render(&pr_app(false, Some(&other)), 100, 30);
        assert!(screen.contains("you ✎ top what if"), "{screen}");
        // Without GitHub's diff every comment goes in the main text.
        let screen = render(&pr_app(false, None), 100, 30);
        assert!(screen.contains("you ✎ top what if"), "{screen}");
        // No tag on the user's own PR.
        let screen = render(&pr_app(true, Some(GH_DIFF)), 100, 30);
        assert!(!screen.contains("✎ file"), "{screen}");
        assert!(!screen.contains("✎ top"), "{screen}");
    }

    #[test]
    fn a_session_card_has_no_pr_line_or_tags() {
        let screen = render(
            &review_app(fixture_with_comments(DiffView::Unified)),
            100,
            30,
        );
        assert!(!screen.contains("PR #"), "{screen}");
        assert!(screen.contains("old ✎ 43 closing brace moved"), "{screen}");
    }

    #[test]
    fn pr_card_not_mine() {
        insta::assert_snapshot!(render(&pr_app(false, Some(GH_DIFF)), 100, 30));
    }

    #[test]
    fn pr_card_mine() {
        insta::assert_snapshot!(render(&pr_app(true, Some(GH_DIFF)), 100, 30));
    }
}
