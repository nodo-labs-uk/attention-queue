# PR review: design

Status: draft for approval, 19 November 2026.

## Intent

The review card already lets the user read a Claude session's changes the way tuicr does, comment on lines and ranges, and send the comments back as one message. Pull requests on GitHub get none of this: the user reads them in the browser or in tuicr, and there is no way to hand comments on a PR to Claude from aq.

This change opens any open GitHub PR in the review card. What happens to the comments depends on whose PR it is:

- **The user's own PR:** the comments go to a Claude session working on that branch, which fixes the code and pushes an update. The next round shows what changed since the last review.
- **Someone else's PR** (a teammate's or an outsider's): the user can ask Claude questions about it in the small side-question box, and the comments go to GitHub as one review under the user's name.

## Goals

1. Find a PR from a list of open PRs in the repos the user's sessions work in, or by typing a number or a GitHub link.
2. Show the PR in the existing review card: split and unified views, file list, line and range comments, files marked done, rounds.
3. The user's own PR: `s` sends the comments to a Claude session on the PR's branch, which fixes and pushes. Rounds work as they do for sessions today.
4. Someone else's PR: `?` asks Claude about it, answered from the PR's checkout with read-only tools. Sending posts the comments to GitHub as one review: comment only, approve, or request changes, with an optional summary.
5. A comment on a line that GitHub's diff does not show is posted as a comment on the file, with its line number in the text, and the card says so before sending.

## Non-goals

- Showing or replying to existing review threads on the PR. (Possible later.)
- CI status, labels, assignees, merge buttons.
- Hosts other than GitHub.
- Reviewing a PR without a local checkout.
- Posting comments one at a time (GitHub's "Add single comment"). Everything goes as one review.

## Anti-goals

- **Touching the user's own checkouts or running sessions.** aq never switches the branch of a folder a session works in. A PR always gets its own worktree.
- **Posting without the user's explicit send.** Nothing reaches GitHub until the user presses one of the send keys and confirms.
- **A second review card.** The PR card is the review card with a different source and a different send. It does not fork the code.

## Approaches

- **A. A worktree for every PR, with the diff computed locally.** aq checks the PR out into a worktree; the review card works on it as it does on a session's checkout. Before posting, aq reads GitHub's own diff (`gh pr diff`) to place each comment.
- **B. GitHub's diff for others' PRs, a worktree only for the user's own.** The card would read the diff and file contents from GitHub's API for others' PRs.
- **C. `gh pr checkout` in a session's folder.** Dropped: it switches a branch under a running Claude session.

**Picked: A** (the user agreed). One diff engine; the user's own PRs need a worktree anyway for Claude to fix them; side questions on others' PRs can read the whole codebase. The cost is a fetch and a worktree per PR.

## How it works

### 1. Finding a PR

A new overlay opened from the sessions view and from any card (proposed key: `R`, for "review a PR"; builder to check it is free).

- It lists open PRs from every GitHub repo that a known session's checkout belongs to (`gh pr list --json number,title,author,headRefName,isDraft,updatedAt` per repo), newest first: repo, number, title, author, and a mark for the user's own and for drafts.
- Typing filters the list. Typing a number picks that PR in the repo of the picked session; pasting a GitHub PR link picks that PR in its own repo, which must be one aq knows a local clone of.
- `Enter` opens the PR. While the worktree is prepared the overlay says so; failure (no clone, fetch failed, `gh` not logged in) is shown in the overlay with the reason.

### 2. The checkout

Which folder the card reviews:

- **The user's own PR, with a known checkout already on the PR's head branch** (usually the folder of the session that opened the PR): the card reviews that checkout. aq does not switch any branch, so the anti-goal holds, and git's rule that a branch can be checked out in only one worktree is never hit.
- **Otherwise:** aq makes a worktree. Git refuses a second checkout of a branch, so:
  - someone else's PR is checked out detached (`gh pr checkout <n> --detach`): it is never pushed from;
  - the user's own PR, with its branch not checked out anywhere, is checked out on its branch (`gh pr checkout <n>`), so Claude can push from it. If a local branch of that name has commits the PR lacks, `gh` fails; aq shows the reason and makes nothing.
- The worktree is made from the repo's main clone (the common git directory of any known checkout of that repo, `git rev-parse --git-common-dir`), at `<state dir>/aq/pr/<owner>-<repo>-<number>`: outside every repo, so session reviews of the main clone never see it as an untracked folder.
- If the worktree already exists, aq reuses it and runs `gh pr checkout` again to bring it up to date.
- Closing the PR review (a key on the card) removes a worktree aq made, if nothing in it is uncommitted and no session runs in it; otherwise aq keeps it and says why. Closing also clears that checkout's saved round and draft, so a reopened PR starts at round 1.
- `git`, `gh pr checkout`, `gh pr diff` and posting can take well over 10 s on a big PR. The `Runner` in `src/flag/facts.rs` is extended with a timeout per call, input on stdin, and the error text kept, and these calls use it with longer limits (proposed 120 s for checkout and fetch, 60 s for diff and posting).

### 3. The card

The existing review card, with:

- A head line naming the PR: `PR #5 · Fix the parser · by Timonsc` and, for someone else's PR, `(not yours: comments go to GitHub)`.
- Round 1 diffs the checkout against the merge base of `HEAD` and `origin/<baseRefName>` (fetched first; `baseRefName` from `gh pr view`). This is the same three-dot diff GitHub shows, and it works when the PR's base is not the default branch. The card keeps that base ref, so `r` (whole branch) uses it too. Later rounds work as now, from the tree of the last round.
- Its own key strip for someone else's PR (section 5).
- The PR's description is reachable behind a key (proposed: `i`, info), shown in the same reader the question card uses. It is not on the card by default.

"Own" means the PR's author is the user's GitHub login. A teammate's PR is someone else's.

### 4. The user's own PR

- `s` sends this round's comments, composed as today, to the Claude session working in the reviewed checkout (section 2):
  - if one is running there, as today;
  - else aq starts one in that checkout (as `N` does). The card holds the message, as it holds a message for a busy session today, until the new session shows up in that checkout, then sends it.
  Comments never go to a session in another folder: Claude would change files the card is not watching, and the next round would never come.
- The message also tells Claude that this is PR #n, to fix what the comments ask, run the tests, commit and push to the PR's branch.
- The push goes through the flagged-action hook like any other push. On the user's own repo and a non-default branch it is cleared; a force push is flagged.
- When the session's turn ends, the next round arrives as a review card, as for any session in review mode. A checkout under review as a PR counts as review mode for that purpose, whatever mode its repo has (the mode lookup checks the open PR reviews first).

### 5. Someone else's PR

- `?` opens the side-question box. Answers come from a `claude -p` run in the worktree with read-only tools (the existing side-question job, with no session). The PR's title, description and diff summary go in the prompt as data.
- Comments are written as now (`c`, `V` for ranges). Three send keys, each first asking for an optional summary in the existing one-line input:
  - `s` posts the review as a comment,
  - `a` approves,
  - `x` requests changes (the existing `x` on the review card deletes a comment; the builder picks a free key and records it).
- Before posting, the card lists anything that cannot go as a line comment (section 6) and asks to confirm.
- aq posts one review with `gh api repos/{owner}/{repo}/pulls/{n}/reviews` (`event`: `COMMENT`, `APPROVE` or `REQUEST_CHANGES`; `commit_id`: the head commit the worktree is on; `body`: the summary; `comments`: the line comments; everything else goes in `body`, after the summary).
- On success the card closes and the status line says how many comments were posted. On failure the card stays, with GitHub's message in the status line, and nothing is lost.
- This card has no session. It is the review card with no agent and a different send path: no rounds, no saved round, and reopening the PR later starts at round 1 against its base. Re-reviewing after the author pushes is reopening it.
- Comments the user writes here are the user's own words, posted by the user's own key press, so they do not pass through flagged-action cards.

### 6. Placing comments on GitHub

- aq reads `gh pr diff <n>` and notes, per file, which lines of the new side and the old side appear in GitHub's hunks.
- A comment on a line in GitHub's diff becomes a line comment: `path`, `line`, `side` (`RIGHT` for the new side, `LEFT` for a removed line), and `start_line`/`start_side` for a range.
- A range that is only partly inside GitHub's diff, a line outside it, or a file GitHub's diff does not have, goes in the review's main text as `` `path:42`: text `` or `` `path:40-45`: text ``. (Changed after the final review: the create-review API documents no file-level comment field.)
- The card marks such comments (proposed: a `file` tag beside the comment) as soon as they are written, not only at send time.
- If the PR's head moved on GitHub since the worktree was checked out, aq says so and offers to refresh before posting; posting against an old `commit_id` would misplace comments.

## Constraints

- Rust, in the existing modules: the review card (`src/review.rs`, `src/ui/review.rs`), the side questions (`src/ask.rs`), `gh` calls behind the `Runner` trait in `src/flag/facts.rs` where they fit, new session start through the existing effect.
- `gh` must be logged in as the user.
- No network calls in tests; `gh` and `git` behind a fake runner as in `src/flag/facts.rs`.
- The other review-card features keep working unchanged for session reviews.

## Decided by the user

- Comments on the user's own PR go to Claude, which fixes and pushes an update.
- On someone else's PR, questions go to Claude in the small side-question box, and comments go to GitHub as PR comments.
- Find a PR from a list or by number or link.
- Approach A: a worktree for every PR.

## Calls made by me (check these)

1. A teammate's PR counts as someone else's: comments go to GitHub.
2. Everything posts as one review, with comment, approve or request changes, and an optional summary.
3. Posting a review the user wrote is not a flagged action.
4. Lines GitHub does not show go in the review's main text with their path and line.
5. The worktree lives in aq's state directory (`<state dir>/aq/pr/<owner>-<repo>-<number>`), outside every repo, and is removed on close when it is clean and no session runs in it.
8. The user's own PR is reviewed in the checkout already on its branch when there is one; a worktree is made only when there is none. Someone else's PR is checked out detached.
9. Comments on the user's own PR go only to a session in the reviewed checkout; if none runs, aq starts one there and holds the message until it appears.
10. Someone else's PR has no rounds: each review is one round, and the card closes after posting.
11. The base is `origin/<baseRefName>`, fetched, so PRs into a non-default branch work and the lines match GitHub's diff.
12. The `Runner` gains per-call timeouts, stdin and kept error text for the long `gh` and `git` calls.
6. Proposed keys: `R` to find a PR, `i` for the description.
7. The PR worktree is always in review mode.

## Left to the builder

- The exact key for "request changes" and for closing the PR review.
- How the list is cached and refreshed.
- The prompt wording for Claude on the user's own PR and for side questions.
- Whether the PR head line and description reader reuse the question card's reader or a smaller view.
