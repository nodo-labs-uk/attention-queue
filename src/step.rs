//! Step-by-step cards: each edit Claude wants to make, held by its
//! PreToolUse hook until the user allows or refuses it. Pure: no I/O.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::Value;

use crate::app::{App, Effect, Overlay};
use crate::edit::EditPreview;
use crate::git::GrepHit;
use crate::ipc::HookReply;
use crate::model::{AgentId, Card, CardKind, Checkout, Mode};
use crate::ui::fold::{self, Seg};
use crate::ui::input::{InputEvent, TextInput};

/// `n`: the reason Claude reads starts with this.
pub const REJECT_PREFIX: &str = "Rejected by reviewer: ";
/// `e`: the change Claude is asked for starts with this.
pub const CHANGE_PREFIX: &str = "Please change this edit instead: ";
/// How long a decision made after its hook went away waits for the retry.
pub const RETRY_FOR: Duration = Duration::from_secs(10 * 60);
/// How long a step card must be on screen before `y`, `n` or `e` count, so
/// a key typed ahead does not decide a card nobody saw.
pub const LOOK_FOR: Duration = Duration::from_millis(300);
/// Lines in the changed middle of a file above which no line diff is tried.
const DIFF_MAX: usize = 4000;

/// Identifies one edit: the same tool call retried gives the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct EditKey(u64);

impl EditKey {
    pub fn new(session_id: &str, tool_name: &str, tool_input: &Value) -> Self {
        let mut h = DefaultHasher::new();
        (session_id, tool_name, canonical(tool_input)).hash(&mut h);
        EditKey(h.finish())
    }
}

/// `v` as JSON with object keys sorted at every level, whatever order the
/// map keeps them in.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical(&m[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(canonical).collect();
            format!("[{}]", parts.join(","))
        }
        v => v.to_string(),
    }
}

/// A PreToolUse request with what the runtime read for it.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingEdit {
    pub session_id: String,
    /// The checkout of the request's cwd; `None` outside git.
    pub checkout: Option<Checkout>,
    /// The checkout holding the edited file; `None` outside git.
    pub file_checkout: Option<Checkout>,
    pub tool_name: String,
    pub tool_input: Value,
    pub path: PathBuf,
    pub preview: EditPreview,
    pub description: Option<String>,
    pub at: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Same,
    Del,
    Add,
}

/// One line of the file view: the whole file after the edit, with the lines
/// it removes shown where they were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    pub text: String,
    /// 1-based line number on its own side (before for `Del`, after otherwise).
    pub no: u32,
    /// 0-based index of the after line at, or just after, this row.
    pub at: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StepView {
    /// Row index; on a closed fold, its first row.
    pub cursor: usize,
    /// Starts of folds the user opened.
    pub open: BTreeSet<usize>,
    /// First key of `za`, `zR` or `gd`.
    pub pending: Option<char>,
    /// Which identifier on the cursor line `gd` searches for.
    pub word: usize,
    /// `m`: show the raw tool input instead of the file.
    pub raw: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StepCard {
    /// Hooks waiting on this card. Empty when they all went away.
    pub req_ids: Vec<u64>,
    pub key: EditKey,
    pub session_id: String,
    pub tool_name: String,
    pub path: PathBuf,
    pub preview: EditPreview,
    pub description: Option<String>,
    pub step_no: u32,
    pub decided: Option<HookReply>,
    /// The tool input as pretty JSON, for `m`.
    pub raw: String,
    pub rows: Vec<Row>,
    /// Row ranges hidden until opened.
    pub folds: Vec<Range<usize>>,
    pub view: StepView,
}

impl StepCard {
    pub fn new(req_id: u64, key: EditKey, step_no: u32, e: PendingEdit) -> Self {
        let rows = rows(&e.preview);
        let folds = fold::folds(&rows);
        let mut card = StepCard {
            req_ids: vec![req_id],
            key,
            session_id: e.session_id,
            tool_name: e.tool_name,
            path: e.path,
            raw: serde_json::to_string_pretty(&e.tool_input).unwrap_or_default(),
            preview: e.preview,
            description: e.description,
            step_no,
            decided: None,
            rows,
            folds,
            view: StepView::default(),
        };
        card.view.cursor = card.change_rows().first().copied().unwrap_or(0);
        card
    }

    pub fn segments(&self) -> Vec<Seg> {
        fold::segments(self.rows.len(), &self.folds, &self.view.open)
    }

    /// The changed line ranges of the after file, 0-based.
    pub fn changed(&self) -> &[Range<u32>] {
        match &self.preview {
            EditPreview::Placed { changed, .. } => changed,
            EditPreview::Unplaced { .. } => &[],
        }
    }

    /// The first row of each changed range.
    pub fn change_rows(&self) -> Vec<usize> {
        if let EditPreview::Unplaced { .. } = self.preview {
            return vec![0];
        }
        self.changed()
            .iter()
            .filter_map(|r| {
                let changed = |row: &Row| row.kind != RowKind::Same && row.at >= r.start;
                self.rows
                    .iter()
                    .position(changed)
                    .or_else(|| self.rows.iter().position(|row| row.at >= r.start))
            })
            .collect()
    }

    /// The identifier `gd` searches for.
    pub fn word(&self) -> Option<&str> {
        let words = identifiers(&self.rows.get(self.view.cursor)?.text);
        words
            .get(self.view.word.min(words.len().checked_sub(1)?))
            .copied()
    }

    fn move_cursor(&mut self, down: bool) {
        let segs = self.segments();
        let Some(i) = segs.iter().position(|s| s.contains(self.view.cursor)) else {
            return;
        };
        let j = if down {
            (i + 1).min(segs.len() - 1)
        } else {
            i.saturating_sub(1)
        };
        self.view.cursor = segs[j].first();
        self.view.word = 0;
    }

    fn next_change(&mut self, forward: bool) {
        let starts = self.change_rows();
        let cur = self.view.cursor;
        let next = if forward {
            starts.iter().find(|&&s| s > cur).or(starts.first())
        } else {
            starts.iter().rev().find(|&&s| s < cur).or(starts.last())
        };
        if let Some(&s) = next {
            self.view.cursor = s;
            self.view.word = 0;
        }
    }

    fn toggle_fold(&mut self) -> Action {
        let cur = self.view.cursor;
        let Some(f) = self.folds.iter().find(|f| f.contains(&cur)) else {
            return Action::Status("Nothing folded here".into());
        };
        if !self.view.open.remove(&f.start) {
            self.view.open.insert(f.start);
        }
        self.view.cursor = f.start;
        Action::None
    }

    /// Moves the cursor to after line `no` (1-based), opening its fold.
    pub fn show_line(&mut self, no: u32) -> bool {
        let Some(i) = self
            .rows
            .iter()
            .position(|r| r.kind != RowKind::Del && r.no == no)
        else {
            return false;
        };
        if let Some(f) = self.folds.iter().find(|f| f.contains(&i)) {
            self.view.open.insert(f.start);
        }
        self.view.cursor = i;
        self.view.word = 0;
        true
    }
}

/// Identifiers in `text`, in order.
pub fn identifiers(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
        let word = c.is_alphanumeric() || c == '_';
        match (start, word) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                let w = &text[s..i];
                if !w.starts_with(|c: char| c.is_ascii_digit()) {
                    out.push(w);
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// Rows of the file view for `preview`. An unplaced edit is its old text as
/// removed lines and its new text as added ones.
pub fn rows(preview: &EditPreview) -> Vec<Row> {
    match preview {
        EditPreview::Placed { before, after, .. } => diff_rows(before, after),
        EditPreview::Unplaced { old, new, .. } => {
            let line = |kind, (i, text): (usize, &str), at| Row {
                kind,
                text: text.to_string(),
                no: i as u32 + 1,
                at,
            };
            let mut out: Vec<Row> = old
                .lines()
                .enumerate()
                .map(|l| line(RowKind::Del, l, 0))
                .collect();
            out.extend(
                new.lines()
                    .enumerate()
                    .map(|l| line(RowKind::Add, l, l.0 as u32)),
            );
            out
        }
    }
}

fn diff_rows(before: &str, after: &str) -> Vec<Row> {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let prefix = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    let suffix = b[prefix..]
        .iter()
        .rev()
        .zip(a[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let mut out = Vec::with_capacity(a.len());
    let same = |out: &mut Vec<Row>, ai: usize| {
        out.push(Row {
            kind: RowKind::Same,
            text: a[ai].to_string(),
            no: ai as u32 + 1,
            at: ai as u32,
        })
    };
    let dels = |out: &mut Vec<Row>, bs: Range<usize>, at: usize| {
        out.extend(bs.map(|bi| Row {
            kind: RowKind::Del,
            text: b[bi].to_string(),
            no: bi as u32 + 1,
            at: at as u32,
        }))
    };
    let adds = |out: &mut Vec<Row>, r: Range<usize>| {
        out.extend(r.map(|ai| Row {
            kind: RowKind::Add,
            text: a[ai].to_string(),
            no: ai as u32 + 1,
            at: ai as u32,
        }))
    };
    for ai in 0..prefix {
        same(&mut out, ai);
    }
    let (mb, ma) = (prefix..b.len() - suffix, prefix..a.len() - suffix);
    if mb.len().max(ma.len()) <= DIFF_MAX {
        let ops = similar::capture_diff_slices(similar::Algorithm::Myers, &b[mb], &a[ma]);
        for op in ops {
            let (tag, ob, na) = op.as_tag_tuple();
            let (ob, na) = (
                ob.start + prefix..ob.end + prefix,
                na.start + prefix..na.end + prefix,
            );
            match tag {
                similar::DiffTag::Equal => na.for_each(|ai| same(&mut out, ai)),
                similar::DiffTag::Delete => dels(&mut out, ob, na.start),
                similar::DiffTag::Insert => adds(&mut out, na),
                similar::DiffTag::Replace => {
                    dels(&mut out, ob, na.start);
                    adds(&mut out, na);
                }
            }
        }
    } else {
        dels(&mut out, mb, ma.start);
        adds(&mut out, ma);
    }
    for ai in a.len() - suffix..a.len() {
        same(&mut out, ai);
    }
    out
}

/// Runs of changed rows.
pub fn clusters(rows: &[Row]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if r.kind == RowKind::Same {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.end == i => last.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    out
}

/// What a key on a step card asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Status(String),
    Allow,
    Reject,
    Change,
    Search(String),
}

pub fn on_key(card: &mut StepCard, key: KeyEvent) -> Action {
    if let Some(first) = card.view.pending.take() {
        return match (first, key.code) {
            ('z', KeyCode::Char('a')) => card.toggle_fold(),
            ('z', KeyCode::Char('R')) => {
                card.view.open = card.folds.iter().map(|f| f.start).collect();
                Action::None
            }
            ('g', KeyCode::Char('d')) => match card.word() {
                Some(w) => Action::Search(w.to_string()),
                None => Action::Status("No identifier on this line".into()),
            },
            _ => Action::None,
        };
    }
    match key.code {
        KeyCode::Char(c @ ('z' | 'g')) => card.view.pending = Some(c),
        KeyCode::Char('y') => return Action::Allow,
        KeyCode::Char('n') => return Action::Reject,
        KeyCode::Char('e') => return Action::Change,
        KeyCode::Char('m') => card.view.raw = !card.view.raw,
        KeyCode::Tab => card.next_change(true),
        KeyCode::BackTab => card.next_change(false),
        KeyCode::Char('j') | KeyCode::Down => card.move_cursor(true),
        KeyCode::Char('k') | KeyCode::Up => card.move_cursor(false),
        KeyCode::Char('w') => card.view.word += 1,
        KeyCode::Char('b') => card.view.word = card.view.word.saturating_sub(1),
        _ => {}
    }
    // Keep `word` on a real identifier so `b` moves back at once.
    let n = card
        .rows
        .get(card.view.cursor)
        .map_or(0, |r| identifiers(&r.text).len());
    card.view.word = card.view.word.min(n.saturating_sub(1));
    Action::None
}

/// A decided step, for the card's "earlier" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub session_id: String,
    pub step_no: u32,
    pub path: PathBuf,
    pub allowed: bool,
    /// The edit's identity, to match its PostToolUse report.
    pub key: EditKey,
    /// Hash of the file as the allowed edit leaves it (`shell::text_hash`).
    pub after: Option<u64>,
}

/// Step state the app keeps outside the queue.
#[derive(Debug, Default)]
pub struct Steps {
    /// Steps seen so far, per session.
    counts: BTreeMap<String, u32>,
    pub history: Vec<Done>,
    /// Cards decided after all their hooks went away, with when. Claude's
    /// retry of the same edit on the same file gets the decision.
    retry: Vec<(StepCard, Option<Instant>)>,
}

/// `n` or `e` on a step card: typing what Claude reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasonDraft {
    pub card: u64,
    /// `e` (ask for a change) rather than `n` (reject).
    pub change: bool,
    pub input: TextInput,
}

/// `gd`: where an identifier appears in the checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchList {
    pub card: u64,
    pub ident: String,
    /// `None` while the search runs.
    pub hits: Option<Vec<GrepHit>>,
    pub cursor: usize,
}

impl App {
    /// A hook asks about an edit. It is held when the checkout of its cwd
    /// or of its file is in step mode; otherwise the answer is "no decision"
    /// at once. A retry of an edit already decided gets that decision, and a
    /// retry of one still open joins its card, but only while the file is
    /// as the card shows it.
    pub(crate) fn on_pre_tool(&mut self, req_id: u64, e: PendingEdit) -> Vec<Effect> {
        let step = |c: &Option<Checkout>| {
            c.as_ref()
                .is_some_and(|c| self.config.mode_for(c) == Mode::Step)
        };
        let held = step(&e.checkout) || step(&e.file_checkout);
        let checkout = e.file_checkout.clone().or_else(|| e.checkout.clone());
        let Some(checkout) = checkout.filter(|_| held) else {
            return vec![Effect::Reply {
                req_id,
                decision: HookReply::NoDecision,
            }];
        };
        let key = EditKey::new(&e.session_id, &e.tool_name, &e.tool_input);
        if let Some(i) = self.steps.retry.iter().position(|(c, _)| c.key == key) {
            // A decision about another version of the file is dropped.
            let (card, _) = self.steps.retry.remove(i);
            if card.preview == e.preview {
                return vec![Effect::Reply {
                    req_id,
                    decision: card.decided.unwrap_or(HookReply::NoDecision),
                }];
            }
        }
        let open = self
            .queue
            .find_mut(|c| matches!(&c.kind, CardKind::Step(s) if s.key == key));
        let mut req_ids = vec![req_id];
        let mut step_no = None;
        if let Some(Card {
            id,
            kind: CardKind::Step(sc),
            ..
        }) = open
        {
            if sc.preview == e.preview {
                sc.req_ids.push(req_id);
                return Vec::new();
            }
            // The file changed under the open card: a fresh card replaces it.
            let id = *id;
            req_ids.splice(0..0, sc.req_ids.iter().copied());
            step_no = Some(sc.step_no);
            self.queue.remove(id);
        }
        let step_no = step_no.unwrap_or_else(|| {
            let count = self.steps.counts.entry(e.session_id.clone()).or_insert(0);
            *count += 1;
            *count
        });
        let agent = self
            .agents
            .iter()
            .find(|a| a.session_id == e.session_id)
            .map_or_else(
                || AgentId {
                    pane: "?".into(),
                    claude_pid: 0,
                },
                |a| a.id.clone(),
            );
        self.next_card_id += 1;
        let created = e.at;
        let mut card = StepCard::new(req_id, key, step_no, e);
        card.req_ids = req_ids;
        self.queue.push(Card {
            id: self.next_card_id,
            checkout,
            agent,
            created,
            kind: CardKind::Step(Box::new(card)),
        });
        Vec::new()
    }

    /// Drops stored decisions older than `RETRY_FOR`.
    pub(crate) fn expire_step_decisions(&mut self, now: Instant) {
        self.steps.retry.retain_mut(|(_, at)| {
            let at = *at.get_or_insert(now);
            now.saturating_duration_since(at) < RETRY_FOR
        });
    }

    /// Whether card `id` has been on screen long enough to be decided.
    pub(crate) fn looked_at(&self, id: u64) -> bool {
        match (self.shown, self.now) {
            (Some((shown, Some(at))), Some(now)) if shown == id => {
                now.saturating_duration_since(at) >= LOOK_FOR
            }
            _ => false,
        }
    }

    /// A hook gave up waiting. Its card stays for Claude's retry.
    pub(crate) fn on_hook_gone(&mut self, req_id: u64) {
        if self.close_permission(req_id) {
            return;
        }
        let card = self
            .queue
            .find_mut(|c| matches!(&c.kind, CardKind::Step(s) if s.req_ids.contains(&req_id)));
        if let Some(Card {
            kind: CardKind::Step(sc),
            ..
        }) = card
        {
            sc.req_ids.retain(|&r| r != req_id);
        }
    }

    pub(crate) fn on_step_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(card) = self.queue.current_mut() else {
            return Vec::new();
        };
        let (id, checkout) = (card.id, card.checkout.clone());
        let looked = self.looked_at(id);
        let Some(Card {
            kind: CardKind::Step(sc),
            ..
        }) = self.queue.current_mut()
        else {
            return Vec::new();
        };
        let reason = |change| {
            Some(Overlay::StepReason(ReasonDraft {
                card: id,
                change,
                input: TextInput::single_line(),
            }))
        };
        let action = on_key(sc, key);
        if matches!(action, Action::Allow | Action::Reject | Action::Change) && !looked {
            self.set_status("Too quick: look at this step first");
            return Vec::new();
        }
        match action {
            Action::None => {}
            Action::Status(s) => self.set_status(s),
            Action::Allow => return self.decide(id, HookReply::Allow),
            Action::Reject => self.overlay = reason(false),
            Action::Change => self.overlay = reason(true),
            Action::Search(ident) => {
                self.overlay = Some(Overlay::Search(SearchList {
                    card: id,
                    ident: ident.clone(),
                    hits: None,
                    cursor: 0,
                }));
                return vec![Effect::Grep { checkout, ident }];
            }
        }
        Vec::new()
    }

    pub(crate) fn on_reason_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(Overlay::StepReason(draft)) = &mut self.overlay else {
            return Vec::new();
        };
        let text = match draft.input.handle_key(key) {
            InputEvent::Editing => return Vec::new(),
            InputEvent::Cancel => None,
            InputEvent::Submit(text) => Some(text),
        };
        let Some(Overlay::StepReason(draft)) = self.overlay.take() else {
            return Vec::new();
        };
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
            return Vec::new();
        };
        let prefix = if draft.change {
            CHANGE_PREFIX
        } else {
            REJECT_PREFIX
        };
        let reason = format!("{prefix}{}", text.trim());
        self.decide(draft.card, HookReply::Deny { reason })
    }

    pub(crate) fn on_search_key(&mut self, key: KeyEvent) {
        let Some(Overlay::Search(list)) = &mut self.overlay else {
            return;
        };
        let n = list.hits.as_ref().map_or(0, Vec::len);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Char('j') | KeyCode::Down => {
                list.cursor = (list.cursor + 1).min(n.saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up => list.cursor = list.cursor.saturating_sub(1),
            KeyCode::Enter => {
                let Some(hit) = list.hits.as_ref().and_then(|h| h.get(list.cursor)).cloned() else {
                    return;
                };
                let card_id = list.card;
                self.overlay = None;
                let card = self.queue.find_mut(|c| c.id == card_id);
                if let Some(Card {
                    checkout,
                    kind: CardKind::Step(sc),
                    ..
                }) = card
                    && checkout.0.join(&hit.path) == sc.path
                {
                    sc.show_line(hit.line);
                }
                self.set_status(format!("{}:{}", hit.path.display(), hit.line));
            }
            _ => {}
        }
    }

    pub(crate) fn on_grep_done(&mut self, ident: String, result: Result<Vec<GrepHit>, String>) {
        let Some(Overlay::Search(list)) = &mut self.overlay else {
            return;
        };
        if list.ident != ident || list.hits.is_some() {
            return;
        }
        match result {
            Ok(hits) => list.hits = Some(hits),
            Err(e) => {
                self.overlay = None;
                self.set_status(e);
            }
        }
    }

    /// Answers every hook waiting on card `id` and closes it. With no hook
    /// left, the answer waits for Claude's retry.
    fn decide(&mut self, id: u64, reply: HookReply) -> Vec<Effect> {
        let Some(Card {
            kind: CardKind::Step(mut sc),
            ..
        }) = self.queue.remove(id)
        else {
            return Vec::new();
        };
        let (verb, allowed) = match &reply {
            HookReply::Allow => ("Allowed", true),
            HookReply::Deny { reason } if reason.starts_with(CHANGE_PREFIX) => {
                ("Asked for a change to", false)
            }
            _ => ("Rejected", false),
        };
        self.steps.history.push(Done {
            session_id: sc.session_id.clone(),
            step_no: sc.step_no,
            path: sc.path.clone(),
            allowed,
            key: sc.key,
            after: match &sc.preview {
                EditPreview::Placed { after, .. } if allowed => {
                    Some(crate::shell::text_hash(after))
                }
                _ => None,
            },
        });
        let effects: Vec<Effect> = sc
            .req_ids
            .iter()
            .map(|&req_id| Effect::Reply {
                req_id,
                decision: reply.clone(),
            })
            .collect();
        let mut status = format!("{verb} step {}", sc.step_no);
        if effects.is_empty() {
            status.push_str("; Claude gets it when it tries again");
            sc.decided = Some(reply);
            self.steps.retry.push((*sc, self.now));
        }
        self.set_status(status);
        effects
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;
    use crate::app::Msg;
    use crate::config::Config;
    use crate::edit::apply_edit;
    use crate::theme::builtin_themes;

    pub(crate) const FILE: &str = "use std::io;\n\nfn helper() -> u32 {\n    1\n}\n\nfn handle(code: u32) -> u32 {\n    let status = match code {\n        1 => 200,\n        2 => 404,\n        _ => 500,\n    };\n    status\n}\n\nfn other() {\n    a();\n    b();\n    c();\n    d();\n    e();\n    f();\n}\n";

    pub(crate) fn input() -> Value {
        json!({"file_path": "/a/src/http.rs", "old_string": "        2 => 404,\n", "new_string": "        2 => 404,\n        3 => 429,\n"})
    }

    pub(crate) fn pending(session: &str, mode_checkout: &str, input: Value) -> PendingEdit {
        PendingEdit {
            session_id: session.into(),
            checkout: Some(Checkout(mode_checkout.into())),
            file_checkout: Some(Checkout(mode_checkout.into())),
            tool_name: "Edit".into(),
            preview: apply_edit("Edit", &input, Some(FILE)),
            tool_input: input,
            path: "/a/src/http.rs".into(),
            description: Some("Return 429 when a client runs out of tokens.".into()),
            at: SystemTime::UNIX_EPOCH,
        }
    }

    pub(crate) fn step_app() -> App {
        let mut a = App::new(Config::default(), builtin_themes());
        a.config.set_mode(&Checkout("/a".into()), Mode::Step);
        a
    }

    thread_local! {
        static CLOCK: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
    }

    /// Test time: a second later each time it is read.
    pub(crate) fn tick() -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let base = *BASE.get_or_init(Instant::now);
        CLOCK.with(|c| {
            c.set(c.get() + Duration::from_secs(1));
            base + c.get()
        })
    }

    fn pre_tool(a: &mut App, req_id: u64, edit: PendingEdit) -> Vec<Effect> {
        pre_tool_at(a, req_id, edit, tick())
    }

    fn pre_tool_at(a: &mut App, req_id: u64, edit: PendingEdit, at: Instant) -> Vec<Effect> {
        a.update(Msg::PreTool { req_id, edit, at })
    }

    fn key(c: char) -> Msg {
        press(KeyCode::Char(c))
    }

    fn press(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::from(code), tick())
    }

    fn step(a: &App) -> &StepCard {
        match &a.queue.current().expect("a card").kind {
            CardKind::Step(s) => s,
            k => panic!("not a step card: {k:?}"),
        }
    }

    #[test]
    fn non_step_checkout_gets_no_decision() {
        let mut a = step_app();
        let reply = |req_id| {
            vec![Effect::Reply {
                req_id,
                decision: HookReply::NoDecision,
            }]
        };
        assert_eq!(pre_tool(&mut a, 1, pending("s1", "/b", input())), reply(1));
        let mut outside = pending("s1", "/a", input());
        outside.checkout = None;
        outside.file_checkout = None;
        assert_eq!(pre_tool(&mut a, 2, outside), reply(2));
        assert!(a.queue.is_empty());
    }

    #[test]
    fn step_request_waits_on_a_card_until_allowed() {
        let mut a = step_app();
        assert!(pre_tool(&mut a, 1, pending("s1", "/a", input())).is_empty());
        assert_eq!(step(&a).step_no, 1);
        assert_eq!(
            a.update(key('y')),
            [Effect::Reply {
                req_id: 1,
                decision: HookReply::Allow,
            }]
        );
        assert!(a.queue.is_empty());
        assert_eq!(a.status_text(), Some("Allowed step 1"));
        assert!(a.steps.history[0].allowed);
    }

    #[test]
    fn retry_of_approved_edit_is_allowed() {
        let mut a = step_app();
        pre_tool(&mut a, 1, pending("s1", "/a", input()));
        // The hook's deadline passed before the user looked.
        a.update(Msg::HookGone { req_id: 1 });
        assert!(step(&a).req_ids.is_empty(), "the card stays");
        assert!(a.update(key('y')).is_empty(), "nobody to answer yet");
        assert!(a.queue.is_empty());
        // Claude tries the same edit again, with its keys in another order.
        let mut retry = pending("s1", "/a", input());
        retry.tool_input = json!({"new_string": input()["new_string"], "old_string": input()["old_string"], "file_path": "/a/src/http.rs"});
        assert_eq!(
            pre_tool(&mut a, 2, retry),
            [Effect::Reply {
                req_id: 2,
                decision: HookReply::Allow,
            }]
        );
        assert!(a.queue.is_empty());
        // A third try is a new edit again.
        assert!(pre_tool(&mut a, 3, pending("s1", "/a", input())).is_empty());
        assert_eq!(step(&a).step_no, 2);
    }

    /// `pending` with the file one line longer than `FILE`, so the same
    /// edit lands on a different file.
    fn pending_on_changed_file(session: &str) -> PendingEdit {
        let mut e = pending(session, "/a", input());
        e.preview = apply_edit("Edit", &input(), Some(&format!("// new\n{FILE}")));
        e
    }

    #[test]
    fn changed_file_makes_a_new_card() {
        let mut a = step_app();
        pre_tool(&mut a, 1, pending("s1", "/a", input()));
        a.update(Msg::HookGone { req_id: 1 });
        a.update(key('y'));
        // The stored Allow was for another file; it is not reused.
        assert!(pre_tool(&mut a, 2, pending_on_changed_file("s1")).is_empty());
        assert_eq!(step(&a).req_ids, [2]);
        assert_eq!(step(&a).step_no, 2);

        // An open card whose file changed is replaced, keeping its hooks.
        assert!(pre_tool(&mut a, 3, pending("s1", "/a", input())).is_empty());
        assert_eq!(a.queue.len(), 1);
        assert_eq!(step(&a).req_ids, [2, 3]);
        assert_eq!(step(&a).step_no, 2);
        assert_eq!(step(&a).preview, pending("s1", "/a", input()).preview);
    }

    #[test]
    fn stored_decision_expires_after_ten_minutes() {
        let mut a = step_app();
        pre_tool(&mut a, 1, pending("s1", "/a", input()));
        a.update(Msg::HookGone { req_id: 1 });
        let decided = tick();
        a.update(Msg::Key(KeyEvent::from(KeyCode::Char('y')), decided));
        a.update(Msg::Tick(decided + Duration::from_secs(9 * 60)));
        assert_eq!(a.steps.retry.len(), 1, "still kept at 9 minutes");
        a.update(Msg::Tick(decided + RETRY_FOR));
        assert!(a.steps.retry.is_empty());
        let late = decided + RETRY_FOR + Duration::from_secs(1);
        assert!(pre_tool_at(&mut a, 2, pending("s1", "/a", input()), late).is_empty());
        assert_eq!(step(&a).req_ids, [2], "a new card, no stale answer");
    }

    #[test]
    fn decision_keys_wait_until_the_card_was_seen() {
        let mut a = step_app();
        let t = tick();
        pre_tool_at(&mut a, 1, pending("s1", "/a", input()), t);
        let mut other = input();
        other["new_string"] = json!("z");
        pre_tool_at(&mut a, 2, pending("s1", "/a", other), t);
        let at = |ms| {
            Msg::Key(
                KeyEvent::from(KeyCode::Char('y')),
                t + Duration::from_millis(ms),
            )
        };
        assert!(a.update(at(100)).is_empty(), "too soon after it appeared");
        assert_eq!(a.status_text(), Some("Too quick: look at this step first"));
        assert_eq!(a.update(at(400)).len(), 1);
        // A second `y` typed straight after lands on the next card: ignored.
        assert!(a.update(at(450)).is_empty());
        assert_eq!(step(&a).req_ids, [2]);
        assert_eq!(
            a.update(at(700)),
            [Effect::Reply {
                req_id: 2,
                decision: HookReply::Allow,
            }]
        );
    }

    #[test]
    fn edit_into_a_step_checkout_from_elsewhere_is_held() {
        let mut a = step_app();
        // The agent runs in /b (review mode) and edits a file in /a (step).
        let mut e = pending("s1", "/b", input());
        e.file_checkout = Some(Checkout("/a".into()));
        assert!(pre_tool(&mut a, 1, e).is_empty());
        assert_eq!(a.queue.current().unwrap().checkout, Checkout("/a".into()));
        // In /a, editing a file outside any checkout: still held.
        let mut e = pending("s2", "/a", input());
        e.file_checkout = None;
        assert!(pre_tool(&mut a, 2, e).is_empty());
        assert_eq!(a.queue.len(), 2);
        // Neither in step mode: no decision.
        let mut e = pending("s3", "/b", input());
        e.file_checkout = Some(Checkout("/c".into()));
        assert_eq!(
            pre_tool(&mut a, 3, e),
            [Effect::Reply {
                req_id: 3,
                decision: HookReply::NoDecision,
            }]
        );
    }

    #[test]
    fn retry_of_open_edit_joins_its_card() {
        let mut a = step_app();
        pre_tool(&mut a, 1, pending("s1", "/a", input()));
        a.update(Msg::HookGone { req_id: 1 });
        assert!(pre_tool(&mut a, 2, pending("s1", "/a", input())).is_empty());
        assert_eq!(a.queue.len(), 1);
        assert_eq!(step(&a).req_ids, [2]);
        // Another session making the same edit is a separate card.
        pre_tool(&mut a, 3, pending("s2", "/a", input()));
        assert_eq!(a.queue.len(), 2);
    }

    #[test]
    fn reject_and_change_send_the_reason() {
        let mut a = step_app();
        pre_tool(&mut a, 1, pending("s1", "/a", input()));
        a.update(key('n'));
        assert!(matches!(a.overlay, Some(Overlay::StepReason(_))));
        for c in "no 429 here".chars() {
            a.update(key(c));
        }
        assert_eq!(
            a.update(press(KeyCode::Enter)),
            [Effect::Reply {
                req_id: 1,
                decision: HookReply::Deny {
                    reason: "Rejected by reviewer: no 429 here".into()
                },
            }]
        );
        assert!(!a.steps.history[0].allowed);

        let mut other = input();
        other["new_string"] = json!("x");
        pre_tool(&mut a, 2, pending("s1", "/a", other));
        a.update(key('e'));
        for c in "use 503".chars() {
            a.update(key(c));
        }
        assert_eq!(
            a.update(press(KeyCode::Enter)),
            [Effect::Reply {
                req_id: 2,
                decision: HookReply::Deny {
                    reason: "Please change this edit instead: use 503".into()
                },
            }]
        );
        assert_eq!(a.status_text(), Some("Asked for a change to step 2"));
    }

    #[test]
    fn step_cards_of_one_checkout_come_one_at_a_time() {
        let mut a = step_app();
        a.config.set_mode(&Checkout("/b".into()), Mode::Step);
        pre_tool(&mut a, 1, pending("s1", "/a", input()));
        let mut b = pending("s2", "/b", input());
        b.at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        pre_tool(&mut a, 2, b);
        let mut second = input();
        second["new_string"] = json!("y");
        let mut a2 = pending("s1", "/a", second);
        a2.at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2);
        pre_tool(&mut a, 3, a2);
        // /a's two steps first, then /b's.
        let reqs: Vec<Vec<u64>> = a
            .queue
            .iter()
            .map(|c| match &c.kind {
                CardKind::Step(s) => s.req_ids.clone(),
                k => panic!("{k:?}"),
            })
            .collect();
        assert_eq!(reqs, [[1], [3], [2]]);
        // Only the card shown is answered; the others' hooks keep waiting.
        assert_eq!(
            a.update(key('y')),
            [Effect::Reply {
                req_id: 1,
                decision: HookReply::Allow,
            }]
        );
        assert_eq!(a.queue.len(), 2);
    }

    #[test]
    fn keys_move_fold_and_search() {
        let mut a = step_app();
        pre_tool(&mut a, 1, pending("s1", "/a", input()));
        let s = step(&a);
        // The cursor starts on the added line, inside handle().
        assert_eq!(s.rows[s.view.cursor].text, "        3 => 429,");
        // The match block and three lines either side stay; the rest folds.
        assert_eq!(s.folds, [0..4, 16..24]);
        a.update(key('k'));
        assert_eq!(step(&a).word(), None, "numbers are not identifiers");
        a.update(key('k'));
        a.update(key('k'));
        a.update(key('w'));
        assert_eq!(step(&a).word(), Some("status"));
        for _ in 0..10 {
            a.update(key('k'));
        }
        assert_eq!(step(&a).view.cursor, 0, "on the first fold");
        a.update(key('z'));
        a.update(key('a'));
        assert!(step(&a).view.open.contains(&0));
        a.update(key('w'));
        assert_eq!(step(&a).word(), Some("std"));
        a.update(key('g'));
        assert_eq!(
            a.update(key('d')),
            [Effect::Grep {
                checkout: Checkout("/a".into()),
                ident: "std".into()
            }]
        );
        a.update(Msg::GrepDone {
            ident: "std".into(),
            result: Ok(vec![GrepHit {
                path: "src/http.rs".into(),
                line: 14,
                text: "}".into(),
            }]),
        });
        a.update(press(KeyCode::Enter));
        assert_eq!(a.overlay, None);
        assert_eq!(a.status_text(), Some("src/http.rs:14"));
        assert_eq!(step(&a).rows[step(&a).view.cursor].no, 14);
        a.update(press(KeyCode::Tab));
        assert_eq!(
            step(&a).rows[step(&a).view.cursor].text,
            "        3 => 429,"
        );
        a.update(key('z'));
        a.update(key('R'));
        assert_eq!(step(&a).view.open.len(), 2);
    }

    #[test]
    fn edit_key_ignores_key_order_and_sees_values() {
        let a = json!({"a": 1, "b": {"c": [1, {"d": 2, "e": 3}]}});
        let b = json!({"b": {"c": [1, {"e": 3, "d": 2}]}, "a": 1});
        assert_eq!(EditKey::new("s", "Edit", &a), EditKey::new("s", "Edit", &b));
        assert_ne!(EditKey::new("s", "Edit", &a), EditKey::new("t", "Edit", &a));
        assert_ne!(
            EditKey::new("s", "Edit", &a),
            EditKey::new("s", "Write", &a)
        );
        assert_ne!(
            EditKey::new("s", "Edit", &a),
            EditKey::new("s", "Edit", &json!({"a": 2}))
        );
    }

    #[test]
    fn rows_show_removed_lines_where_they_were() {
        let p = apply_edit(
            "Edit",
            &json!({"old_string": "1 => 200", "new_string": "1 => 201"}),
            Some(FILE),
        );
        let rows = rows(&p);
        let changed: Vec<(RowKind, u32, &str)> = rows
            .iter()
            .filter(|r| r.kind != RowKind::Same)
            .map(|r| (r.kind, r.no, r.text.as_str()))
            .collect();
        assert_eq!(
            changed,
            [
                (RowKind::Del, 9, "        1 => 200,"),
                (RowKind::Add, 9, "        1 => 201,"),
            ]
        );
    }
}
