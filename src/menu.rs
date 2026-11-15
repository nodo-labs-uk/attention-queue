//! Menu cards: a menu of Claude Code's own on a session's screen, such as
//! whether to trust a folder, shown in aq so it can be answered without
//! going to the window. A number key picks; the runtime moves the cursor in
//! the pane and presses Enter. The card goes once the menu leaves the
//! screen, whoever answered it. Pure: no I/O.

use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent};

use crate::agents::{Agent, ScreenMenu};
use crate::app::{App, Effect};
use crate::model::{Card, CardKind, Checkout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuCard {
    pub menu: ScreenMenu,
    /// A pick was sent; the card waits for the menu to leave the screen.
    pub picked: bool,
}

impl App {
    /// Makes a card for each menu that showed up, keeps each card's menu as
    /// the screen has it now, and drops the cards whose menu is gone.
    pub(crate) fn note_menus(&mut self, agents: &[Agent]) {
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
                    }
                    mc.menu = menu.clone();
                }
                // Any other card of this session's (a permission or a
                // choice) is how this menu is already answered.
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
                    picked: false,
                }),
            });
        }
    }

    /// 1 to 9 picks that option; g goes to the session's window.
    pub(crate) fn on_menu_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(card) = self.queue.current_mut() else {
            return Vec::new();
        };
        let pane = card.agent.pane.clone();
        let CardKind::Menu(mc) = &mut card.kind else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Char(c @ '1'..='9') => {
                let choice = c as usize - '1' as usize;
                if mc.picked || choice >= mc.menu.options.len() {
                    return Vec::new();
                }
                mc.picked = true;
                let menu = mc.menu.clone();
                self.set_status(format!("Picked {}", menu.options[choice]));
                vec![Effect::PickMenu { pane, menu, choice }]
            }
            KeyCode::Char('g') => vec![Effect::GoToWindow { pane }],
            _ => Vec::new(),
        }
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
    }
}
