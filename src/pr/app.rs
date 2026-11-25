//! The PR finder (`R`) and the PR review cards, as `App` sees them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::checkout::PrCheckout;
use super::place::{Event, Placement, placement, review_body};
use super::{GhDiffRead, PrInfo, PrListing, PrSummary, parse_pr_ref};
use crate::app::{App, Effect, Overlay, ReviewRequest};
use crate::model::{AgentId, CardKind, Checkout};
use crate::review;
use crate::ui::input::{InputEvent, TextInput};

/// A PR being reviewed, by the folder it is checked out in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrReview {
    pub info: PrInfo,
    /// The main clone the folder belongs to.
    pub main: PathBuf,
    /// The folder is a worktree that aq made: it is in aq's PR folder.
    pub made: bool,
    /// `origin/<baseRefName>`: the first round diffs from its merge base.
    pub base_ref: String,
    /// The lines GitHub's diff shows, or why `gh pr diff` could not read it.
    pub gh_diff: GhDiffRead,
}

/// Posts that failed in a way that may have made the review all the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uncertain {
    /// When the first such attempt started, in seconds since 1970.
    pub started: u64,
    /// Every head commit such an attempt was made on.
    pub heads: Vec<String>,
}

/// What is under way on PR cards, by card id.
#[derive(Debug, Default)]
pub struct PrCards {
    /// Reviews being posted, with how many comments each carries.
    pub posting: BTreeMap<u64, usize>,
    /// PRs being checked out again.
    pub refreshing: BTreeSet<u64>,
    /// Cards whose diff could not be made after a refresh: they show an
    /// older diff than the head a review would be posted on.
    pub stale: BTreeSet<u64>,
    /// Posts that may have made the review all the same.
    pub uncertain: BTreeMap<u64, Uncertain>,
    /// PRs that got new commits after this round's comments were written.
    pub moved: BTreeSet<u64>,
    /// The user's own PR cards being approved: the PR review ends when
    /// the card closes.
    pub approved: BTreeSet<u64>,
}

impl PrCards {
    fn forget(&mut self, id: u64) {
        self.posting.remove(&id);
        self.refreshing.remove(&id);
        self.stale.remove(&id);
        self.uncertain.remove(&id);
        self.moved.remove(&id);
        self.approved.remove(&id);
    }
}

/// True when `dir` is a folder of aq's own: one in `<state>/pr`.
fn in_pr_folder(dir: &Path) -> bool {
    let ours = crate::config::paths().state_dir.join("pr");
    dir.file_name().is_some() && dir.parent() == Some(ours.as_path())
}

/// What the status line says when GitHub's diff of PR `number` could not
/// be read.
pub fn diff_note(number: u64, error: &str) -> String {
    if error.contains("HTTP 406") {
        "GitHub will not show this diff: all comments will go in the review text".into()
    } else {
        format!("GitHub's diff for PR #{number} could not be read: {error}")
    }
}

/// What the status line and the summary box say when a refresh moved the
/// PR's head under this round's comments.
pub fn moved_note(number: u64) -> String {
    format!("PR #{number} has new commits since you wrote these comments: check where they sit")
}

/// `R`: the open PRs of the known repos, filtered by what is typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFinder {
    /// `None` while the list loads.
    pub prs: Option<Result<PrListing, String>>,
    pub filter: TextInput,
    /// Index into `shown`.
    pub cursor: usize,
    /// The PR being opened: the repo asked for, if any, and its number.
    pub opening: Option<(Option<String>, u64)>,
    /// Why the last open failed.
    pub error: Option<String>,
}

impl PrFinder {
    fn new() -> Self {
        PrFinder {
            prs: None,
            filter: TextInput::single_line(),
            cursor: 0,
            opening: None,
            error: None,
        }
    }

    /// The PRs whose repo, number, title or author hold the typed text.
    pub fn shown(&self) -> Vec<&PrSummary> {
        let needle = self.filter.text().trim().to_lowercase();
        let Some(Ok(listing)) = &self.prs else {
            return Vec::new();
        };
        listing
            .prs
            .iter()
            .filter(|p| {
                let hay = format!("{} #{} {} {}", p.repo, p.number, p.title, p.author);
                hay.to_lowercase().contains(&needle)
            })
            .collect()
    }

    pub fn picked(&self) -> Option<&PrSummary> {
        self.shown().get(self.cursor).copied()
    }

    /// The repos the list named, once it is in.
    fn repos(&self) -> &[String] {
        match &self.prs {
            Some(Ok(listing)) => &listing.repos,
            _ => &[],
        }
    }
}

impl App {
    /// The checkouts of the known sessions, each once.
    fn known_checkouts(&self) -> Vec<PathBuf> {
        let set: std::collections::BTreeSet<PathBuf> = self
            .agents
            .iter()
            .filter_map(|a| a.checkout.as_ref().map(|c| c.0.clone()))
            .collect();
        set.into_iter().collect()
    }

    /// The session working in `dir`, or nobody.
    fn agent_in(&self, dir: &Path) -> AgentId {
        self.agents
            .iter()
            .find(|a| a.checkout.as_ref().is_some_and(|c| c.0 == dir))
            .map_or_else(AgentId::unknown, |a| a.id.clone())
    }

    /// The base a review of `checkout` diffs from, when it is a PR's.
    pub(crate) fn pr_base(&self, checkout: &Checkout) -> Option<String> {
        self.pr_reviews
            .get(&checkout.0)
            .map(|pr| pr.base_ref.clone())
    }

    /// The review card on `dir`, if one is in the queue.
    fn review_card_in(&self, dir: &Path) -> Option<u64> {
        self.queue
            .iter()
            .find(|c| c.checkout.0 == dir && matches!(c.kind, CardKind::Review(_)))
            .map(|c| c.id)
    }

    /// `R`: opens the finder and lists the PRs of every known repo.
    pub(crate) fn open_pr_finder(&mut self) -> Vec<Effect> {
        let dirs = self.known_checkouts();
        let mut finder = PrFinder::new();
        let effects = if dirs.is_empty() {
            finder.prs = Some(Err(
                "No session is open in a git repo, so there are no PRs to list".into(),
            ));
            Vec::new()
        } else {
            vec![Effect::ListPrs { dirs }]
        };
        self.overlay = Some(Overlay::PrFinder(Box::new(finder)));
        effects
    }

    /// Typing filters; arrows (or j/k before anything is typed) move;
    /// Enter opens; Esc closes. While a PR opens, only Esc works.
    pub(crate) fn on_pr_finder_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::PrFinder(f)) = &mut self.overlay else {
            return Vec::new();
        };
        if key.code == KeyCode::Esc {
            self.overlay = None;
            return Vec::new();
        }
        if f.opening.is_some() {
            return Vec::new();
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let typed = f.filter.text();
        let last = f.shown().len().saturating_sub(1);
        match key.code {
            KeyCode::Enter => return self.open_picked_pr(),
            KeyCode::Down => f.cursor = (f.cursor + 1).min(last),
            KeyCode::Char('j') if plain && typed.is_empty() => f.cursor = (f.cursor + 1).min(last),
            KeyCode::Up => f.cursor = f.cursor.saturating_sub(1),
            KeyCode::Char('k') if plain && typed.is_empty() => {
                f.cursor = f.cursor.saturating_sub(1)
            }
            _ => {
                f.filter.handle_key(key);
                if f.filter.text() != typed {
                    f.cursor = 0;
                    f.error = None;
                }
            }
        }
        Vec::new()
    }

    /// Enter: the PR the typed number or link names, or the picked row.
    /// A typed number takes its repo from the link, else from a row with
    /// that number (the picked one first), else from the only repo.
    fn open_picked_pr(&mut self) -> Vec<Effect> {
        let known = self.known_checkouts();
        let here = self.here().map(|(dir, ..)| dir);
        let Some(Overlay::PrFinder(f)) = &mut self.overlay else {
            return Vec::new();
        };
        let picked = f.picked().map(|p| (p.repo.clone(), p.number));
        let target = match parse_pr_ref(&f.filter.text()) {
            Some((Some(repo), n)) => Some((Some(repo), n)),
            Some((None, n)) => {
                let row = picked
                    .clone()
                    .filter(|p| p.1 == n)
                    .map(|p| p.0)
                    .or_else(|| {
                        let shown = f.shown();
                        shown.iter().find(|p| p.number == n).map(|p| p.repo.clone())
                    });
                let repo = match (row, f.repos()) {
                    (Some(r), _) => Some(r),
                    (None, [only]) => Some(only.clone()),
                    // Not listed yet: a single folder names its repo itself.
                    (None, []) if known.len() <= 1 => None,
                    (None, _) => {
                        f.error = Some("aq knows several repos: paste the PR's link".into());
                        return Vec::new();
                    }
                };
                Some((repo, n))
            }
            None => picked.map(|(repo, n)| (Some(repo), n)),
        };
        let Some((repo, number)) = target else {
            return Vec::new();
        };
        let dir = match here.filter(|h| known.contains(h)) {
            Some(h) => h,
            None => match known.first() {
                Some(d) => d.clone(),
                None => {
                    f.error = Some("No session is open in a clone to check the PR out in".into());
                    return Vec::new();
                }
            },
        };
        // A PR already in the queue is shown, not opened again.
        let open = self.pr_reviews.iter().find(|(_, pr)| {
            pr.info.number == number && repo.as_ref().is_none_or(|r| *r == pr.info.repo)
        });
        if let Some(id) = open.and_then(|(d, _)| self.review_card_in(d)) {
            self.queue.show(id);
            self.overlay = None;
            return Vec::new();
        }
        let Some(Overlay::PrFinder(f)) = &mut self.overlay else {
            return Vec::new();
        };
        f.opening = Some((repo.clone(), number));
        f.error = None;
        vec![Effect::OpenPr {
            dir,
            repo,
            number,
            known,
        }]
    }

    /// The list `ListPrs` asked for, if the finder is still open.
    pub(crate) fn on_pr_list(&mut self, prs: Result<PrListing, String>) {
        if let Some(Overlay::PrFinder(f)) = &mut self.overlay {
            f.prs = Some(prs);
            f.cursor = f.cursor.min(f.shown().len().saturating_sub(1));
        }
    }

    /// A PR made ready: it becomes a review card diffed against its base.
    /// Only the result of the finder's own request closes it or shows its
    /// error there; any other error goes to the status line, and any other
    /// success still opens its card.
    pub(crate) fn on_pr_opened(
        &mut self,
        asked: (Option<String>, u64),
        result: Result<(PrInfo, PrCheckout, GhDiffRead), String>,
    ) -> Vec<Effect> {
        let ours = matches!(
            &self.overlay,
            Some(Overlay::PrFinder(f)) if f.opening.as_ref() == Some(&asked)
        );
        let (info, checkout, gh_diff) = match result {
            Ok(r) => r,
            Err(e) => {
                match &mut self.overlay {
                    Some(Overlay::PrFinder(f)) if ours => {
                        f.opening = None;
                        f.error = Some(e);
                    }
                    _ => self.set_status(e),
                }
                return Vec::new();
            }
        };
        if ours {
            self.overlay = None;
        }
        let dir = checkout.dir;
        let base_ref = checkout.base_ref;
        if let (Err(e), false) = (&gh_diff, info.mine) {
            self.set_status(diff_note(info.number, e));
        }
        self.pr_reviews.insert(
            dir.clone(),
            PrReview {
                info,
                main: checkout.main,
                // A worktree aq made can come back as a session's folder.
                made: in_pr_folder(&dir),
                base_ref: base_ref.clone(),
                gh_diff,
            },
        );
        // A card already there (a session's, in the user's own PR's folder)
        // shows the PR's whole branch from now on.
        if let Some(id) = self.review_card_in(&dir) {
            self.queue.show(id);
            let agent = self
                .queue
                .current()
                .map_or_else(AgentId::unknown, |c| c.agent.clone());
            return vec![Effect::ComputeReview(ReviewRequest {
                checkout: Checkout(dir),
                agent,
                from_tree: None,
                full_branch: true,
                card: Some(id),
                base_ref: Some(base_ref),
            })];
        }
        vec![Effect::ComputeReview(ReviewRequest {
            agent: self.agent_in(&dir),
            checkout: Checkout(dir),
            from_tree: None,
            full_branch: false,
            card: None,
            base_ref: Some(base_ref),
        })]
    }

    /// `i` and `D` on a PR card; on someone else's, `s`, `a` and `X` too,
    /// and `r` checks it out again. `None` for any other key or card.
    pub(crate) fn on_pr_card_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        let card = self.queue.current()?;
        let pr = self.pr_reviews.get(&card.checkout.0)?;
        let (id, number, mine) = (card.id, pr.info.number, pr.info.mine);
        // What is posted is what the card held at Enter.
        if self.pr_cards.posting.contains_key(&id)
            && matches!(
                key.code,
                KeyCode::Char('s' | 'a' | 'X' | 'r' | 'c' | 'x' | 'D')
            )
        {
            self.set_status(format!("Posting your review of PR #{number}…"));
            return Some(Vec::new());
        }
        // On the user's own PR, the session's message is on its way.
        if mine && self.sending.contains_key(&id) && key.code == KeyCode::Char('D') {
            self.set_status(format!(
                "Your message about PR #{number} is on its way to the session"
            ));
            return Some(Vec::new());
        }
        if self.pr_cards.refreshing.contains(&id)
            && matches!(key.code, KeyCode::Char('s' | 'a' | 'X' | 'r' | 'D'))
        {
            self.set_status(format!("Checking out PR #{number} again…"));
            return Some(Vec::new());
        }
        // The card shows an older diff than the head a review would go on.
        if !mine
            && self.pr_cards.stale.contains(&id)
            && matches!(key.code, KeyCode::Char('s' | 'a' | 'X'))
        {
            self.set_status(format!("PR #{number}'s diff could not be shown: press r"));
            return Some(Vec::new());
        }
        match key.code {
            KeyCode::Char('X') if mine => {
                self.set_status("X requests changes on someone else's PR");
            }
            KeyCode::Char('s') if !mine => self.open_pr_submit(id, Event::Comment),
            KeyCode::Char('a') if !mine => self.open_pr_submit(id, Event::Approve),
            KeyCode::Char('X') => self.open_pr_submit(id, Event::RequestChanges),
            KeyCode::Char('r') if !mine => {
                let dir = &card.checkout.0;
                if self
                    .agents
                    .iter()
                    .any(|a| a.checkout.as_ref().is_some_and(|c| &c.0 == dir))
                {
                    self.set_status(format!(
                        "A session is working in PR #{number}'s worktree, so it is not checked out again"
                    ));
                    return Some(Vec::new());
                }
                let (info, main) = (pr.info.clone(), pr.main.clone());
                self.pr_cards.refreshing.insert(id);
                self.set_status(format!("Checking out PR #{number} again…"));
                return Some(vec![Effect::RefreshPr {
                    card: id,
                    info,
                    main,
                }]);
            }
            KeyCode::Char('i') if pr.info.body.trim().is_empty() => {
                let number = pr.info.number;
                self.set_status(format!("PR #{number} has no description"));
            }
            KeyCode::Char('i') => {
                let body = pr.info.body.clone();
                self.read_fast(&body);
            }
            KeyCode::Char('D') => self.closing_pr = Some(card.id),
            _ => return None,
        }
        Some(Vec::new())
    }

    /// y or Enter closes the PR review `D` asked about; n, q or Esc keeps
    /// it. `None` when nothing is asked, or the card is no longer on screen.
    pub(crate) fn on_close_pr_key(&mut self, key: KeyEvent) -> Option<Vec<Effect>> {
        let id = self.closing_pr?;
        if self.queue.current().map(|c| c.id) != Some(id) {
            self.closing_pr = None;
            return None;
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                self.closing_pr = None;
                Some(self.close_pr(id, None))
            }
            KeyCode::Char('n' | 'q') | KeyCode::Esc => {
                self.closing_pr = None;
                Some(Vec::new())
            }
            _ => Some(Vec::new()),
        }
    }

    /// Ends the review of the PR on card `id`: the card, its rounds and
    /// saved draft go, and a worktree aq made is removed. `done` is what
    /// the status line says, if not that the review closed.
    fn close_pr(&mut self, id: u64, done: Option<String>) -> Vec<Effect> {
        self.pr_cards.forget(id);
        let Some(card) = self.queue.remove(id) else {
            return Vec::new();
        };
        let dir = card.checkout.0;
        self.rounds.remove(&dir);
        self.unclaimed.forget(&dir);
        let Some(pr) = self.pr_reviews.remove(&dir) else {
            return Vec::new();
        };
        self.drop_pr_folder(pr, dir, done)
    }

    /// After a PR review ended: removes `dir` if it is a worktree aq made
    /// and no session works in it (`checkout::remove` then keeps it if it
    /// has uncommitted changes). Any other folder is kept. `done` is what
    /// the status line says, if not that the review closed.
    fn drop_pr_folder(&mut self, pr: PrReview, dir: PathBuf, done: Option<String>) -> Vec<Effect> {
        let closed = done
            .clone()
            .unwrap_or_else(|| format!("Closed the review of PR #{}", pr.info.number));
        if !pr.made {
            self.set_status(closed);
            return Vec::new();
        }
        if self
            .agents
            .iter()
            .any(|a| a.checkout.as_ref().is_some_and(|c| c.0 == dir))
        {
            self.set_status(match done {
                Some(done) => {
                    format!("{done}; a session is working in its worktree, so it is kept")
                }
                None => "A session is working in it; the worktree is kept".into(),
            });
            return Vec::new();
        }
        self.set_status(closed);
        vec![Effect::RemovePrWorktree { main: pr.main, dir }]
    }

    /// Review card `id` in `dir` closed with `status` said. If it was the
    /// user's own PR being approved, the PR review ends: the folder's own
    /// mode counts again, and a worktree aq made goes as `D` removes it.
    pub(crate) fn on_review_closed(&mut self, id: u64, dir: &Path, status: &str) -> Vec<Effect> {
        if !self.pr_cards.approved.contains(&id) {
            return Vec::new();
        }
        self.pr_cards.forget(id);
        let Some(pr) = self.pr_reviews.remove(dir) else {
            return Vec::new();
        };
        if pr.made {
            self.rounds.remove(dir);
            self.unclaimed.forget(dir);
        }
        self.drop_pr_folder(pr, dir.to_path_buf(), Some(status.to_string()))
    }

    /// `a` on review card `id`: on the user's own PR, the PR review ends
    /// once the card closes.
    pub(crate) fn approving(&mut self, id: u64, dir: &Path) {
        if self.pr_reviews.get(dir).is_some_and(|pr| pr.info.mine) {
            self.pr_cards.approved.insert(id);
        }
    }

    /// `s` on the user's own PR card: the comments, after a line naming
    /// the PR, go to the session in its checkout. With none there, a new
    /// session starts with them, and the round closes as if sent.
    pub(crate) fn send_my_pr(&mut self, id: u64) -> Vec<Effect> {
        // Sending, not approving: the PR review goes on.
        self.pr_cards.approved.remove(&id);
        let Some(card) = self.queue.current().filter(|c| c.id == id) else {
            return Vec::new();
        };
        let (CardKind::Review(rc), Some(pr)) = (&card.kind, self.pr_reviews.get(&card.checkout.0))
        else {
            return Vec::new();
        };
        let n = rc.current_comments().count();
        if n == 0 {
            self.set_status("No comments to send; A approves");
            return Vec::new();
        }
        let number = pr.info.number;
        let text = format!(
            "This is PR #{number} ({}). Fix what the comments below ask, run the tests, \
then commit and push to the PR's branch.\n\n{}",
            pr.info.url,
            review::compose_message(rc)
        );
        let dir = card.checkout.0.clone();
        // The session that raised the card, if it is still in the checkout;
        // else any there, as one may have started since the card was made.
        let raised = self
            .agent(&card.agent)
            .is_some_and(|a| a.checkout.as_ref().is_some_and(|c| c.0 == dir));
        let agent = if raised {
            card.agent.clone()
        } else {
            self.agent_in(&dir)
        };
        let Some(name) = self.agent(&agent).map(|a| a.window_name.clone()) else {
            let mut effects = self.close_review(
                id,
                format!("Started a session on PR #{number} with your comments"),
            );
            self.unclaimed.forget(&dir);
            effects.push(Effect::NewSession {
                dir,
                prompt: Some(text),
                worktree: false,
            });
            return effects;
        };
        if let Some(card) = self.queue.current_mut() {
            card.agent = agent.clone();
        }
        self.picked = Some(agent.clone());
        let s = if n == 1 { "" } else { "s" };
        self.send_when_ready(id, agent, text, format!("Sent {n} comment{s} to {name}"))
    }

    /// The summary box for posting card `id`'s review as `event`.
    fn open_pr_submit(&mut self, id: u64, event: Event) {
        self.overlay = Some(Overlay::PrSubmit {
            card: id,
            event,
            input: TextInput::single_line(),
        });
    }

    /// How this round's comments on card `id` will be placed: on lines,
    /// and in the review's main text.
    pub fn pr_placements(&self, id: u64) -> Option<[usize; 2]> {
        let card = self.queue.iter().find(|c| c.id == id)?;
        let CardKind::Review(rc) = &card.kind else {
            return None;
        };
        let diff = self.pr_reviews.get(&card.checkout.0)?.gh_diff.as_ref().ok();
        let mut counts = [0; 2];
        for c in rc.current_comments() {
            counts[usize::from(placement(diff, c) != Placement::Line)] += 1;
        }
        Some(counts)
    }

    /// Typing the summary; Enter posts the review, Esc cancels. A request
    /// for changes or a comment needs a comment or a summary.
    pub(crate) fn on_pr_submit_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::PrSubmit { card, event, input }) = &mut self.overlay else {
            return Vec::new();
        };
        let summary = match input.handle_key(key) {
            InputEvent::Editing => return Vec::new(),
            InputEvent::Cancel => {
                self.overlay = None;
                return Vec::new();
            }
            InputEvent::Submit(text) => text.trim().to_string(),
        };
        let (id, event) = (*card, *event);
        let found = self.queue.iter().find(|c| c.id == id).and_then(|c| {
            let CardKind::Review(rc) = &c.kind else {
                return None;
            };
            let pr = self.pr_reviews.get(&c.checkout.0)?;
            Some((c, rc, pr, pr.gh_diff.as_ref().ok()))
        });
        let Some((card, rc, pr, diff)) = found else {
            self.overlay = None;
            return Vec::new();
        };
        if event != Event::Approve && summary.is_empty() && rc.current_comments().next().is_none() {
            self.set_status("GitHub needs a comment or a summary");
            return Vec::new();
        }
        let body = review_body(
            diff,
            &rc.comments,
            rc.round,
            event,
            &summary,
            &pr.info.head_sha,
        );
        let effect = Effect::PostReview {
            dir: card.checkout.0.clone(),
            repo: pr.info.repo.clone(),
            number: pr.info.number,
            head_sha: pr.info.head_sha.clone(),
            body,
            card: id,
            earlier: self.pr_cards.uncertain.get(&id).cloned(),
        };
        let number = pr.info.number;
        let count = rc.current_comments().count();
        self.overlay = None;
        self.pr_cards.posting.insert(id, count);
        self.set_status(format!("Posting your review of PR #{number}…"));
        vec![effect]
    }

    /// The review posted: the card closes as `D` closes it, and the status
    /// line counts every comment, those in the main text too. Not posted:
    /// the card and its comments stay, and the status line says why. A
    /// post that may have reached GitHub is checked for before the next.
    pub(crate) fn on_review_posted(
        &mut self,
        id: u64,
        result: Result<(), super::PostError>,
    ) -> Vec<Effect> {
        use super::PostError;
        let k = self.pr_cards.posting.remove(&id).unwrap_or(0);
        let posted = self
            .queue
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| self.pr_reviews.get(&c.checkout.0))
            .map(|pr| (pr.info.number, pr.info.head_sha.clone()));
        let number = posted.as_ref().map(|p| p.0);
        let pr = number.map_or_else(|| "the PR".to_string(), |n| format!("PR #{n}"));
        match result {
            Err(PostError::NotPosted(e)) => self.set_status(format!("Not posted: {e}")),
            Err(PostError::MayHavePosted { error, started }) => {
                // No refresh runs while a post does, so the head is the one
                // it was made on.
                let tried = self.pr_cards.uncertain.entry(id).or_insert(Uncertain {
                    started,
                    heads: Vec::new(),
                });
                if let Some((_, head)) = posted
                    && !tried.heads.contains(&head)
                {
                    tried.heads.push(head);
                }
                self.set_status(format!(
                    "May not have posted: {error}. Check {pr} on GitHub before posting again"
                ));
            }
            // The attempt that seemed to fail did post it.
            Err(PostError::Reached) => {
                if let Some(n) = number {
                    return self
                        .close_pr(id, Some(format!("Your review had already reached PR #{n}")));
                }
                self.set_status("Your review had already reached GitHub");
            }
            Ok(()) => {
                let with = match k {
                    0 => String::new(),
                    1 => " with 1 comment".into(),
                    k => format!(" with {k} comments"),
                };
                let done = format!("Posted your review on {pr}{with}");
                if number.is_some() {
                    return self.close_pr(id, Some(done));
                }
                self.set_status(done);
            }
        }
        Vec::new()
    }

    /// The PR on card `id` checked out again: its head and GitHub's diff
    /// are the new ones, and the card is diffed again, keeping its comments.
    pub(crate) fn on_pr_refreshed(
        &mut self,
        id: u64,
        result: Result<(PrInfo, PrCheckout, GhDiffRead), String>,
    ) -> Vec<Effect> {
        // Until the card shows the new head's diff, nothing posts against it:
        // the mark stays until `on_review_recomputed`.
        let (info, checkout, gh_diff) = match result {
            Ok(r) => r,
            Err(e) => {
                self.pr_cards.refreshing.remove(&id);
                self.set_status(e);
                return Vec::new();
            }
        };
        let found = self.queue.iter().find(|c| c.id == id).and_then(|card| {
            let CardKind::Review(rc) = &card.kind else {
                return None;
            };
            let commented = rc.current_comments().next().is_some();
            Some((
                card.checkout.0.clone(),
                card.agent.clone(),
                rc.show_full_branch,
                commented,
            ))
        });
        let Some((dir, agent, full_branch, commented)) = found else {
            self.pr_cards.refreshing.remove(&id);
            return Vec::new();
        };
        let Some(pr) = self.pr_reviews.get_mut(&dir) else {
            self.pr_cards.refreshing.remove(&id);
            return Vec::new();
        };
        let number = info.number;
        let moved = pr.info.head_sha != info.head_sha;
        let no_diff = gh_diff.as_ref().err().map(|e| diff_note(number, e));
        pr.info = info;
        pr.gh_diff = gh_diff;
        pr.base_ref = checkout.base_ref;
        let base_ref = pr.base_ref.clone();
        if moved && commented {
            self.pr_cards.moved.insert(id);
            self.set_status(moved_note(number));
        } else {
            self.set_status(format!("Checked out PR #{number} again"));
        }
        // The summary box warns of the move as well; this it would not.
        if let Some(note) = no_diff {
            self.set_status(note);
        }
        vec![Effect::ComputeReview(ReviewRequest {
            checkout: Checkout(dir),
            agent,
            from_tree: None,
            full_branch,
            card: Some(id),
            base_ref: Some(base_ref),
        })]
    }

    /// The diff a `ComputeReview` for card `card` asked for came back
    /// (`shown`), or could not be made: a refresh of the card is over. If
    /// the diff after a refresh could not be made, the card shows an older
    /// diff than the PR's new head, and nothing posts until one is shown.
    pub(crate) fn on_review_recomputed(&mut self, card: Option<u64>, shown: bool) {
        let Some(id) = card else {
            return;
        };
        let refreshed = self.pr_cards.refreshing.remove(&id);
        if shown {
            self.pr_cards.stale.remove(&id);
        } else if refreshed {
            self.pr_cards.stale.insert(id);
        }
    }

    /// A review found nothing: for a PR's first round, says so and lets the
    /// PR go, so its folder's own mode counts again. A worktree aq made
    /// goes as `D` removes it.
    pub(crate) fn on_empty_pr(&mut self, req: &ReviewRequest) -> Vec<Effect> {
        if req.card.is_some() || req.from_tree.is_some() {
            return Vec::new();
        }
        let Some(pr) = self.pr_reviews.remove(&req.checkout.0) else {
            return Vec::new();
        };
        let none = format!(
            "PR #{} has no changes against {}",
            pr.info.number, pr.base_ref
        );
        self.drop_pr_folder(pr, req.checkout.0.clone(), Some(none))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crossterm::event::{KeyCode, KeyEvent};

    use super::*;
    use crate::app::Msg;
    use crate::app::tests::{agent, app, computed, fixture_files, key, review, turn_end, typed};
    use crate::model::Mode;
    use crate::pr::PostError;
    use crate::review::LastRound;

    fn press(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::from(code), Instant::now())
    }

    fn pr(repo: &str, number: u64, title: &str, author: &str) -> PrSummary {
        PrSummary {
            repo: repo.into(),
            number,
            title: title.into(),
            author: author.into(),
            head: format!("head-{number}"),
            draft: false,
            mine: author == "smnai",
            updated: format!("2026-11-{:02}", 30 - number.min(29)),
        }
    }

    fn info(number: u64, mine: bool) -> PrInfo {
        PrInfo {
            repo: "o/api".into(),
            number,
            title: "Fix the parser".into(),
            body: "Body".into(),
            author: if mine { "smnai" } else { "alice" }.into(),
            head_ref: "fix".into(),
            base_ref: "dev".into(),
            head_sha: "abc".into(),
            url: format!("https://github.com/o/api/pull/{number}"),
            mine,
        }
    }

    /// GitHub's diff, read and empty.
    fn gh() -> GhDiffRead {
        Ok(Default::default())
    }

    fn checkout(dir: &str) -> PrCheckout {
        PrCheckout {
            dir: dir.into(),
            main: "/m".into(),
            made: true,
            base_ref: "origin/dev".into(),
        }
    }

    /// An app with sessions in `/a` and `/b`, the finder open and listed.
    fn finder() -> App {
        let mut a = app();
        a.update(Msg::Agents(vec![
            agent("%1", 1, "api", "/a", true),
            agent("%2", 2, "web", "/b", true),
        ]));
        a.update(key('R'));
        a.update(listed(vec![
            pr("o/api", 5, "Fix the parser", "alice"),
            pr("o/web", 9, "New button", "smnai"),
            pr("o/web", 12, "Draft idea", "bob"),
        ]));
        a
    }

    /// The list, naming the repos of `prs`.
    fn listed(prs: Vec<PrSummary>) -> Msg {
        let mut repos: Vec<String> = prs.iter().map(|p| p.repo.clone()).collect();
        repos.dedup();
        Msg::PrList(Ok(PrListing {
            prs,
            repos,
            failed: Vec::new(),
        }))
    }

    /// PR 5 of o/api made ready in `dir`, as asked for by `asked`.
    fn ready(asked: (Option<&str>, u64), dir: &str) -> Msg {
        Msg::PrOpened {
            asked: (asked.0.map(String::from), asked.1),
            result: Ok((info(5, false), checkout(dir), gh())),
        }
    }

    fn failed(asked: (Option<&str>, u64), e: &str) -> Msg {
        Msg::PrOpened {
            asked: (asked.0.map(String::from), asked.1),
            result: Err(e.into()),
        }
    }

    fn open_effect(effects: &[Effect]) -> (PathBuf, Option<String>, u64, Vec<PathBuf>) {
        match effects {
            [
                Effect::OpenPr {
                    dir,
                    repo,
                    number,
                    known,
                },
            ] => (dir.clone(), repo.clone(), *number, known.clone()),
            e => panic!("expected one OpenPr, got {e:?}"),
        }
    }

    fn the_finder(a: &App) -> &PrFinder {
        match &a.overlay {
            Some(Overlay::PrFinder(f)) => f,
            o => panic!("no finder: {o:?}"),
        }
    }

    /// Opens PR 5 in `/w`, with a session in `/a`; the review it asks for.
    fn opened(a: &mut App, dir: &str) -> ReviewRequest {
        let effects = a.update(ready((Some("o/api"), 5), dir));
        match effects.as_slice() {
            [Effect::ComputeReview(req)] => req.clone(),
            e => panic!("expected one ComputeReview, got {e:?}"),
        }
    }

    #[test]
    fn r_opens_the_finder_and_lists_known_repos() {
        let mut a = app();
        a.update(Msg::Agents(vec![
            agent("%1", 1, "api", "/a", true),
            agent("%2", 2, "web", "/b", true),
            agent("%3", 3, "api2", "/a", true),
        ]));
        let effects = a.update(key('R'));
        assert_eq!(
            effects,
            [Effect::ListPrs {
                dirs: vec!["/a".into(), "/b".into()]
            }]
        );
        assert_eq!(the_finder(&a).prs, None);
        a.update(press(KeyCode::Esc));
        assert_eq!(a.overlay, None);
    }

    #[test]
    fn r_does_not_open_over_typing() {
        let mut a = app();
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        a.update(key('N'));
        assert!(a.update(key('R')).is_empty());
        assert!(matches!(a.overlay, Some(Overlay::NewSession(_))));
    }

    #[test]
    fn typing_filters_and_enter_opens_the_picked_pr() {
        let mut a = finder();
        assert_eq!(the_finder(&a).shown().len(), 3);
        typed(&mut a, "web");
        let numbers: Vec<u64> = the_finder(&a).shown().iter().map(|p| p.number).collect();
        assert_eq!(numbers, [9, 12]);
        a.update(press(KeyCode::Down));
        let (dir, repo, number, known) = open_effect(&a.update(press(KeyCode::Enter)));
        assert_eq!((repo.as_deref(), number), (Some("o/web"), 12));
        assert_eq!(known, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert!(known.contains(&dir));
        assert_eq!(
            the_finder(&a).opening,
            Some((Some("o/web".to_string()), 12))
        );
        // While it opens, keys do nothing more.
        assert!(a.update(press(KeyCode::Enter)).is_empty());
    }

    #[test]
    fn j_and_k_move_until_something_is_typed() {
        let mut a = finder();
        a.update(key('j'));
        assert_eq!(the_finder(&a).cursor, 1);
        a.update(key('k'));
        assert_eq!(the_finder(&a).cursor, 0);
        typed(&mut a, "ajk");
        assert_eq!(the_finder(&a).filter.text(), "ajk");
    }

    #[test]
    fn a_pasted_link_opens_that_pr() {
        let mut a = finder();
        typed(&mut a, "https://github.com/o/other/pull/77");
        assert!(the_finder(&a).shown().is_empty());
        let (_, repo, number, _) = open_effect(&a.update(press(KeyCode::Enter)));
        assert_eq!((repo.as_deref(), number), (Some("o/other"), 77));
    }

    #[test]
    fn a_typed_number_takes_the_picked_rows_repo() {
        let mut a = finder();
        typed(&mut a, "#12");
        let (_, repo, number, _) = open_effect(&a.update(press(KeyCode::Enter)));
        assert_eq!((repo.as_deref(), number), (Some("o/web"), 12));
    }

    #[test]
    fn an_opened_pr_becomes_a_review_card_against_its_base() {
        let mut a = finder();
        typed(&mut a, "#5");
        a.update(press(KeyCode::Enter));
        let req = opened(&mut a, "/a");
        assert_eq!(a.overlay, None);
        assert_eq!(
            req,
            ReviewRequest {
                checkout: Checkout("/a".into()),
                agent: a.agents[0].id.clone(),
                from_tree: None,
                full_branch: false,
                card: None,
                base_ref: Some("origin/dev".into()),
            }
        );
        a.update(computed(req, "base", "t1", fixture_files()));
        assert_eq!(a.queue.len(), 1);
        assert_eq!(review(&a).round, 1);
        assert_eq!(a.queue.current().unwrap().checkout, Checkout("/a".into()));
        assert_eq!(a.pr_reviews[Path::new("/a")].info.number, 5);
    }

    #[test]
    fn a_worktree_pr_has_no_session_and_an_empty_one_says_so() {
        let mut a = finder();
        let dir = pr_dir();
        let req = opened(&mut a, &dir.to_string_lossy());
        assert_eq!(req.agent, AgentId::unknown());
        // The worktree aq made goes as D removes it.
        let effects = a.update(computed(req, "base", "base", Vec::new()));
        assert_eq!(
            effects,
            [Effect::RemovePrWorktree {
                main: "/m".into(),
                dir,
            }]
        );
        assert!(a.queue.is_empty());
        assert_eq!(
            a.status_text(),
            Some("PR #5 has no changes against origin/dev")
        );
        assert!(
            a.pr_reviews.is_empty(),
            "the folder's own mode counts again"
        );
        // A session's folder is kept.
        let req = opened(&mut a, "/a");
        assert!(
            a.update(computed(req, "base", "base", Vec::new()))
                .is_empty()
        );
        assert!(a.pr_reviews.is_empty());
    }

    #[test]
    fn a_worktree_aq_made_is_removable_when_found_as_a_sessions_folder() {
        let mut a = app();
        let dir = pr_dir();
        let mut c = checkout(&dir.to_string_lossy());
        // Reopened while a session had the PR's branch there: prepare
        // matched it as a known folder.
        c.made = false;
        a.update(Msg::PrOpened {
            asked: (Some("o/api".into()), 5),
            result: Ok((info(5, true), c, gh())),
        });
        assert!(a.pr_reviews[&dir].made);
        let mut c = checkout("/w");
        c.made = true;
        a.update(Msg::PrOpened {
            asked: (Some("o/api".into()), 5),
            result: Ok((info(5, true), c, gh())),
        });
        assert!(!a.pr_reviews[Path::new("/w")].made, "not in aq's PR folder");
    }

    #[test]
    fn a_pr_already_open_is_shown_not_opened_twice() {
        let mut a = finder();
        let req = opened(&mut a, "/w");
        a.update(computed(req, "base", "t1", fixture_files()));
        let pr_card = a.queue.current().unwrap().id;
        // Another card in front of it.
        let other = turn_end_card(&mut a);
        a.queue.show(other);
        a.update(key('R'));
        a.update(listed(vec![pr("o/api", 5, "Fix the parser", "alice")]));
        assert!(a.update(press(KeyCode::Enter)).is_empty());
        assert_eq!(a.overlay, None);
        assert_eq!(a.queue.current().unwrap().id, pr_card);
        // The job coming back for a folder with a card shows that card,
        // refreshed as the PR's whole branch.
        a.queue.show(other);
        let effects = a.update(ready((Some("o/api"), 5), "/w"));
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        assert_eq!(
            (req.card, req.full_branch, req.from_tree.as_deref()),
            (Some(pr_card), true, None)
        );
        assert_eq!(req.base_ref.as_deref(), Some("origin/dev"));
        assert_eq!(a.queue.current().unwrap().id, pr_card);
    }

    /// A session review card on `/b`; its id.
    fn turn_end_card(a: &mut App) -> u64 {
        a.update(Msg::Agents(vec![
            agent("%1", 1, "api", "/a", true),
            agent("%2", 2, "web", "/b", false),
        ]));
        let effects = turn_end(
            a,
            vec![
                agent("%1", 1, "api", "/a", true),
                agent("%2", 2, "web", "/b", true),
            ],
        );
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        a.update(computed(req.clone(), "base", "t1", fixture_files()));
        a.queue
            .iter()
            .find(|c| c.checkout == Checkout("/b".into()))
            .unwrap()
            .id
    }

    #[test]
    fn an_error_stays_in_the_finder() {
        let mut a = finder();
        a.update(press(KeyCode::Enter));
        a.update(failed((Some("o/api"), 5), "gh: not logged in"));
        let f = the_finder(&a);
        assert_eq!(f.error.as_deref(), Some("gh: not logged in"));
        assert_eq!(f.opening, None);
        // A list that failed stays too.
        a.update(Msg::PrList(Err("gh: no network".into())));
        assert_eq!(the_finder(&a).prs, Some(Err("gh: no network".to_string())));
        // Closed meanwhile: the status line says it.
        a.update(press(KeyCode::Esc));
        a.update(failed((Some("o/api"), 5), "gh: not logged in"));
        assert_eq!(a.status_text(), Some("gh: not logged in"));
    }

    #[test]
    fn a_pr_checkout_always_counts_as_review_mode() {
        let mut a = finder();
        a.config.set_mode(&Checkout("/a".into()), Mode::Vibe);
        let req = opened(&mut a, "/a");
        a.update(computed(req, "base", "t1", fixture_files()));
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", false)]));
        let effects = turn_end(&mut a, vec![agent("%1", 1, "api", "/a", true)]);
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        assert_eq!(req.base_ref.as_deref(), Some("origin/dev"));
    }

    #[test]
    fn r_on_my_pr_card_requests_with_the_prs_base() {
        let mut a = finder();
        a.update(press(KeyCode::Esc));
        // On someone else's PR, r checks it out again instead.
        pr_card(&mut a, Path::new("/a"), true);
        a.rounds.insert(
            "/a".into(),
            LastRound {
                tree: "t0".into(),
                round: 1,
                comments: Vec::new(),
            },
        );
        let effects = a.update(key('r'));
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        assert!(req.full_branch);
        assert_eq!(req.base_ref.as_deref(), Some("origin/dev"));
    }

    #[test]
    fn a_typed_number_opens_the_row_with_that_number() {
        let mut a = finder();
        a.update(listed(vec![
            pr("o/web", 52, "Later", "bob"),
            pr("o/api", 5, "Fix the parser", "alice"),
        ]));
        typed(&mut a, "#5");
        assert_eq!(the_finder(&a).shown().len(), 2);
        assert_eq!(the_finder(&a).picked().unwrap().number, 52);
        let (_, repo, number, _) = open_effect(&a.update(press(KeyCode::Enter)));
        assert_eq!((repo.as_deref(), number), (Some("o/api"), 5));
    }

    #[test]
    fn a_typed_number_without_its_row_takes_no_other_rows_repo() {
        let mut a = finder();
        typed(&mut a, "2");
        let numbers: Vec<u64> = the_finder(&a).shown().iter().map(|p| p.number).collect();
        assert_eq!(numbers, [12]);
        assert!(a.update(press(KeyCode::Enter)).is_empty());
        assert_eq!(
            the_finder(&a).error.as_deref(),
            Some("aq knows several repos: paste the PR's link")
        );
        // With one repo listed, the number is that repo's.
        a.update(listed(vec![pr("o/web", 12, "Draft idea", "bob")]));
        let (_, repo, number, _) = open_effect(&a.update(press(KeyCode::Enter)));
        assert_eq!((repo.as_deref(), number), (Some("o/web"), 2));
    }

    #[test]
    fn two_checkouts_of_one_repo_count_as_one_repo() {
        let mut a = app();
        a.update(Msg::Agents(vec![
            agent("%1", 1, "api", "/a", true),
            agent("%2", 2, "api2", "/a2", true),
        ]));
        a.update(key('R'));
        a.update(Msg::PrList(Ok(PrListing {
            prs: Vec::new(),
            repos: vec!["o/api".into()],
            failed: Vec::new(),
        })));
        typed(&mut a, "7");
        let (_, repo, number, _) = open_effect(&a.update(press(KeyCode::Enter)));
        assert_eq!((repo.as_deref(), number), (Some("o/api"), 7));
    }

    #[test]
    fn r_with_no_session_says_why_and_asks_for_nothing() {
        let mut a = app();
        assert!(a.update(key('R')).is_empty());
        assert_eq!(
            the_finder(&a).prs,
            Some(Err(
                "No session is open in a git repo, so there are no PRs to list".to_string()
            ))
        );
    }

    #[test]
    fn an_older_open_never_touches_a_newer_one() {
        let mut a = finder();
        a.update(press(KeyCode::Enter));
        a.update(press(KeyCode::Esc));
        a.update(key('R'));
        a.update(listed(vec![pr("o/web", 9, "New button", "smnai")]));
        a.update(press(KeyCode::Enter));
        let newer = Some((Some("o/web".to_string()), 9));
        assert_eq!(the_finder(&a).opening, newer);
        // The first one's error goes to the status line.
        a.update(failed((Some("o/api"), 5), "gh: not logged in"));
        assert_eq!(the_finder(&a).opening, newer);
        assert_eq!(the_finder(&a).error, None);
        assert_eq!(a.status_text(), Some("gh: not logged in"));
        // Its success opens its card and leaves the finder be.
        let effects = a.update(ready((Some("o/api"), 5), "/w"));
        assert!(matches!(
            effects.as_slice(),
            [Effect::ComputeReview(ReviewRequest { card: None, .. })]
        ));
        assert_eq!(the_finder(&a).opening, newer);
        assert!(a.pr_reviews.contains_key(Path::new("/w")));
    }

    /// PR 5, by alice or (`mine`) the user, open as a card in `dir` with
    /// round 1 computed; the card's id.
    fn pr_card(a: &mut App, dir: &Path, mine: bool) -> u64 {
        let effects = a.update(Msg::PrOpened {
            asked: (Some("o/api".into()), 5),
            result: Ok((info(5, mine), checkout(&dir.to_string_lossy()), gh())),
        });
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        a.update(computed(req.clone(), "base", "t1", fixture_files()));
        a.queue.current().unwrap().id
    }

    /// Writes one comment on the line under the cursor.
    fn write_comment(a: &mut App, body: &str) {
        a.update(key('c'));
        typed(a, body);
        a.update(crate::app::tests::ctrl('s'));
    }

    fn pr_dir() -> PathBuf {
        super::super::checkout::worktree_dir(&crate::config::paths().state_dir, "o/api", 5)
    }

    #[test]
    fn i_shows_the_description() {
        let mut a = app();
        pr_card(&mut a, Path::new("/w"), false);
        a.update(key('i'));
        let Some(Overlay::Reader(r)) = &a.overlay else {
            panic!("no reader: {:?}", a.overlay);
        };
        assert_eq!(r.words, ["Body"]);
    }

    #[test]
    fn d_closes_and_removes_a_worktree_aq_made() {
        let mut a = app();
        let dir = pr_dir();
        pr_card(&mut a, &dir, false);
        a.rounds.insert(
            dir.clone(),
            LastRound {
                tree: "t0".into(),
                round: 1,
                comments: Vec::new(),
            },
        );
        assert!(a.update(key('D')).is_empty());
        assert_eq!(a.closing_pr, a.queue.current().map(|c| c.id));
        // n keeps it.
        a.update(key('n'));
        assert_eq!(a.closing_pr, None);
        assert_eq!(a.queue.len(), 1);
        a.update(key('D'));
        let effects = a.update(key('y'));
        assert_eq!(
            effects,
            [Effect::RemovePrWorktree {
                main: "/m".into(),
                dir: dir.clone(),
            }]
        );
        assert!(a.queue.is_empty());
        assert!(a.pr_reviews.is_empty());
        assert!(a.rounds.is_empty());
        assert_eq!(a.closing_pr, None);
        assert_eq!(a.status_text(), Some("Closed the review of PR #5"));
    }

    #[test]
    fn d_never_removes_a_folder_outside_aqs_pr_folder() {
        let mut a = app();
        let outside = crate::config::paths().state_dir.join("o-api-5");
        for dir in [PathBuf::from("/w"), outside, pr_dir().join("..")] {
            pr_card(&mut a, &dir, false);
            a.update(key('D'));
            assert!(a.update(key('y')).is_empty(), "{}", dir.display());
            assert!(a.queue.is_empty());
            assert!(a.pr_reviews.is_empty());
            assert_eq!(a.status_text(), Some("Closed the review of PR #5"));
        }
    }

    #[test]
    fn d_keeps_a_worktree_with_a_session_in_it() {
        let mut a = app();
        let dir = pr_dir();
        let d = dir.to_string_lossy().into_owned();
        a.update(Msg::Agents(vec![agent("%1", 1, "api", &d, true)]));
        pr_card(&mut a, &dir, false);
        a.update(key('D'));
        assert!(a.update(key('y')).is_empty());
        assert!(a.queue.is_empty());
        assert!(a.pr_reviews.is_empty());
        assert_eq!(
            a.status_text(),
            Some("A session is working in it; the worktree is kept")
        );
    }

    #[test]
    fn d_on_my_pr_in_my_own_folder_removes_nothing() {
        let mut a = finder();
        a.update(press(KeyCode::Esc));
        let mut c = checkout("/a");
        c.made = false;
        a.update(Msg::PrOpened {
            asked: (Some("o/api".into()), 5),
            result: Ok((info(5, true), c, gh())),
        });
        let req = ReviewRequest {
            checkout: Checkout("/a".into()),
            agent: a.agents[0].id.clone(),
            from_tree: None,
            full_branch: false,
            card: None,
            base_ref: Some("origin/dev".into()),
        };
        a.update(computed(req, "base", "t1", fixture_files()));
        a.update(key('D'));
        assert!(a.update(key('y')).is_empty());
        assert!(a.queue.is_empty());
        assert_eq!(a.status_text(), Some("Closed the review of PR #5"));
    }

    const MY_PR_LINE: &str = "This is PR #5 (https://github.com/o/api/pull/5). Fix what the \
comments below ask, run the tests, then commit and push to the PR's branch.\n\n";

    #[test]
    fn my_pr_sends_to_the_session_in_its_folder_with_the_pr_line() {
        let mut a = app();
        let id = pr_card(&mut a, Path::new("/w"), true);
        // The session starts after the card was made.
        assert_eq!(a.queue.current().unwrap().agent, AgentId::unknown());
        a.update(Msg::Agents(vec![
            agent("%1", 1, "api", "/a", true),
            agent("%2", 2, "pr", "/w", true),
        ]));
        write_comment(&mut a, "store down?");
        let effects = a.update(key('s'));
        let text = format!(
            "{MY_PR_LINE}Review comments (round 1):\n\nsrc/limit.rs:40\n\
             > let b = store.get(key).unwrap_or_default();\nstore down?"
        );
        assert_eq!(
            effects,
            [Effect::Send {
                agent: a.agents[1].id.clone(),
                text,
                card: Some(id),
            }]
        );
        a.update(Msg::SendDone { card: id });
        assert!(a.queue.is_empty());
        assert_eq!(a.status_text(), Some("Sent 1 comment to pr"));
    }

    #[test]
    fn my_pr_with_no_session_starts_one_with_the_comments() {
        let mut a = app();
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/a", true)]));
        pr_card(&mut a, Path::new("/w"), true);
        write_comment(&mut a, "store down?");
        let effects = a.update(key('s'));
        let text = format!(
            "{MY_PR_LINE}Review comments (round 1):\n\nsrc/limit.rs:40\n\
             > let b = store.get(key).unwrap_or_default();\nstore down?"
        );
        assert_eq!(
            effects,
            [Effect::NewSession {
                dir: "/w".into(),
                prompt: Some(text),
                worktree: false,
            }]
        );
        assert!(a.queue.is_empty());
        let round = &a.rounds[Path::new("/w")];
        assert_eq!((round.tree.as_str(), round.round), ("t1", 1));
        assert_eq!(round.comments.len(), 1);
        assert_eq!(
            a.status_text(),
            Some("Started a session on PR #5 with your comments")
        );
        // The new session's first turn ends: round 2 diffs from this tree.
        a.update(Msg::Agents(vec![
            agent("%1", 1, "api", "/a", true),
            agent("%2", 2, "pr", "/w", false),
        ]));
        let effects = turn_end(
            &mut a,
            vec![
                agent("%1", 1, "api", "/a", true),
                agent("%2", 2, "pr", "/w", true),
            ],
        );
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        assert_eq!(req.from_tree.as_deref(), Some("t1"));
    }

    #[test]
    fn a_on_my_pr_approves_as_before_and_ends_the_pr_review() {
        let mut a = app();
        pr_card(&mut a, Path::new("/w"), true);
        assert!(a.update(key('a')).is_empty());
        assert!(a.queue.is_empty());
        assert_eq!(a.status_text(), Some("Approved"));
        assert!(
            a.pr_reviews.is_empty(),
            "the folder's own mode counts again"
        );
        assert!(a.pr_cards.approved.is_empty());
        // In a worktree aq made, the worktree goes as D removes it.
        let mut a = app();
        let dir = pr_dir();
        pr_card(&mut a, &dir, true);
        let effects = a.update(key('a'));
        assert_eq!(
            effects,
            [Effect::RemovePrWorktree {
                main: "/m".into(),
                dir,
            }]
        );
        assert!(a.pr_reviews.is_empty());
        assert!(a.rounds.is_empty());
        assert_eq!(a.status_text(), Some("Approved"));
    }

    #[test]
    fn approving_my_pr_to_a_waiting_session_ends_the_pr_review_once_sent() {
        let mut a = app();
        let dir = pr_dir();
        let d = dir.to_string_lossy().into_owned();
        a.update(Msg::Agents(vec![agent("%1", 1, "pr", &d, true)]));
        let id = pr_card(&mut a, &dir, true);
        a.queue.current_mut().unwrap().agent = a.agents[0].id.clone();
        let effects = a.update(key('a'));
        assert!(matches!(effects.as_slice(), [Effect::Send { .. }]));
        assert!(a.pr_reviews.contains_key(&dir), "not yet sent");
        // D waits while the message is on its way.
        assert!(a.update(key('D')).is_empty());
        assert_eq!(a.closing_pr, None);
        assert_eq!(
            a.status_text(),
            Some("Your message about PR #5 is on its way to the session")
        );
        let effects = a.update(Msg::SendDone { card: id });
        assert!(effects.is_empty(), "a session works in the worktree");
        assert!(a.queue.is_empty());
        assert!(a.pr_reviews.is_empty());
        assert_eq!(
            a.status_text(),
            Some("Approved; a session is working in its worktree, so it is kept")
        );
    }

    #[test]
    fn sending_my_pr_comments_keeps_the_pr_review() {
        let mut a = app();
        a.update(Msg::Agents(vec![agent("%1", 1, "pr", "/w", true)]));
        let id = pr_card(&mut a, Path::new("/w"), true);
        write_comment(&mut a, "store down?");
        a.update(key('s'));
        a.update(Msg::SendDone { card: id });
        assert!(a.queue.is_empty());
        assert!(a.pr_reviews.contains_key(Path::new("/w")));
    }

    #[test]
    fn a_side_question_on_a_pr_includes_its_description() {
        let mut a = app();
        pr_card(&mut a, Path::new("/w"), false);
        a.update(key('W'));
        let Some(Overlay::Ask(ask)) = &a.overlay else {
            panic!("no ask box: {:?}", a.overlay);
        };
        let ctx = &ask.context;
        for want in ["PR #5", "Fix the parser", "alice", "Body", "src/limit.rs"] {
            assert!(ctx.contains(want), "{want} in {ctx}");
        }
        assert!(ctx.find("Body").unwrap() < ctx.find("src/limit.rs").unwrap());
    }

    #[test]
    fn a_session_card_sends_as_before_and_has_no_pr_keys() {
        let mut a = app();
        let req = crate::app::tests::finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        let id = a.queue.current().unwrap().id;
        a.update(key('i'));
        assert_eq!(a.overlay, None);
        a.update(key('D'));
        assert_eq!(a.closing_pr, None);
        write_comment(&mut a, "store down?");
        let effects = a.update(key('s'));
        assert_eq!(
            effects,
            [Effect::Send {
                agent: a.agents[0].id.clone(),
                text: "Review comments (round 1):\n\nsrc/limit.rs:40\n\
                       > let b = store.get(key).unwrap_or_default();\nstore down?"
                    .into(),
                card: Some(id),
            }]
        );
    }

    #[test]
    fn my_pr_keeps_the_session_that_raised_the_card() {
        let mut a = app();
        a.update(Msg::Agents(vec![
            agent("%1", 1, "one", "/w", true),
            agent("%2", 2, "two", "/w", true),
        ]));
        let id = pr_card(&mut a, Path::new("/w"), true);
        let second = a.agents[1].id.clone();
        a.queue.current_mut().unwrap().agent = second.clone();
        write_comment(&mut a, "store down?");
        let effects = a.update(key('s'));
        let [Effect::Send { agent, card, .. }] = effects.as_slice() else {
            panic!("expected one Send, got {effects:?}");
        };
        assert_eq!((agent, *card), (&second, Some(id)));
    }

    #[test]
    fn a_close_question_goes_when_another_card_comes_in_front() {
        let mut a = app();
        let id = pr_card(&mut a, Path::new("/w"), false);
        a.update(key('D'));
        assert_eq!(a.closing_pr, Some(id));
        let other = turn_end_card(&mut a);
        a.queue.show(other);
        a.update(Msg::Agents(a.agents.clone()));
        assert_eq!(a.closing_pr, None);
        // Back on the PR card, y does nothing by surprise.
        a.queue.show(id);
        assert!(a.update(key('y')).is_empty());
        assert_eq!(a.queue.len(), 2);
    }

    /// GitHub's diff of PR 5: new lines 38-41 of src/limit.rs only.
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

    /// Alice's PR 5 open in `dir` with GitHub's diff read; the card's id.
    fn their_pr(a: &mut App, dir: &Path) -> u64 {
        let id = pr_card(a, dir, false);
        a.pr_reviews.get_mut(dir).unwrap().gh_diff =
            Ok(super::super::place::parse_gh_diff(GH_DIFF));
        id
    }

    fn submit_box(a: &App) -> (u64, Event, String) {
        match &a.overlay {
            Some(Overlay::PrSubmit { card, event, input }) => (*card, *event, input.text()),
            o => panic!("no summary box: {o:?}"),
        }
    }

    #[test]
    fn s_a_and_x_open_the_summary_box_with_their_event() {
        let mut a = app();
        let id = their_pr(&mut a, Path::new("/w"));
        for (k, event) in [
            ('s', Event::Comment),
            ('a', Event::Approve),
            ('X', Event::RequestChanges),
        ] {
            assert!(a.update(key(k)).is_empty());
            assert_eq!(submit_box(&a), (id, event, String::new()));
            a.update(press(KeyCode::Esc));
            assert_eq!(a.overlay, None);
            assert_eq!(a.queue.len(), 1, "Esc keeps the card");
        }
    }

    #[test]
    fn enter_posts_the_review_body_with_the_head_commit() {
        let mut a = app();
        let id = their_pr(&mut a, Path::new("/w"));
        write_comment(&mut a, "store down?");
        a.update(key('X'));
        typed(&mut a, "Needs a test");
        let effects = a.update(press(KeyCode::Enter));
        let rc = review(&a);
        let want = super::super::place::review_body(
            Some(&super::super::place::parse_gh_diff(GH_DIFF)),
            &rc.comments,
            rc.round,
            Event::RequestChanges,
            "Needs a test",
            "abc",
        );
        assert_eq!(want["comments"].as_array().unwrap().len(), 1);
        assert_eq!(
            effects,
            [Effect::PostReview {
                dir: "/w".into(),
                repo: "o/api".into(),
                number: 5,
                head_sha: "abc".into(),
                body: want,
                card: id,
                earlier: None,
            }]
        );
        assert_eq!(a.overlay, None);
        assert_eq!(a.status_text(), Some("Posting your review of PR #5…"));
        // While it posts, nothing posts twice, and D does not close it.
        assert!(a.update(key('s')).is_empty());
        assert_eq!(a.overlay, None);
        assert!(a.update(key('D')).is_empty());
        assert_eq!(a.closing_pr, None);
        assert_eq!(a.status_text(), Some("Posting your review of PR #5…"));
    }

    #[test]
    fn a_posted_review_closes_the_card() {
        let mut a = app();
        let dir = pr_dir();
        let id = their_pr(&mut a, &dir);
        write_comment(&mut a, "store down?");
        a.update(key('s'));
        a.update(press(KeyCode::Enter));
        let effects = a.update(Msg::ReviewPosted {
            card: id,
            result: Ok(()),
        });
        assert_eq!(
            effects,
            [Effect::RemovePrWorktree {
                main: "/m".into(),
                dir,
            }]
        );
        assert!(a.queue.is_empty());
        assert!(a.pr_reviews.is_empty());
        assert!(a.rounds.is_empty());
        assert_eq!(
            a.status_text(),
            Some("Posted your review on PR #5 with 1 comment")
        );
    }

    #[test]
    fn a_posted_review_keeps_a_worktree_with_a_session_in_it() {
        let mut a = app();
        let dir = pr_dir();
        let d = dir.to_string_lossy().into_owned();
        a.update(Msg::Agents(vec![agent("%1", 1, "api", &d, true)]));
        let id = their_pr(&mut a, &dir);
        a.update(key('a'));
        a.update(press(KeyCode::Enter));
        let effects = a.update(Msg::ReviewPosted {
            card: id,
            result: Ok(()),
        });
        assert!(effects.is_empty());
        assert!(a.queue.is_empty());
        assert_eq!(
            a.status_text(),
            Some(
                "Posted your review on PR #5; a session is working in its worktree, so it is kept"
            )
        );
    }

    #[test]
    fn a_failed_post_keeps_the_card_and_comments() {
        let mut a = app();
        let id = their_pr(&mut a, Path::new("/w"));
        write_comment(&mut a, "store down?");
        a.update(key('s'));
        a.update(press(KeyCode::Enter));
        let effects = a.update(Msg::ReviewPosted {
            card: id,
            result: Err(PostError::NotPosted(
                "PR #5 changed on GitHub since you opened it: press r to refresh".into(),
            )),
        });
        assert!(effects.is_empty());
        assert_eq!(a.queue.len(), 1);
        assert_eq!(review(&a).comments.len(), 1);
        assert!(a.pr_reviews.contains_key(Path::new("/w")));
        assert_eq!(
            a.status_text(),
            Some("Not posted: PR #5 changed on GitHub since you opened it: press r to refresh")
        );
        // It can be posted again.
        a.update(key('s'));
        assert!(matches!(a.overlay, Some(Overlay::PrSubmit { .. })));
    }

    #[test]
    fn request_changes_needs_a_comment_or_a_summary() {
        let mut a = app();
        their_pr(&mut a, Path::new("/w"));
        for k in ['X', 's'] {
            a.update(key(k));
            assert!(a.update(press(KeyCode::Enter)).is_empty());
            assert!(matches!(a.overlay, Some(Overlay::PrSubmit { .. })));
            assert_eq!(a.status_text(), Some("GitHub needs a comment or a summary"));
            a.update(press(KeyCode::Esc));
        }
        // A summary alone is enough.
        a.update(key('X'));
        typed(&mut a, "Please add tests");
        assert!(matches!(
            a.update(press(KeyCode::Enter)).as_slice(),
            [Effect::PostReview { .. }]
        ));
    }

    #[test]
    fn an_empty_approval_is_allowed() {
        let mut a = app();
        their_pr(&mut a, Path::new("/w"));
        a.update(key('a'));
        let effects = a.update(press(KeyCode::Enter));
        let [Effect::PostReview { body, .. }] = effects.as_slice() else {
            panic!("expected one PostReview, got {effects:?}");
        };
        assert_eq!(body["event"], "APPROVE");
        assert_eq!(body["body"], "");
    }

    /// Alice's PR 5 opened in `/w` with GitHub's diff refused as `error`.
    fn opened_without_diff(a: &mut App, error: &str) -> u64 {
        let effects = a.update(Msg::PrOpened {
            asked: (Some("o/api".into()), 5),
            result: Ok((info(5, false), checkout("/w"), Err(error.into()))),
        });
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        a.update(computed(req.clone(), "base", "t1", fixture_files()));
        a.queue.current().unwrap().id
    }

    #[test]
    fn without_githubs_diff_every_comment_goes_in_the_main_text() {
        let mut a = app();
        let id = opened_without_diff(&mut a, "gh: Server Error (HTTP 502)");
        assert_eq!(
            a.status_text(),
            Some("GitHub's diff for PR #5 could not be read: gh: Server Error (HTTP 502)")
        );
        write_comment(&mut a, "store down?");
        assert_eq!(a.pr_placements(id), Some([0, 1]));
        a.update(key('s'));
        assert!(matches!(a.overlay, Some(Overlay::PrSubmit { .. })));
        let effects = a.update(press(KeyCode::Enter));
        let [Effect::PostReview { body, .. }] = effects.as_slice() else {
            panic!("expected one PostReview, got {effects:?}");
        };
        assert_eq!(body["comments"], serde_json::json!([]));
        assert_eq!(body["body"], "`src/limit.rs:40`: store down?");
        // A diff GitHub will not show says so: r does not help.
        let mut a = app();
        opened_without_diff(
            &mut a,
            "gh: Sorry, the diff exceeded the maximum (HTTP 406)",
        );
        assert_eq!(
            a.status_text(),
            Some("GitHub will not show this diff: all comments will go in the review text")
        );
    }

    #[test]
    fn a_refresh_without_githubs_diff_says_so() {
        let mut a = app();
        let id = their_pr(&mut a, Path::new("/w"));
        a.update(key('r'));
        a.update(Msg::PrRefreshed {
            card: id,
            result: Ok((info(5, false), checkout("/w"), Err("HTTP 502".into()))),
        });
        assert_eq!(
            a.status_text(),
            Some("GitHub's diff for PR #5 could not be read: HTTP 502")
        );
        assert!(a.pr_reviews[Path::new("/w")].gh_diff.is_err());
    }

    #[test]
    fn x_on_my_pr_does_nothing() {
        let mut a = app();
        pr_card(&mut a, Path::new("/w"), true);
        assert!(a.update(key('X')).is_empty());
        assert_eq!(a.overlay, None);
        assert_eq!(a.queue.len(), 1);
        assert_eq!(
            a.status_text(),
            Some("X requests changes on someone else's PR")
        );
    }

    #[test]
    fn a_session_review_card_still_sends_to_its_session() {
        let mut a = app();
        let req = crate::app::tests::finish_round(&mut a);
        a.update(computed(req, "base", "t1", fixture_files()));
        a.update(key('X'));
        assert_eq!(a.overlay, None);
        write_comment(&mut a, "store down?");
        let effects = a.update(key('s'));
        assert!(
            matches!(effects.as_slice(), [Effect::Send { .. }]),
            "{effects:?}"
        );
        assert_eq!(a.overlay, None);
    }

    #[test]
    fn a_post_that_may_have_reached_github_is_checked_for_next_time() {
        let mut a = app();
        let id = their_pr(&mut a, Path::new("/w"));
        write_comment(&mut a, "store down?");
        a.update(key('s'));
        a.update(press(KeyCode::Enter));
        a.update(Msg::ReviewPosted {
            card: id,
            result: Err(PostError::MayHavePosted {
                error: "timed out after 60s".into(),
                started: 500,
            }),
        });
        assert_eq!(a.queue.len(), 1);
        assert_eq!(
            a.status_text(),
            Some(
                "May not have posted: timed out after 60s. Check PR #5 on GitHub before posting again"
            )
        );
        let tried = |heads: &[&str]| {
            Some(Uncertain {
                started: 500,
                heads: heads.iter().map(|h| h.to_string()).collect(),
            })
        };
        a.update(key('s'));
        let effects = a.update(press(KeyCode::Enter));
        let [Effect::PostReview { earlier, .. }] = effects.as_slice() else {
            panic!("expected one PostReview, got {effects:?}");
        };
        assert_eq!(*earlier, tried(&["abc"]));
        // A head check that fails keeps the first attempt's time.
        a.update(Msg::ReviewPosted {
            card: id,
            result: Err(PostError::NotPosted("HTTP 502".into())),
        });
        assert_eq!(a.status_text(), Some("Not posted: HTTP 502"));
        // A refresh to a new head keeps the old one: a review that reached
        // GitHub on it is still found.
        a.update(key('r'));
        let mut fresh = info(5, false);
        fresh.head_sha = "def".into();
        a.update(Msg::PrRefreshed {
            card: id,
            result: Ok((
                fresh,
                checkout("/w"),
                Ok(super::super::place::parse_gh_diff(GH_DIFF)),
            )),
        });
        done_refreshing(&mut a, id);
        a.update(key('s'));
        let effects = a.update(press(KeyCode::Enter));
        let [Effect::PostReview { earlier, .. }] = effects.as_slice() else {
            panic!("expected one PostReview, got {effects:?}");
        };
        assert_eq!(*earlier, tried(&["abc"]));
        a.update(Msg::ReviewPosted {
            card: id,
            result: Err(PostError::MayHavePosted {
                error: "timed out after 60s".into(),
                started: 900,
            }),
        });
        a.update(key('s'));
        let effects = a.update(press(KeyCode::Enter));
        let [Effect::PostReview { earlier, .. }] = effects.as_slice() else {
            panic!("expected one PostReview, got {effects:?}");
        };
        assert_eq!(*earlier, tried(&["abc", "def"]));
    }

    #[test]
    fn a_review_that_had_reached_github_closes_the_card_as_posted() {
        let mut a = app();
        let dir = pr_dir();
        let id = their_pr(&mut a, &dir);
        a.update(key('a'));
        a.update(press(KeyCode::Enter));
        let effects = a.update(Msg::ReviewPosted {
            card: id,
            result: Err(PostError::Reached),
        });
        assert_eq!(
            effects,
            [Effect::RemovePrWorktree {
                main: "/m".into(),
                dir,
            }]
        );
        assert!(a.queue.is_empty());
        assert!(a.pr_reviews.is_empty());
        assert_eq!(
            a.status_text(),
            Some("Your review had already reached PR #5")
        );
    }

    #[test]
    fn posting_waits_for_the_refreshed_diff() {
        let mut a = app();
        let id = their_pr(&mut a, Path::new("/w"));
        a.update(key('r'));
        let mut fresh = info(5, false);
        fresh.head_sha = "def".into();
        let effects = a.update(Msg::PrRefreshed {
            card: id,
            result: Ok((fresh, checkout("/w"), gh())),
        });
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        // The head is new but the card still shows the old diff.
        for k in ['s', 'a', 'X', 'r', 'D'] {
            assert!(a.update(key(k)).is_empty(), "{k}");
            assert_eq!(a.overlay, None, "{k}");
            assert_eq!(a.closing_pr, None, "{k}");
        }
        a.update(computed(req.clone(), "base", "t2", fixture_files()));
        assert!(a.pr_cards.refreshing.is_empty());
        // A diff that cannot be made ends the refresh too.
        a.update(key('r'));
        a.update(Msg::PrRefreshed {
            card: id,
            result: Ok((info(5, false), checkout("/w"), gh())),
        });
        a.update(Msg::ReviewFailed {
            req: req.clone(),
            error: "Could not diff w: gone".into(),
        });
        assert_eq!(a.status_text(), Some("Could not diff w: gone"));
        assert!(a.pr_cards.refreshing.is_empty());
        // The card still shows the old diff: nothing posts until r shows
        // the new one.
        for k in ['s', 'a', 'X'] {
            assert!(a.update(key(k)).is_empty(), "{k}");
            assert_eq!(a.overlay, None, "{k}");
            assert_eq!(
                a.status_text(),
                Some("PR #5's diff could not be shown: press r")
            );
        }
        assert_eq!(a.update(key('r')).len(), 1);
        a.update(Msg::PrRefreshed {
            card: id,
            result: Ok((info(5, false), checkout("/w"), gh())),
        });
        a.update(computed(req.clone(), "base", "t3", fixture_files()));
        a.update(key('s'));
        assert!(matches!(a.overlay, Some(Overlay::PrSubmit { .. })));
        a.update(press(KeyCode::Esc));
        a.update(key('D'));
        assert_eq!(a.closing_pr, Some(id));
    }

    #[test]
    fn the_count_posted_includes_comments_in_the_main_text() {
        let mut a = app();
        let id = their_pr(&mut a, &pr_dir());
        write_comment(&mut a, "store down?");
        if let Some(c) = a.queue.current_mut()
            && let CardKind::Review(rc) = &mut c.kind
        {
            let mut other = rc.comments[0].clone();
            other.path = "src/http.rs".into();
            rc.comments.push(other);
        }
        a.update(key('s'));
        let effects = a.update(press(KeyCode::Enter));
        let [Effect::PostReview { body, .. }] = effects.as_slice() else {
            panic!("expected one PostReview, got {effects:?}");
        };
        assert_eq!(body["comments"].as_array().unwrap().len(), 1);
        a.update(Msg::ReviewPosted {
            card: id,
            result: Ok(()),
        });
        assert_eq!(
            a.status_text(),
            Some("Posted your review on PR #5 with 2 comments")
        );
    }

    #[test]
    fn while_a_refresh_runs_nothing_posts_or_closes() {
        let mut a = app();
        let id = their_pr(&mut a, Path::new("/w"));
        assert_eq!(a.update(key('r')).len(), 1);
        for k in ['r', 's', 'a', 'X', 'D'] {
            assert!(a.update(key(k)).is_empty(), "{k}");
            assert_eq!(a.overlay, None, "{k}");
            assert_eq!(a.closing_pr, None, "{k}");
            assert_eq!(a.status_text(), Some("Checking out PR #5 again…"));
        }
        a.update(Msg::PrRefreshed {
            card: id,
            result: Err("Could not refresh PR #5: dirty".into()),
        });
        a.update(key('s'));
        assert!(matches!(a.overlay, Some(Overlay::PrSubmit { .. })));
    }

    #[test]
    fn r_is_refused_while_a_session_works_in_the_worktree() {
        let mut a = app();
        a.update(Msg::Agents(vec![agent("%1", 1, "api", "/w", true)]));
        their_pr(&mut a, Path::new("/w"));
        assert!(a.update(key('r')).is_empty());
        assert_eq!(
            a.status_text(),
            Some("A session is working in PR #5's worktree, so it is not checked out again")
        );
        assert!(a.pr_cards.refreshing.is_empty());
    }

    /// The diff the refresh of card `id` asked for comes back.
    fn done_refreshing(a: &mut App, id: u64) {
        let req = ReviewRequest {
            checkout: Checkout("/w".into()),
            agent: AgentId::unknown(),
            from_tree: None,
            full_branch: false,
            card: Some(id),
            base_ref: Some("origin/dev".into()),
        };
        a.update(computed(req, "base", "t2", fixture_files()));
    }

    #[test]
    fn a_refresh_that_moves_the_head_under_comments_says_so() {
        let mut a = app();
        let id = their_pr(&mut a, Path::new("/w"));
        // No comments: nothing to warn about.
        a.update(key('r'));
        let mut fresh = info(5, false);
        fresh.head_sha = "def".into();
        a.update(Msg::PrRefreshed {
            card: id,
            result: Ok((fresh.clone(), checkout("/w"), gh())),
        });
        done_refreshing(&mut a, id);
        assert_eq!(a.status_text(), Some("Checked out PR #5 again"));
        assert!(a.pr_cards.moved.is_empty());
        write_comment(&mut a, "store down?");
        // The same head again: nothing moved.
        a.update(key('r'));
        a.update(Msg::PrRefreshed {
            card: id,
            result: Ok((fresh.clone(), checkout("/w"), gh())),
        });
        done_refreshing(&mut a, id);
        assert!(a.pr_cards.moved.is_empty());
        fresh.head_sha = "fed".into();
        a.update(key('r'));
        a.update(Msg::PrRefreshed {
            card: id,
            result: Ok((fresh, checkout("/w"), gh())),
        });
        assert_eq!(
            a.status_text(),
            Some("PR #5 has new commits since you wrote these comments: check where they sit")
        );
        assert!(a.pr_cards.moved.contains(&id));
    }

    #[test]
    fn r_on_their_pr_refreshes_the_checkout_and_keeps_the_comments() {
        let mut a = app();
        let id = their_pr(&mut a, Path::new("/w"));
        write_comment(&mut a, "store down?");
        let effects = a.update(key('r'));
        assert_eq!(
            effects,
            [Effect::RefreshPr {
                card: id,
                info: info(5, false),
                main: "/m".into(),
            }]
        );
        let mut fresh = info(5, false);
        fresh.head_sha = "def".into();
        fresh.base_ref = "main".into();
        let mut moved_base = checkout("/w");
        moved_base.base_ref = "origin/main".into();
        let effects = a.update(Msg::PrRefreshed {
            card: id,
            result: Ok((fresh, moved_base, gh())),
        });
        let [Effect::ComputeReview(req)] = effects.as_slice() else {
            panic!("expected one ComputeReview, got {effects:?}");
        };
        assert_eq!(req.card, Some(id));
        assert_eq!(req.base_ref.as_deref(), Some("origin/main"), "the new base");
        assert_eq!(a.pr_reviews[Path::new("/w")].base_ref, "origin/main");
        let pr = &a.pr_reviews[Path::new("/w")];
        assert_eq!(pr.info.head_sha, "def");
        assert_eq!(pr.gh_diff, gh());
        a.update(computed(req.clone(), "base", "t2", fixture_files()));
        assert_eq!(review(&a).comments.len(), 1);
        // A failed refresh says why and changes nothing.
        a.update(key('r'));
        a.update(Msg::PrRefreshed {
            card: id,
            result: Err("Could not refresh PR #5: dirty".into()),
        });
        assert_eq!(a.status_text(), Some("Could not refresh PR #5: dirty"));
        assert_eq!(a.pr_reviews[Path::new("/w")].info.head_sha, "def");
    }
}
