# PR review

`R` opens a pull request as a [review card](review.md). You comment on its lines as you would on a session's diff. On your own PR the comments go to Claude, which fixes the code and pushes. On someone else's PR they go to GitHub as one review.

## Opening a PR

`R` works from anywhere in aq, except while you type in a box. It lists the open PRs of every repo that a session of yours has open, newest first, and marks yours. Type to filter by repo, number, title or author. `↑` `↓` choose, `Enter` opens.

You can also type a PR number or paste its link, then press `Enter`. A bare number needs aq to know which repo you mean: it takes the repo of the row with that number, or the only repo. With several repos and no matching row, paste the link.

A session must be open in a clone of the repo, because aq needs a clone to check the PR out in. `gh` must be logged in.

| Key | What it does |
|---|---|
| `Enter` | open the PR that is chosen, or the number or link that is typed |
| `↑` `↓` | choose a PR (`j` `k` too, before you type anything) |
| `Esc` | close the list |

A PR that is already open is shown, not opened again.

## Where the checkout comes from

aq never switches a branch in a session's folder.

- Your own PR, when a session's folder already has its branch checked out: the card reviews that folder.
- Any other PR: aq makes a worktree in its state folder, at `pr/<owner>-<repo>-<number>` (see [Files and configuration](../configuration.md#pr-worktrees)), and checks the PR out there with `gh pr checkout`. Someone else's PR is checked out on its head commit with no branch, and aq never pushes from it.

A PR counts as someone else's unless you wrote it. A teammate's PR is someone else's.

If the checkout fails, for example because a local branch with the PR's name has extra commits, aq shows the reason `gh` gave and creates no worktree.

The card shows a line with the PR's number, title and author, and diffs the whole PR from its base branch.

## Your own PR

Comment as on any review card. `s` sends every comment to the Claude session in that folder, after a line that names the PR and asks Claude to fix what the comments ask, run the tests, commit and push to the PR's branch. With no session in the folder, aq starts one with the comments as its first prompt.

`a` approves as on any review card, and ends the PR review: the folder's next review is an ordinary session review again. A worktree that aq made is removed under the same rules as `D` below.

## Asking about a PR

`?` asks Claude a [side question](../tools.md#side-questions) about the PR, on your own PR or someone else's. The PR's description goes with the question.

## Someone else's PR

| Key | What it does |
|---|---|
| `c` | comment on a line, or on the range you picked with `V` |
| `s` | post your comments as a review with no verdict |
| `a` | post the review and approve |
| `X` | post the review and request changes |
| `r` | check the PR out again, to pick up new commits |

`s`, `a` and `X` open a summary box first. It counts where each comment will go and takes one line of text for the review. `Enter` posts and `Esc` cancels. A comment review or a request for changes needs a comment or a summary. Approving needs neither.

When the review is posted, the card closes, and the worktree is removed under the same rules as `D` below.

## Where comments land

GitHub takes a line comment only on a line that its own diff shows. A comment on any other line goes in the review's main text, after your summary, as `` `path:12`: text `` or `` `path:12-15`: text ``. aq tags those comments on the card when you write them:

| Tag | Where the comment goes |
|---|---|
| none | on the line |
| `file` | in the main text: GitHub's diff has the file, but not those lines |
| `top` | in the main text: GitHub's diff has no such file, or could not be read |

The summary box counts how many comments go on lines and how many go in the main text.

If aq could not read GitHub's diff of the PR, the status line says so when the card opens or after `r`. Every comment then goes in the main text, and you can still post. When GitHub refuses to show a diff because it is too large, the status line says `GitHub will not show this diff`, and `r` does not change that.

## What aq checks

- aq refuses to post if the PR moved on GitHub since you opened or refreshed it. Press `r`, then post again. After a refresh that brought new commits, if you already have comments, aq warns you to check where your comments sit.
- If the card's diff could not be made after `r`, the card still shows the older diff. aq refuses to post until `r` shows the new one.
- If posting fails, the card and your comments stay, and the status line shows GitHub's message.
- A failure that may have reached GitHub (a timeout, a network error or a server error) is checked for: before it posts again, aq looks on GitHub for your review on every commit it tried to post on. If the review is there, it is not posted twice: aq closes the card and says `Your review had already reached PR #n`. When GitHub refused the review (an error in the 400s), nothing was made; aq still runs the check if an earlier attempt on this card may have reached GitHub.
- `r` is refused while a session is working in the worktree, and aq will not check out over uncommitted changes in it.

## The rest of the card

| Key | What it does |
|---|---|
| `i` | read the PR's description, a word at a time, as the [fast reader](../tools.md#fast-reading) does |
| `D` | close the PR review, after a yes |

`D` removes the card, its rounds and its saved comments. It removes the worktree only if aq made it, it has no uncommitted changes and no session is working in it. Any other folder is kept. A worktree counts as made by aq when it is in aq's `pr` folder.

A PR with no changes against its base opens no card. The status line says so, and a worktree aq made for it is removed under the same rules.

The other keys are those of the [review card](review.md).
