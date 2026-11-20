//! `R`: the PR finder.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::input::{self, TextInput};
use super::window::{self, Height};
use super::workspace::{list_box, shown};
use crate::app::App;
use crate::pr::app::PrFinder;
use crate::pr::place::Event;
use crate::pr::{PrSummary, parse_pr_ref};
use crate::theme::Theme;
use crate::visible::{cols, wrap_words};

/// The keys along the bottom of the finder and in the keys row.
pub const KEYS: [(&str, &str); 2] = [("Enter", "open"), ("↑/↓", "choose")];

/// The typed text on the first row, a note on the second (loading, opening,
/// an error), then the PRs. The box keeps one size throughout.
pub fn render(frame: &mut Frame, area: Rect, f: &PrFinder, t: &Theme) {
    let panel = window::place(area, Height::Tall);
    // Inside the window's border and padding.
    let (width, room) = (
        panel.width.saturating_sub(window::INSET.0) as usize,
        panel.height.saturating_sub(window::INSET.1) as usize,
    );
    let dim = Style::new().fg(t.dim);
    let bad = Style::new().fg(t.hi);
    let typed = f.filter.text();
    let mut rows = vec![if typed.is_empty() {
        Line::from(vec![
            Span::styled("› ", Style::new().fg(t.acc)),
            Span::styled("Type to filter, or paste a PR link", dim),
        ])
    } else {
        Line::from(vec![
            Span::styled("› ", Style::new().fg(t.acc)),
            Span::raw(shown(&typed)),
        ])
    }];
    let list = f.shown();
    let note: Vec<Line> = match (&f.opening, &f.error, &f.prs) {
        (Some((_, n)), ..) => vec![Line::styled(format!("Opening PR #{n}…"), dim)],
        (_, Some(e), _) | (_, _, Some(Err(e))) => note(e, width, bad),
        (_, _, None) => vec![Line::styled("Listing open PRs…", dim)],
        (_, _, Some(Ok(all))) if !all.failed.is_empty() => note(&all.failed.join("; "), width, dim),
        (_, _, Some(Ok(all))) if all.prs.is_empty() => vec![Line::styled("No open PRs", dim)],
        _ if list.is_empty() => match parse_pr_ref(&typed) {
            Some((_, n)) => vec![Line::styled(format!("Enter opens PR #{n}"), dim)],
            None => vec![Line::styled("No PR matches", dim)],
        },
        _ => vec![Line::raw("")],
    };
    rows.extend(note);
    // Scrolled here, so the typed text and the note stay in view.
    let left = room.saturating_sub(rows.len());
    let top = (f.cursor + 1).saturating_sub(left);
    let picked = Style::new().fg(t.bg).bg(t.acc).add_modifier(Modifier::BOLD);
    rows.extend(
        list.iter()
            .enumerate()
            .skip(top)
            .take(left)
            .map(|(i, p)| pr_row(p, i == f.cursor, picked, t)),
    );
    list_box(frame, area, "Review a PR", rows, &KEYS, t);
    if f.opening.is_none() && room > 0 {
        let at = typed.chars().map(cols).sum::<usize>() + 2;
        let x = (panel.x + 2).saturating_add(at.min(width) as u16);
        frame.set_cursor_position(Position::new(x, panel.y + 1));
    }
}

/// The summary box for posting card `card`'s review as `event`.
pub fn render_submit(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    card: u64,
    event: Event,
    input: &TextInput,
    t: &Theme,
) {
    let pr = app
        .queue
        .iter()
        .find(|c| c.id == card)
        .and_then(|c| app.pr_reviews.get(&c.checkout.0));
    let number = pr.map_or(0, |pr| pr.info.number);
    let title = match event {
        Event::Comment => format!("Comment on PR #{number}"),
        Event::Approve => format!("Approve PR #{number}"),
        Event::RequestChanges => format!("Request changes on PR #{number}"),
    };
    let ghost = input.text().is_empty().then_some("Summary, if any");
    input::render_with_ghost(
        frame,
        area,
        input,
        &title,
        "post",
        &[],
        ghost,
        t,
        Height::Short,
    );
    // Under the summary: a warning when the PR moved under the comments,
    // how the comments will be placed, and a note when GitHub's diff could
    // not be read or GitHub would refuse the review.
    let rows = input::below_first_row(area, Height::Short);
    if rows.height == 0 {
        return;
    }
    let dim = Style::new().fg(t.dim);
    let [on_lines, in_text] = app.pr_placements(card).unwrap_or_default();
    let total = on_lines + in_text;
    let bad = Style::new().fg(t.hi);
    let mut lines = vec![Line::raw("")];
    if app.pr_cards.moved.contains(&card) {
        lines.push(Line::styled(crate::pr::app::moved_note(number), bad));
    }
    lines.push(Line::styled(placed(total, on_lines, in_text), dim));
    if total > 0 && pr.is_some_and(|pr| pr.gh_diff.is_err()) {
        lines.push(Line::styled(
            "GitHub's diff could not be read: all go in the main text",
            bad,
        ));
    }
    if event != Event::Approve && total == 0 && input.text().trim().is_empty() {
        lines.push(Line::styled("GitHub needs a comment or a summary", bad));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rows);
}

/// `3 comments: 2 on lines, 1 in the main text`
fn placed(total: usize, on_lines: usize, in_text: usize) -> String {
    let n = |k: usize, one: &str, many: &str| match k {
        1 => format!("1 {one}"),
        k => format!("{k} {many}"),
    };
    let head = match total {
        0 => return "No comments".into(),
        k => n(k, "comment", "comments"),
    };
    let mut parts = Vec::new();
    if on_lines > 0 {
        parts.push(n(on_lines, "on its line", "on lines"));
    }
    if in_text > 0 {
        parts.push(format!("{in_text} in the main text"));
    }
    if parts.is_empty() {
        return head;
    }
    format!("{head}: {}", parts.join(", "))
}

/// `text` wrapped to at most three rows, so the list still shows.
fn note(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    wrap_words(&shown(text), width, &mut |l| {
        lines.push(Line::styled(l, style))
    });
    lines.truncate(3);
    lines
}

/// `● repo #n  title  · author`: the dot marks the user's own PR; a draft
/// is dimmed and says so.
fn pr_row(p: &PrSummary, on: bool, picked: Style, t: &Theme) -> Line<'static> {
    let style = match (on, p.draft) {
        (true, _) => picked,
        (false, true) => Style::new().fg(t.dim),
        (false, false) => Style::new().fg(t.fg),
    };
    let mark = if p.mine { "● " } else { "  " };
    let draft = if p.draft { "  draft" } else { "" };
    let text = format!(
        "{mark}{} #{}  {}  · {}{draft}",
        shown(&p.repo),
        p.number,
        shown(&p.title),
        shown(&p.author)
    );
    Line::styled(text, style)
}

#[cfg(test)]
mod tests {
    use crate::app::Msg;
    use crate::app::tests::{agent, app, key};
    use crate::pr::{PrListing, PrSummary};
    use crate::ui::tests::render;

    fn pr(repo: &str, number: u64, title: &str, author: &str, draft: bool) -> PrSummary {
        PrSummary {
            repo: repo.into(),
            number,
            title: title.into(),
            author: author.into(),
            head: "h".into(),
            draft,
            mine: author == "smnai",
            updated: "2026-11-18".into(),
        }
    }

    fn finder() -> crate::app::App {
        finder_with(Vec::new())
    }

    /// The finder listed, with `failed` repos noted.
    fn finder_with(failed: Vec<String>) -> crate::app::App {
        let mut a = app();
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        a.update(key('R'));
        let prs = vec![
            pr(
                "nodo/api",
                41,
                "Fix the parser for nested lists",
                "alice",
                false,
            ),
            pr("nodo/api", 39, "Add a retry to the fetcher", "smnai", false),
            pr("nodo/web", 7, "Try a new layout", "bob", true),
        ];
        a.update(Msg::PrList(Ok(PrListing {
            prs,
            repos: vec!["nodo/api".into(), "nodo/web".into()],
            failed,
        })));
        a
    }

    #[test]
    fn the_finder_lists_prs_with_marks() {
        insta::assert_snapshot!(render(&finder(), 100, 24));
    }

    #[test]
    fn the_finder_keeps_rows_and_keys_when_narrow() {
        let text = render(&finder(), 40, 24);
        assert!(text.contains("nodo/api #41"), "{text}");
        assert!(text.contains("● nodo/api #39"), "{text}");
        assert!(text.contains("Enter"), "{text}");
        assert!(text.contains("Ctrl-Q close"), "{text}");
    }

    fn corners(s: &str) -> Vec<(usize, Option<usize>)> {
        s.lines()
            .enumerate()
            .filter(|(_, l)| l.contains('╭') || l.contains('╰'))
            .map(|(i, l)| (i, l.find('╭').or(l.find('╰'))))
            .collect()
    }

    #[test]
    fn a_repo_that_could_not_be_listed_is_noted_in_the_same_box() {
        let text = render(
            &finder_with(vec!["Could not list nodo/old: HTTP 404".into()]),
            100,
            24,
        );
        assert!(text.contains("Could not list nodo/old: HTTP 404"), "{text}");
        assert!(text.contains("nodo/api #41"), "{text}");
        assert_eq!(corners(&text), corners(&render(&finder(), 100, 24)));
    }

    #[test]
    fn the_finder_keeps_its_size_while_loading_and_on_an_error() {
        let mut a = app();
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        a.update(key('R'));
        let loading = render(&a, 100, 24);
        assert!(loading.contains("Listing open PRs…"), "{loading}");
        a.update(Msg::PrList(Err("Could not list PRs: gh auth login".into())));
        let failed = render(&a, 100, 24);
        assert!(failed.contains("gh auth login"), "{failed}");
        assert_eq!(corners(&loading), corners(&failed));
    }

    /// Alice's PR 5 as a card, GitHub's diff showing new lines 38-41 of
    /// src/limit.rs only, with comments on a line in it, a line outside
    /// it, and a file that is not in it.
    fn their_pr() -> crate::app::App {
        use crate::app::Effect;
        use crate::app::tests::{computed, fixture_files};
        use crate::pr::PrInfo;
        use crate::pr::checkout::PrCheckout;
        use crate::review::{Comment, Side};
        let mut a = app();
        let info = PrInfo {
            repo: "o/api".into(),
            number: 5,
            title: "Fix the parser".into(),
            body: "Body".into(),
            author: "alice".into(),
            head_ref: "fix".into(),
            base_ref: "dev".into(),
            head_sha: "abc".into(),
            url: "https://github.com/o/api/pull/5".into(),
            mine: false,
        };
        let checkout = PrCheckout {
            dir: "/w".into(),
            main: "/m".into(),
            made: true,
            base_ref: "origin/dev".into(),
        };
        let gh = "diff --git a/src/limit.rs b/src/limit.rs\n--- a/src/limit.rs\n\
                  +++ b/src/limit.rs\n@@ -38,3 +38,4 @@\n a\n b\n-c\n+d\n+e\n";
        let effects = a.update(Msg::PrOpened {
            asked: (Some("o/api".into()), 5),
            result: Ok((info, checkout, Ok(crate::pr::place::parse_gh_diff(gh)))),
        });
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        a.update(computed(req.clone(), "base", "t1", fixture_files()));
        let comment = |path: &str, line| Comment {
            path: path.into(),
            line,
            end_line: line,
            side: Side::New,
            body: "b".into(),
            round: 1,
        };
        let card = a.queue.current_mut().unwrap();
        let crate::model::CardKind::Review(rc) = &mut card.kind else {
            panic!("not a review card");
        };
        rc.comments = vec![
            comment("src/limit.rs", 40),
            comment("src/limit.rs", 41),
            comment("src/limit.rs", 43),
            comment("src/http.rs", 3),
        ];
        a
    }

    #[test]
    fn the_summary_box_counts_where_the_comments_go() {
        let mut a = their_pr();
        // A refresh moved the PR's head under the comments.
        let id = a.queue.current().unwrap().id;
        a.pr_cards.moved.insert(id);
        a.update(key('X'));
        insta::assert_snapshot!(render(&a, 100, 24));
    }

    #[test]
    fn without_githubs_diff_the_summary_box_says_all_go_in_the_main_text() {
        let mut a = their_pr();
        let dir = a.queue.current().unwrap().checkout.0.clone();
        a.pr_reviews.get_mut(&dir).unwrap().gh_diff = Err("HTTP 406".into());
        a.update(key('s'));
        let screen = render(&a, 100, 24);
        assert!(
            screen.contains("4 comments: 4 in the main text"),
            "{screen}"
        );
        assert!(
            screen.contains("GitHub's diff could not be read: all go in the main text"),
            "{screen}"
        );
    }

    #[test]
    fn r_refreshes_someone_elses_pr() {
        let mut a = their_pr();
        a.config.show_keys = true;
        let screen = render(&a, 200, 40);
        assert!(screen.contains("r  refresh"), "{screen}");
        assert!(!screen.contains("whole branch"), "{screen}");
        let dir = a.queue.current().unwrap().checkout.0.clone();
        a.pr_reviews.get_mut(&dir).unwrap().info.mine = true;
        let screen = render(&a, 200, 40);
        assert!(screen.contains("r  whole branch"), "{screen}");
    }

    #[test]
    fn the_summary_box_says_when_github_would_refuse() {
        let mut a = their_pr();
        if let Some(c) = a.queue.current_mut()
            && let crate::model::CardKind::Review(rc) = &mut c.kind
        {
            rc.comments.clear();
        }
        a.update(key('X'));
        let screen = render(&a, 100, 24);
        assert!(screen.contains("Request changes on PR #5"), "{screen}");
        assert!(screen.contains("No comments"), "{screen}");
        assert!(
            screen.contains("GitHub needs a comment or a summary"),
            "{screen}"
        );
        crate::app::tests::typed(&mut a, "Tests please");
        let screen = render(&a, 100, 24);
        assert!(!screen.contains("GitHub needs"), "{screen}");
        a.update(Msg::Key(
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Esc),
            std::time::Instant::now(),
        ));
        a.update(key('a'));
        let screen = render(&a, 100, 24);
        assert!(screen.contains("Approve PR #5"), "{screen}");
        assert!(!screen.contains("GitHub needs"), "{screen}");
    }
}
