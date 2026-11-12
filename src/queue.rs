use crate::model::{Card, CardKind};

/// Cards waiting for the user, in the order they will be shown.
///
/// Order (spec call 11): permission cards and option menus first, since a
/// session is stopped mid-turn on each, oldest first. Then each
/// checkout holding a step or shell-change card, one checkout at a time, the
/// one with the oldest such card first, with all its cards. Then everything
/// else, oldest first. A skipped card goes behind every card of its group;
/// one sent to the back goes behind every card, whatever its group.
///
/// That order applies only behind the pinned card: once a card is on screen
/// (`pin`), it stays first until it is removed or skipped, so a new card
/// never takes the screen from it.
#[derive(Debug, Default)]
pub struct Queue {
    entries: Vec<Entry>,
    skips: u64,
    /// Id of the card on screen.
    pinned: Option<u64>,
}

#[derive(Debug)]
struct Entry {
    card: Card,
    /// 0 until skipped; then the skip count at that moment, so later skips sort later.
    skipped: u64,
    /// 0 until sent to the back; then the skip count at that moment.
    back: u64,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, card: Card) {
        self.entries.push(Entry {
            card,
            skipped: 0,
            back: 0,
        });
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn current(&self) -> Option<&Card> {
        self.order().first().map(|&i| &self.entries[i].card)
    }

    pub fn current_mut(&mut self) -> Option<&mut Card> {
        let i = *self.order().first()?;
        Some(&mut self.entries[i].card)
    }

    pub fn remove(&mut self, id: u64) -> Option<Card> {
        let i = self.entries.iter().position(|e| e.card.id == id)?;
        Some(self.entries.remove(i).card)
    }

    /// Puts `card` in place of card `old`, keeping its place on screen.
    pub fn replace(&mut self, old: u64, card: Card) {
        self.remove(old);
        if self.pinned == Some(old) {
            self.pinned = Some(card.id);
        }
        self.push(card);
    }

    /// Keeps the current card first until it is removed or skipped. Called
    /// once the card is on screen.
    pub fn pin(&mut self) {
        self.pinned = self.current().map(|c| c.id);
    }

    /// Moves the current card to the back of its group.
    pub fn skip(&mut self) {
        if let Some(&i) = self.order().first() {
            self.skips += 1;
            self.entries[i].skipped = self.skips;
            self.pinned = None;
        }
    }

    /// Moves the current card behind every card in the queue.
    pub fn to_back(&mut self) {
        if let Some(&i) = self.order().first() {
            self.skips += 1;
            self.entries[i].back = self.skips;
            self.pinned = None;
        }
    }

    /// Cards in display order.
    pub fn iter(&self) -> impl Iterator<Item = &Card> {
        self.order().into_iter().map(|i| &self.entries[i].card)
    }

    pub fn cards_mut(&mut self) -> impl Iterator<Item = &mut Card> {
        self.entries.iter_mut().map(|e| &mut e.card)
    }

    pub fn find_mut(&mut self, mut pred: impl FnMut(&Card) -> bool) -> Option<&mut Card> {
        self.entries
            .iter_mut()
            .map(|e| &mut e.card)
            .find(|c| pred(c))
    }

    /// Indices into `entries` in display order.
    fn order(&self) -> Vec<usize> {
        let mut by_age: Vec<usize> = (0..self.entries.len()).collect();
        by_age.sort_by_key(|&i| {
            let e = &self.entries[i];
            (e.skipped, e.card.created, e.card.id)
        });
        let card = |i: usize| &self.entries[i].card;
        let (mut out, mut rest): (Vec<usize>, Vec<usize>) = by_age
            .into_iter()
            .partition(|&i| matches!(card(i).kind, CardKind::Permission(_) | CardKind::Choice(_)));
        while let Some(&first) = rest
            .iter()
            .find(|&&i| matches!(card(i).kind, CardKind::Step(_) | CardKind::ShellChange(_)))
        {
            let checkout = &card(first).checkout;
            let (this, others): (Vec<usize>, Vec<usize>) = rest
                .into_iter()
                .partition(|&i| &card(i).checkout == checkout);
            out.extend(this);
            rest = others;
        }
        out.extend(rest);
        // Cards sent to the back, the latest last. A sort keeps the rest in order.
        out.sort_by_key(|&i| self.entries[i].back);
        if let Some(at) = self
            .pinned
            .and_then(|id| out.iter().position(|&i| card(i).id == id))
        {
            let i = out.remove(at);
            out.insert(0, i);
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::model::{AgentId, Checkout, PermissionCard, ReviewCard, ShellCard};

    pub(crate) fn card(id: u64, checkout: &str, kind: CardKind, age_secs: u64) -> Card {
        Card {
            id,
            checkout: Checkout(checkout.into()),
            agent: AgentId {
                pane: format!("%{id}"),
                claude_pid: id as u32,
            },
            created: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_secs),
            kind,
        }
    }

    fn review() -> CardKind {
        CardKind::Review(ReviewCard::default())
    }
    fn step() -> CardKind {
        CardKind::Step(Box::default())
    }

    fn ids(q: &Queue) -> Vec<u64> {
        q.iter().map(|c| c.id).collect()
    }

    #[test]
    fn to_back_goes_behind_every_group() {
        let mut q = Queue::new();
        q.push(card(
            1,
            "/a",
            CardKind::Permission(PermissionCard::default()),
            10,
        ));
        q.push(card(2, "/b", step(), 30));
        q.push(card(3, "/c", review(), 20));
        q.pin();
        assert_eq!(ids(&q), [1, 2, 3]);
        q.to_back();
        assert_eq!(ids(&q), [2, 3, 1], "a permission, behind a review");
        q.to_back();
        assert_eq!(ids(&q), [3, 1, 2], "the latest sent last");
        // A new card comes in ahead of those sent back.
        q.push(card(4, "/d", review(), 0));
        assert_eq!(ids(&q), [3, 4, 1, 2]);
    }

    #[test]
    fn oldest_first() {
        let mut q = Queue::new();
        q.push(card(1, "/a", review(), 10));
        q.push(card(2, "/b", review(), 30));
        q.push(card(3, "/c", review(), 20));
        assert_eq!(ids(&q), [2, 3, 1]);
        assert_eq!(q.current().unwrap().id, 2);
    }

    #[test]
    fn pinned_card_stays_first() {
        let mut q = Queue::new();
        q.push(card(1, "/a", review(), 10));
        q.pin();
        q.push(card(2, "/b", review(), 30));
        q.push(card(
            3,
            "/c",
            CardKind::Permission(PermissionCard::default()),
            0,
        ));
        q.push(card(4, "/d", step(), 0));
        // The new cards keep their order behind the one on screen.
        assert_eq!(ids(&q), [1, 3, 4, 2]);
        q.pin();
        assert_eq!(q.current().unwrap().id, 1);
        // Once it goes, the next in order comes up and is pinned in turn.
        q.remove(1);
        q.pin();
        q.push(card(
            5,
            "/e",
            CardKind::Permission(PermissionCard::default()),
            60,
        ));
        assert_eq!(ids(&q), [3, 5, 4, 2]);
    }

    #[test]
    fn skip_unpins() {
        let mut q = Queue::new();
        q.push(card(1, "/a", review(), 10));
        q.pin();
        q.push(card(2, "/b", review(), 30));
        q.skip();
        assert_eq!(ids(&q), [2, 1]);
    }

    #[test]
    fn permission_cards_jump_ahead() {
        let mut q = Queue::new();
        q.push(card(1, "/a", review(), 50));
        q.push(card(2, "/b", step(), 40));
        q.push(card(
            3,
            "/c",
            CardKind::Permission(PermissionCard::default()),
            5,
        ));
        q.push(card(
            4,
            "/a",
            CardKind::Permission(PermissionCard::default()),
            10,
        ));
        assert_eq!(ids(&q), [4, 3, 2, 1]);
    }

    #[test]
    fn step_checkout_blocks_others() {
        let mut q = Queue::new();
        // The review card is the oldest of all, yet waits behind both step checkouts.
        q.push(card(1, "/r", review(), 100));
        q.push(card(2, "/b", step(), 50));
        q.push(card(3, "/a", step(), 60));
        q.push(card(4, "/b", step(), 40));
        q.push(card(
            5,
            "/a",
            CardKind::ShellChange(ShellCard::default()),
            30,
        ));
        assert_eq!(ids(&q), [3, 5, 2, 4, 1]);
    }

    #[test]
    fn step_checkout_brings_its_other_cards() {
        let mut q = Queue::new();
        q.push(card(1, "/r", review(), 100));
        q.push(card(2, "/a", review(), 90));
        q.push(card(3, "/a", step(), 10));
        assert_eq!(ids(&q), [2, 3, 1]);
    }

    #[test]
    fn skip_moves_to_back() {
        let mut q = Queue::new();
        q.push(card(1, "/a", review(), 30));
        q.push(card(2, "/b", review(), 20));
        q.push(card(3, "/c", review(), 10));
        q.skip();
        assert_eq!(ids(&q), [2, 3, 1]);
        q.skip();
        assert_eq!(ids(&q), [3, 1, 2]);
        // A newly pushed card goes ahead of skipped ones.
        q.push(card(4, "/d", review(), 0));
        assert_eq!(ids(&q), [3, 4, 1, 2]);
    }

    #[test]
    fn skip_on_empty_is_harmless() {
        let mut q = Queue::new();
        q.skip();
        assert!(q.current().is_none());
    }

    #[test]
    fn remove_and_find_mut() {
        let mut q = Queue::new();
        q.push(card(1, "/a", review(), 30));
        q.push(card(2, "/b", review(), 20));
        assert_eq!(q.remove(1).unwrap().id, 1);
        assert!(q.remove(1).is_none());
        assert_eq!(q.current().unwrap().id, 2);
        q.find_mut(|c| c.id == 2).unwrap().checkout = Checkout("/z".into());
        assert_eq!(q.current_mut().unwrap().checkout, Checkout("/z".into()));
        assert!(q.find_mut(|c| c.id == 9).is_none());
        assert_eq!(q.len(), 1);
    }
}
