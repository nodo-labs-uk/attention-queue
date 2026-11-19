//! Decides, from an action and the facts about its target, whether the action
//! is cleared, flagged, or too unclear to place (grey, for Haiku to judge).

use super::FlagKind;
use super::candidate::{Action, IssueVerb, PrVerb};
use super::facts::{Facts, IssueFacts, Me, PrFacts, PushFacts};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Cleared,
    Flagged { kind: FlagKind, label: &'static str },
    Grey,
}

const AS_YOU: Decision = Decision::Flagged {
    kind: FlagKind::SomeoneElse,
    label: "AS YOU",
};
const SOMEONE_ELSE: Decision = Decision::Flagged {
    kind: FlagKind::SomeoneElse,
    label: "SOMEONE ELSE",
};

const fn high(label: &'static str) -> Decision {
    Decision::Flagged {
        kind: FlagKind::HighStakes,
        label,
    }
}

/// Applies the rules table to one action.
pub fn decide(action: &Action, facts: &Facts, me: &Me) -> Decision {
    match action {
        Action::Push {
            force,
            delete,
            tags,
            ..
        } => decide_push(*force, *delete, *tags, facts, me),
        Action::BranchForceDelete { .. } => high("DELETE BRANCH"),
        Action::Release { .. } => high("RELEASE"),
        Action::RepoAdmin { .. } => high("REPO SETTINGS"),
        Action::Pr { verb, speaks, .. } => match facts {
            Facts::Pr(f) => decide_pr(*verb, *speaks, f, me),
            _ if *speaks => AS_YOU,
            _ => Decision::Grey,
        },
        Action::Issue { verb, speaks, .. } => {
            let issue = match facts {
                Facts::Issue(f) => Some(f),
                _ => None,
            };
            decide_issue(*verb, *speaks, issue, me)
        }
        Action::ApiWrite { .. } | Action::McpWrite { .. } | Action::TooDeep => Decision::Grey,
    }
}

/// The reason from the action alone, when it needs no facts.
fn push_reason(force: bool, delete: bool, tags: bool) -> Option<Decision> {
    if force {
        Some(high("FORCE PUSH"))
    } else if delete {
        Some(high("DELETE BRANCH"))
    } else if tags {
        Some(high("TAG"))
    } else {
        None
    }
}

fn decide_push(force: bool, delete: bool, tags: bool, facts: &Facts, me: &Me) -> Decision {
    match facts {
        Facts::Push(PushFacts {
            owner,
            branch,
            default_branch,
            pushes_tag,
            ..
        }) => {
            if !me.inside(owner) {
                SOMEONE_ELSE
            } else if let Some(d) = push_reason(force, delete, tags || *pushes_tag) {
                d
            } else if branch == default_branch {
                high("MAIN BRANCH")
            } else {
                Decision::Cleared
            }
        }
        _ => push_reason(force, delete, tags).unwrap_or(Decision::Grey),
    }
}

fn decide_pr(verb: PrVerb, speaks: bool, f: &PrFacts, me: &Me) -> Decision {
    if !f.inside(me) {
        SOMEONE_ELSE
    } else if speaks {
        AS_YOU
    } else if verb == PrVerb::Merge && f.base == f.default_branch {
        high("MAIN BRANCH")
    } else {
        Decision::Cleared
    }
}

fn decide_issue(verb: IssueVerb, speaks: bool, f: Option<&IssueFacts>, me: &Me) -> Decision {
    let outsider = f.is_some_and(|f| !f.inside(me));
    if outsider {
        SOMEONE_ELSE
    } else if matches!(verb, IssueVerb::Delete | IssueVerb::Transfer) {
        high("DELETE ISSUE")
    } else if speaks {
        AS_YOU
    } else if f.is_some() {
        Decision::Cleared
    } else {
        Decision::Grey
    }
}

/// Combines the verdicts of several actions: someone else, then high stakes,
/// then grey, then cleared. Among equals, the first wins.
pub fn decide_all(verdicts: impl IntoIterator<Item = Decision>) -> Decision {
    fn rank(d: &Decision) -> u8 {
        match d {
            Decision::Flagged {
                kind: FlagKind::SomeoneElse,
                ..
            } => 3,
            Decision::Flagged {
                kind: FlagKind::HighStakes,
                ..
            } => 2,
            Decision::Grey => 1,
            Decision::Cleared => 0,
        }
    }
    let mut best = Decision::Cleared;
    for d in verdicts {
        if rank(&d) > rank(&best) {
            best = d;
        }
    }
    best
}

/// Lines for the card about a PR that is not in a good state to act on.
pub fn conflicts(facts: &Facts, _me: &Me) -> Vec<String> {
    let Facts::Pr(f) = facts else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(r) = &f.my_last_review
        && r.state == "CHANGES_REQUESTED"
    {
        match f.commits_since_my_review {
            Some(0) => out.push(format!(
                "You requested changes on {}; no commits since",
                r.at
            )),
            Some(n) => out.push(format!(
                "You requested changes on {}; {n} commit(s) since",
                r.at
            )),
            None => {}
        }
    }
    if f.checks_failing {
        out.push("Checks are failing".to_string());
    }
    if f.draft {
        out.push("This PR is a draft".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag::facts::{IssueFacts, Review};
    use serde_json::json;

    fn me() -> Me {
        Me {
            login: "smnai".into(),
            orgs: vec!["nodo-labs-uk".into()],
        }
    }

    fn pr_facts(author: &str, cross: bool, head_owner: &str, base: &str) -> PrFacts {
        PrFacts {
            repo: "nodo-labs-uk/socratui".into(),
            number: 5,
            author: author.into(),
            author_is_bot: false,
            author_association: String::new(),
            base: base.into(),
            default_branch: "main".into(),
            cross_repo: cross,
            head_owner: head_owner.into(),
            draft: false,
            checks_failing: false,
            my_last_review: None,
            commits_since_my_review: None,
        }
    }

    fn push(force: bool, delete: bool, tags: bool) -> Action {
        Action::Push {
            dir: None,
            remote: None,
            refspecs: vec![],
            force,
            delete,
            tags,
        }
    }

    fn push_facts(owner: &str, branch: &str) -> Facts {
        Facts::Push(PushFacts {
            owner: owner.into(),
            repo: "socratui".into(),
            branch: branch.into(),
            default_branch: "main".into(),
            pushes_tag: false,
        })
    }

    fn pr(verb: PrVerb, speaks: bool) -> Action {
        Action::Pr {
            verb,
            number: Some(5),
            repo: None,
            selector: None,
            speaks,
        }
    }

    fn flagged(kind: FlagKind, label: &'static str) -> Decision {
        Decision::Flagged { kind, label }
    }

    #[test]
    fn the_incident_is_flagged_at_every_step() {
        let me = me();
        let mut f = pr_facts("Timonsc", true, "Timonsc", "main");
        f.my_last_review = Some(Review {
            state: "CHANGES_REQUESTED".into(),
            at: "2026-11-17".into(),
        });
        f.commits_since_my_review = Some(0);
        let facts = Facts::Pr(f);
        let someone_else = flagged(FlagKind::SomeoneElse, "SOMEONE ELSE");

        assert_eq!(
            decide(
                &push(false, false, false),
                &push_facts("Timonsc", "fix-5"),
                &me
            ),
            someone_else
        );
        assert_eq!(decide(&pr(PrVerb::Merge, false), &facts, &me), someone_else);
        assert_eq!(
            decide(&pr(PrVerb::Comment, true), &facts, &me),
            someone_else
        );
        assert_eq!(
            decide(
                &push(false, false, false),
                &push_facts("nodo-labs-uk", "fix-5"),
                &me
            ),
            Decision::Cleared
        );
        assert!(
            conflicts(&facts, &me)
                .contains(&"You requested changes on 2026-11-17; no commits since".to_string())
        );
    }

    #[test]
    fn speaking_on_an_inside_pr_is_as_you() {
        let facts = Facts::Pr(pr_facts("smnai", false, "nodo-labs-uk", "main"));
        assert_eq!(
            decide(&pr(PrVerb::Comment, true), &facts, &me()),
            flagged(FlagKind::SomeoneElse, "AS YOU")
        );
        assert_eq!(
            decide(&pr(PrVerb::Comment, true), &Facts::Unknown, &me()),
            flagged(FlagKind::SomeoneElse, "AS YOU")
        );
    }

    #[test]
    fn merging_my_own_pr_into_main_is_main_branch() {
        let facts = Facts::Pr(pr_facts("smnai", false, "nodo-labs-uk", "main"));
        assert_eq!(
            decide(&pr(PrVerb::Merge, false), &facts, &me()),
            flagged(FlagKind::HighStakes, "MAIN BRANCH")
        );
    }

    #[test]
    fn merging_into_a_feature_branch_is_cleared() {
        let facts = Facts::Pr(pr_facts("smnai", false, "nodo-labs-uk", "feat/x"));
        assert_eq!(
            decide(&pr(PrVerb::Merge, false), &facts, &me()),
            Decision::Cleared
        );
    }

    #[test]
    fn pushing_a_tag_is_high_stakes_unless_someone_else() {
        let tag = |owner: &str| {
            let Facts::Push(f) = push_facts(owner, "v1.2") else {
                unreachable!()
            };
            Facts::Push(PushFacts {
                pushes_tag: true,
                ..f
            })
        };
        assert_eq!(
            decide(&push(false, false, false), &tag("nodo-labs-uk"), &me()),
            flagged(FlagKind::HighStakes, "TAG")
        );
        assert_eq!(
            decide(&push(false, false, false), &tag("someone"), &me()),
            SOMEONE_ELSE
        );
    }

    #[test]
    fn force_push_tags_release_and_branch_delete_are_high_stakes() {
        let me = me();
        for facts in [Facts::Unknown, push_facts("nodo-labs-uk", "fix")] {
            assert_eq!(
                decide(&push(true, false, false), &facts, &me),
                flagged(FlagKind::HighStakes, "FORCE PUSH")
            );
            assert_eq!(
                decide(&push(false, false, true), &facts, &me),
                flagged(FlagKind::HighStakes, "TAG")
            );
            assert_eq!(
                decide(&push(false, true, false), &facts, &me),
                flagged(FlagKind::HighStakes, "DELETE BRANCH")
            );
            assert_eq!(
                decide(
                    &Action::BranchForceDelete {
                        dir: None,
                        branch: "x".into()
                    },
                    &facts,
                    &me
                ),
                flagged(FlagKind::HighStakes, "DELETE BRANCH")
            );
            assert_eq!(
                decide(&Action::Release { repo: None }, &facts, &me),
                flagged(FlagKind::HighStakes, "RELEASE")
            );
            assert_eq!(
                decide(&Action::RepoAdmin { repo: None }, &facts, &me),
                flagged(FlagKind::HighStakes, "REPO SETTINGS")
            );
        }
        assert_eq!(
            decide(
                &push(false, false, false),
                &push_facts("nodo-labs-uk", "main"),
                &me
            ),
            flagged(FlagKind::HighStakes, "MAIN BRANCH")
        );
        assert_eq!(
            decide(
                &push(true, false, false),
                &push_facts("Timonsc", "fix"),
                &me
            ),
            flagged(FlagKind::SomeoneElse, "SOMEONE ELSE")
        );
        let issue = Action::Issue {
            verb: IssueVerb::Delete,
            number: Some(1),
            repo: None,
            speaks: false,
        };
        assert_eq!(
            decide(&issue, &Facts::Unknown, &me),
            flagged(FlagKind::HighStakes, "DELETE ISSUE")
        );
    }

    #[test]
    fn edits_without_a_body_ready_and_reopen_on_inside_prs_are_cleared() {
        let facts = Facts::Pr(pr_facts("smnai", false, "nodo-labs-uk", "main"));
        for verb in [PrVerb::Edit, PrVerb::Ready, PrVerb::Reopen, PrVerb::Close] {
            assert_eq!(decide(&pr(verb, false), &facts, &me()), Decision::Cleared);
        }
        let issue = Action::Issue {
            verb: IssueVerb::Close,
            number: Some(2),
            repo: None,
            speaks: false,
        };
        let ifacts = Facts::Issue(IssueFacts {
            repo: "nodo-labs-uk/x".into(),
            number: 2,
            author: "smnai".into(),
            author_is_bot: false,
            author_association: String::new(),
        });
        assert_eq!(decide(&issue, &ifacts, &me()), Decision::Cleared);
    }

    #[test]
    fn api_and_mcp_writes_are_grey() {
        let api = Action::ApiWrite {
            method: "POST".into(),
            path: "repos/a/b/x".into(),
        };
        let mcp = Action::McpWrite {
            tool: "create_issue".into(),
            input: json!({}),
        };
        assert_eq!(decide(&api, &Facts::Unknown, &me()), Decision::Grey);
        assert_eq!(decide(&mcp, &Facts::Unknown, &me()), Decision::Grey);
    }

    #[test]
    fn unknown_facts_make_a_plain_push_grey() {
        assert_eq!(
            decide(&push(false, false, false), &Facts::Unknown, &me()),
            Decision::Grey
        );
        assert_eq!(
            decide(&pr(PrVerb::Merge, false), &Facts::Unknown, &me()),
            Decision::Grey
        );
    }

    #[test]
    fn bots_and_orgs_are_inside() {
        let me = me();
        assert!(me.inside("dependabot[bot]"));
        assert!(me.inside("Nodo-Labs-UK"));
        assert!(me.inside("SMNAI"));
        assert!(!me.inside("Timonsc"));
    }

    fn by(author: &str, association: &str, repo: &str, base: &str) -> Facts {
        let mut f = pr_facts(author, false, repo.split('/').next().unwrap(), base);
        f.repo = repo.into();
        f.author_association = association.into();
        Facts::Pr(f)
    }

    #[test]
    fn a_teammates_pr_is_inside_and_an_outsiders_is_not() {
        let merge = pr(PrVerb::Merge, false);
        // A member of the user's org, merged into a feature branch.
        assert_eq!(
            decide(
                &merge,
                &by("bob", "MEMBER", "nodo-labs-uk/aq", "feat/x"),
                &me()
            ),
            Decision::Cleared
        );
        assert_eq!(
            decide(
                &merge,
                &by("bob", "COLLABORATOR", "smnai/dotfiles", "feat/x"),
                &me()
            ),
            Decision::Cleared
        );
        // An outside contributor.
        assert_eq!(
            decide(
                &merge,
                &by("Timonsc", "CONTRIBUTOR", "nodo-labs-uk/aq", "feat/x"),
                &me()
            ),
            SOMEONE_ELSE
        );
        // A member of a repo outside the user's orgs is outside.
        assert_eq!(
            decide(&merge, &by("bob", "MEMBER", "other-org/x", "feat/x"), &me()),
            SOMEONE_ELSE
        );
        // The target line uses the same test.
        let teammate = by("bob", "MEMBER", "nodo-labs-uk/aq", "feat/x");
        assert_eq!(
            crate::flag::target(std::slice::from_ref(&teammate), Some(&me())).unwrap(),
            "nodo-labs-uk/aq PR #5 by bob"
        );
        let outsider = by("Timonsc", "CONTRIBUTOR", "nodo-labs-uk/aq", "feat/x");
        assert!(
            crate::flag::target(std::slice::from_ref(&outsider), Some(&me()))
                .unwrap()
                .ends_with("(outside your org)")
        );
        // An inside author's own fork is inside; someone else's fork is not.
        let Facts::Pr(mut f) = teammate else {
            unreachable!()
        };
        f.cross_repo = true;
        f.head_owner = "bob".into();
        assert_eq!(
            decide(&merge, &Facts::Pr(f.clone()), &me()),
            Decision::Cleared
        );
        f.head_owner = "Timonsc".into();
        assert_eq!(decide(&merge, &Facts::Pr(f), &me()), SOMEONE_ELSE);
    }

    #[test]
    fn a_bots_pr_is_inside() {
        let merge = pr(PrVerb::Merge, false);
        let Facts::Pr(mut f) = by("app/dependabot", "", "nodo-labs-uk/aq", "feat/x") else {
            unreachable!()
        };
        assert_eq!(
            decide(&merge, &Facts::Pr(f.clone()), &me()),
            Decision::Cleared
        );
        f.author = "renovate".into();
        f.author_is_bot = true;
        assert_eq!(decide(&merge, &Facts::Pr(f), &me()), Decision::Cleared);
        assert!(me().inside("app/renovate"));
    }

    #[test]
    fn mirror_prune_and_follow_tags_are_high_stakes_and_all_is_grey() {
        let one = |line: &str| {
            let actions = crate::flag::candidate::candidates("Bash", &json!({ "command": line }));
            assert_eq!(actions.len(), 1, "{line}");
            decide(&actions[0], &Facts::Unknown, &me())
        };
        let delete = flagged(FlagKind::HighStakes, "DELETE BRANCH");
        assert_eq!(one("git push --mirror"), delete);
        assert_eq!(one("git push --prune origin"), delete);
        assert_eq!(
            one("git push --follow-tags"),
            flagged(FlagKind::HighStakes, "TAG")
        );
        assert_eq!(one("git push --all origin"), Decision::Grey);
    }

    #[test]
    fn the_strongest_verdict_wins() {
        let high = flagged(FlagKind::HighStakes, "MAIN BRANCH");
        let someone = flagged(FlagKind::SomeoneElse, "SOMEONE ELSE");
        assert_eq!(decide_all([Decision::Cleared, high, someone]), someone);
        assert_eq!(
            decide_all([Decision::Cleared, Decision::Grey]),
            Decision::Grey
        );
        assert_eq!(decide_all([Decision::Grey, high]), high);
        assert_eq!(decide_all([]), Decision::Cleared);
    }

    #[test]
    fn conflicts_report_commits_checks_and_drafts() {
        let mut f = pr_facts("smnai", false, "smnai", "main");
        f.my_last_review = Some(Review {
            state: "CHANGES_REQUESTED".into(),
            at: "2026-11-17".into(),
        });
        f.commits_since_my_review = Some(2);
        f.checks_failing = true;
        f.draft = true;
        assert_eq!(
            conflicts(&Facts::Pr(f), &me()),
            [
                "You requested changes on 2026-11-17; 2 commit(s) since",
                "Checks are failing",
                "This PR is a draft"
            ]
        );
        assert!(conflicts(&Facts::Unknown, &me()).is_empty());
    }
}
