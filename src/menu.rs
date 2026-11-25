//! Menu cards: a menu of Claude Code's own on a session's screen, such as
//! whether to trust a folder, shown in aq so it can be answered without
//! going to the window. A number key picks, or Enter picks the option the
//! arrows moved to; the runtime moves the cursor in the pane and presses
//! Enter. The card goes once the menu leaves the
//! screen, whoever answered it. Pure: no I/O.

use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent};

use crate::agents::{Agent, ScreenMenu};
use crate::app::{App, Effect, Overlay};
use crate::model::{Card, CardKind, Checkout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuCard {
    pub menu: ScreenMenu,
    /// The option Enter picks; up and down move it.
    pub sel: usize,
    /// A pick was sent; the card waits for the menu to leave the screen.
    pub picked: bool,
}

impl App {
    /// Makes a card for each menu that showed up, keeps each card's menu as
    /// the screen has it now, and drops the cards whose menu is gone. A plan
    /// menu in a pane whose plan was just approved on its card is answered
    /// yes, with no card: the picks are returned.
    pub(crate) fn note_menus(&mut self, agents: &[Agent]) -> Vec<Effect> {
        let mut picks = Vec::new();
        self.plans_approved.retain(|_, polls| {
            *polls = polls.saturating_sub(1);
            *polls > 0
        });
        // A folder trusted in its own window is no longer asked about: keys
        // sent for it would land in the session's input box.
        self.pending_trust.retain(|(pane, _)| {
            !agents
                .iter()
                .any(|a| a.id.pane == *pane && a.menu.is_none())
        });
        if self.pending_trust.is_empty() && self.overlay == Some(Overlay::Trust) {
            self.overlay = None;
        }
        self.menus_in_window.retain(|pane| {
            agents
                .iter()
                .any(|a| a.id.pane == *pane && a.menu.is_some())
        });
        let gone: Vec<u64> = self
            .queue
            .iter()
            .filter(|c| matches!(c.kind, CardKind::Menu(_)))
            .filter(|c| !agents.iter().any(|a| a.id == c.agent && a.menu.is_some()))
            .map(|c| c.id)
            .collect();
        for id in gone {
            self.queue.remove(id);
        }
        for a in agents {
            let Some(menu) = &a.menu else { continue };
            let answers_it = |k: &CardKind| {
                matches!(
                    k,
                    CardKind::Menu(_) | CardKind::Permission(_) | CardKind::Choice(_)
                )
            };
            if let Some(card) = self
                .queue
                .cards_mut()
                .find(|c| c.agent == a.id && answers_it(&c.kind))
            {
                if let CardKind::Menu(mc) = &mut card.kind {
                    if mc.menu.options != menu.options || mc.menu.question != menu.question {
                        mc.picked = false;
                        mc.sel = menu.cursor;
                    }
                    mc.menu = menu.clone();
                }
                // Any other card of this session's (a permission or a
                // choice) is how this menu is already answered.
                continue;
            }
            if self.menus_in_window.contains(&a.id.pane) {
                continue;
            }
            if self.plans_approved.contains_key(&a.id.pane)
                && let Some(choice) = plan_yes(menu)
            {
                self.plans_approved.remove(&a.id.pane);
                self.set_status(format!("Plan approved: picked {}", menu.options[choice]));
                picks.push(Effect::PickMenu {
                    pane: a.id.pane.clone(),
                    menu: menu.clone(),
                    choice,
                });
                continue;
            }
            // A folder aq started a session in is asked about in the trust box.
            if self
                .pending_trust
                .iter()
                .any(|(pane, _)| *pane == a.id.pane)
            {
                continue;
            }
            self.next_card_id += 1;
            self.queue.push(Card {
                id: self.next_card_id,
                checkout: a.checkout.clone().unwrap_or_else(|| Checkout::new(&a.cwd)),
                agent: a.id.clone(),
                // The session can do nothing until it is answered.
                created: SystemTime::UNIX_EPOCH,
                kind: CardKind::Menu(MenuCard {
                    menu: menu.clone(),
                    sel: menu.cursor,
                    picked: false,
                }),
            });
        }
        picks
    }

    /// 1 to 9 picks that option; up and down (or j and k) move to one and
    /// Enter picks it; g goes to the session's window; x leaves
    /// the menu to be answered there.
    pub(crate) fn on_menu_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(card) = self.queue.current_mut() else {
            return Vec::new();
        };
        let pane = card.agent.pane.clone();
        let CardKind::Menu(mc) = &mut card.kind else {
            return Vec::new();
        };
        let choice = match key.code {
            KeyCode::Char(c @ '1'..='9') => c as usize - '1' as usize,
            KeyCode::Enter => mc.sel,
            KeyCode::Down | KeyCode::Char('j') => {
                mc.sel = (mc.sel + 1).min(mc.menu.options.len().saturating_sub(1));
                return Vec::new();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                mc.sel = mc.sel.saturating_sub(1);
                return Vec::new();
            }
            KeyCode::Char('g') => return vec![Effect::GoToWindow { pane }],
            KeyCode::Char('x') => {
                let id = card.id;
                self.queue.remove(id);
                self.menus_in_window.insert(pane);
                self.set_status("Left the menu in the session's window");
                return Vec::new();
            }
            _ => return Vec::new(),
        };
        if mc.picked || choice >= mc.menu.options.len() {
            return Vec::new();
        }
        mc.picked = true;
        let menu = mc.menu.clone();
        self.set_status(format!("Picked {}", menu.options[choice]));
        vec![Effect::PickMenu { pane, menu, choice }]
    }

    /// The pick did not land: the card takes keys again.
    pub(crate) fn on_menu_pick_failed(&mut self, pane: &str, error: String) {
        for card in self.queue.cards_mut().filter(|c| c.agent.pane == pane) {
            if let CardKind::Menu(mc) = &mut card.kind {
                mc.picked = false;
            }
        }
        self.set_status(format!("Could not pick: {error}"));
    }
}

/// The option that approves a plan in Claude Code's plan menu ("Would you
/// like to proceed?"): yes with auto mode, else the first yes.
fn plan_yes(menu: &ScreenMenu) -> Option<usize> {
    let is_plan = menu
        .options
        .iter()
        .any(|o| o.contains("Tell Claude what to change"));
    let o = &menu.options;
    is_plan
        .then(|| {
            o.iter()
                .position(|o| o.contains("Yes, and use auto mode"))
                .or_else(|| o.iter().position(|o| o.contains("Yes")))
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::app::tests::{agent, app, key};

    fn trust() -> ScreenMenu {
        ScreenMenu {
            question: vec!["Accessing workspace:".into(), "/w/api".into()],
            options: vec!["No, exit".into(), "Yes, I trust this folder".into()],
            cursor: 0,
        }
    }

    fn asking(menu: Option<ScreenMenu>) -> Agent {
        let mut a = agent("%1", 1, "api", "/w/api", false);
        a.menu = menu;
        a
    }

    fn menu_card(a: &App) -> Option<&MenuCard> {
        a.queue.iter().find_map(|c| match &c.kind {
            CardKind::Menu(mc) => Some(mc),
            _ => None,
        })
    }

    #[test]
    fn a_menu_on_screen_becomes_a_card_and_a_number_picks() {
        let mut a = app();
        a.update(Msg::Agents(vec![asking(Some(trust()))]));
        assert!(matches!(
            a.queue.current().map(|c| &c.kind),
            Some(CardKind::Menu(_))
        ));
        assert!(a.update(key('3')).is_empty(), "there is no option 3");
        let effects = a.update(key('2'));
        assert_eq!(
            effects,
            [Effect::PickMenu {
                pane: "%1".into(),
                menu: trust(),
                choice: 1
            }]
        );
        assert!(a.update(key('2')).is_empty(), "picked once");
        // The menu leaves the screen: the card goes.
        a.update(Msg::Agents(vec![asking(None)]));
        assert_eq!(menu_card(&a), None);
    }

    #[test]
    fn the_arrows_move_and_enter_picks() {
        let mut a = app();
        a.update(Msg::Agents(vec![asking(Some(trust()))]));
        assert!(
            a.update(Msg::Key(
                KeyEvent::from(KeyCode::Down),
                std::time::Instant::now()
            ))
            .is_empty()
        );
        assert!(
            a.update(Msg::Key(
                KeyEvent::from(KeyCode::Down),
                std::time::Instant::now()
            ))
            .is_empty()
        );
        assert_eq!(menu_card(&a).unwrap().sel, 1, "stops at the last option");
        a.update(Msg::Key(
            KeyEvent::from(KeyCode::Up),
            std::time::Instant::now(),
        ));
        a.update(key('j'));
        let effects = a.update(Msg::Key(
            KeyEvent::from(KeyCode::Enter),
            std::time::Instant::now(),
        ));
        assert_eq!(
            effects,
            [Effect::PickMenu {
                pane: "%1".into(),
                menu: trust(),
                choice: 1
            }]
        );
    }

    fn plan_menu() -> ScreenMenu {
        ScreenMenu {
            question: vec!["Would you like to proceed?".into()],
            options: vec![
                "1. Yes, and use auto mode".into(),
                "2. Yes, manually approve edits".into(),
                "3. Tell Claude what to change".into(),
            ],
            cursor: 0,
        }
    }

    #[test]
    fn a_plan_approved_on_its_card_is_answered_yes_in_claudes_menu() {
        let mut a = app();
        a.plans_approved.insert("%1".into(), 15);
        let effects = a.update(Msg::Agents(vec![asking(Some(plan_menu()))]));
        assert!(effects.contains(&Effect::PickMenu {
            pane: "%1".into(),
            menu: plan_menu(),
            choice: 0,
        }));
        assert_eq!(menu_card(&a), None, "no card for it");
        assert!(a.plans_approved.is_empty());
        // Not approved in aq: the menu is a card as before.
        let mut b = app();
        b.update(Msg::Agents(vec![asking(Some(plan_menu()))]));
        assert!(menu_card(&b).is_some());
        // Any other menu is never answered for you.
        let mut c = app();
        c.plans_approved.insert("%1".into(), 15);
        c.update(Msg::Agents(vec![asking(Some(trust()))]));
        assert!(menu_card(&c).is_some());
    }

    #[test]
    fn an_approved_plan_waits_only_a_few_polls_for_its_menu() {
        let mut a = app();
        a.plans_approved.insert("%1".into(), 2);
        a.update(Msg::Agents(vec![asking(None)]));
        a.update(Msg::Agents(vec![asking(None)]));
        assert!(a.plans_approved.is_empty());
        a.update(Msg::Agents(vec![asking(Some(plan_menu()))]));
        assert!(menu_card(&a).is_some());
    }

    #[test]
    fn x_leaves_a_menu_in_the_window_until_it_goes() {
        let mut a = app();
        a.update(Msg::Agents(vec![asking(Some(trust()))]));
        assert!(a.update(key('x')).is_empty());
        assert_eq!(menu_card(&a), None);
        a.update(Msg::Agents(vec![asking(Some(trust()))]));
        assert_eq!(menu_card(&a), None, "not taken back");
        // Once it has left the screen, the next menu gets a card again.
        a.update(Msg::Agents(vec![asking(None)]));
        a.update(Msg::Agents(vec![asking(Some(trust()))]));
        assert!(menu_card(&a).is_some());
    }

    #[test]
    fn a_failed_pick_can_be_tried_again() {
        let mut a = app();
        a.update(Msg::Agents(vec![asking(Some(trust()))]));
        a.update(key('2'));
        a.update(Msg::MenuPickFailed {
            pane: "%1".into(),
            error: "the menu is no longer on its screen".into(),
        });
        assert_eq!(
            a.status_text(),
            Some("Could not pick: the menu is no longer on its screen")
        );
        assert_eq!(a.update(key('1')).len(), 1);
    }

    #[test]
    fn one_card_per_menu_kept_up_to_date() {
        let mut a = app();
        a.update(Msg::Agents(vec![asking(Some(trust()))]));
        let mut moved = trust();
        moved.cursor = 1;
        a.update(Msg::Agents(vec![asking(Some(moved))]));
        assert_eq!(a.queue.len(), 1);
        assert_eq!(menu_card(&a).unwrap().menu.cursor, 1);
    }

    #[test]
    fn a_folder_in_the_trust_box_gets_no_card_too() {
        let mut a = app();
        a.update(Msg::TrustAsked {
            pane: "%1".into(),
            dir: "/w/api".into(),
        });
        a.update(Msg::Agents(vec![asking(Some(trust()))]));
        assert_eq!(menu_card(&a), None);
        // Answered in its own window: the trust box lets it go.
        a.update(Msg::Agents(vec![asking(None)]));
        assert!(a.pending_trust.is_empty());
        assert_eq!(a.overlay, None);
        assert!(a.update(key('y')).is_empty(), "no keys sent to it");
    }
}
