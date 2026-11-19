# Flagged action card

aq holds some actions that Claude runs, in every mode, until you answer. They are the ones that speak for you or are hard to undo, such as merging a stranger's PR, force-pushing, or posting a comment under your name.

The card is the [permission card](permission.md) with a band above the request. `y` allows, and `n` refuses with a reason Claude reads.

## What is flagged

A `PreToolUse` hook on Bash and GitHub tools sends aq only the calls that could be one of these. Everything else never reaches aq, so ordinary commands are not slowed down.

| Action | Flag | Why |
|---|---|---|
| A push to a repo owned outside your orgs, such as a contributor's fork. A push is judged by the owner of the repo it goes to, whatever PR it is for | `SOMEONE ELSE` | it changes someone else's work |
| Any write on a PR whose author or head repo is outside, or on an issue whose author is outside, such as a merge or a close | `SOMEONE ELSE` | it acts on someone else's work |
| A comment, review, reply or body edit on any PR or issue, or a merge, close or reopen with a message | `AS YOU` | it is posted under your name |
| A merge or push to the default branch | `MAIN BRANCH` | everyone sees it |
| A push of tags, including `git push --follow-tags` | `TAG` | hard to undo |
| `gh release` writes | `RELEASE` | public, and hard to undo |
| A force push, to any branch | `FORCE PUSH` | it rewrites history |
| A remote branch delete, `git push --mirror` or `--prune`, `git branch -D` | `DELETE BRANCH` | it can lose work |
| `gh issue delete` or `transfer` | `DELETE ISSUE` | hard to undo |
| `gh repo delete`, `archive`, `unarchive`, `rename` or `edit` | `REPO SETTINGS` | it changes the whole repo |
| A call the rules cannot place, which Haiku flags, such as `git push --all` or a GitHub write aq does not know | `HIGH STAKES` or `SOMEONE ELSE` | Haiku's judgement |

Routine work is not flagged: a push to a feature branch in your own repo, opening your own PR or issue (`gh pr create`, `gh issue create`), and merging your own PR into a branch that is not the default.

When both kinds apply, such as merging an outsider's PR into main, the card shows `SOMEONE ELSE`.

## Which calls are checked

In Bash, aq reads each command of the line, including the commands inside `$(...)` and backticks. A name the shell fills in, such as `gh pr merge $n` or `git push $REMOTE`, is not looked up, so the call goes to Haiku.

| Call | Read as |
|---|---|
| `git push` | a push. With no remote named, it goes where git sends it: the branch's `pushRemote`, then `remote.pushDefault`, then the branch's remote, then `origin`. With no branch named, it lands on the branch's upstream branch, or else one of the same name |
| `gh pr merge`, `close`, `comment`, `review`, `edit`, `ready`, `reopen` | a PR write, on the PR named by number, link or branch. A link's own repo is used |
| `gh issue close`, `comment`, `edit`, `reopen`, `delete`, `transfer` | an issue write |
| `gh api` POST to `repos/{owner}/{repo}/issues/{n}/comments` | a comment, as you |
| `gh api` POST to `repos/{owner}/{repo}/pulls/{n}/reviews` or `/comments` | a PR review or comment, as you |
| `gh api -X PUT repos/{owner}/{repo}/pulls/{n}/merge` | a PR merge |
| `gh api -X DELETE repos/{owner}/{repo}/git/refs/heads/{branch}` | a remote branch delete |
| `gh api graphql` | checked only when the query is a `mutation`, or comes from a file |
| Any other `gh api` write | a call for Haiku |

GitHub MCP tools are all checked except reads: tools whose name starts with `get_`, `list_` or `search_`, or ends with `_read`. The common ones are read as the same actions:

| MCP tool | Read as |
|---|---|
| `merge_pull_request` | a PR merge |
| `add_issue_comment`, `add_comment_to_pending_review`, `add_reply_to_pull_request_comment` | a comment, as you |
| `create_pending_pull_request_review`, `submit_pending_pull_request_review`, `create_and_submit_pull_request_review`, `pull_request_review_write` | a PR review, as you |
| `update_pull_request`, `update_issue`, `issue_write` | an edit, close or reopen, as you when it sets a body |
| `push_files`, `create_or_update_file`, `delete_file` | a push to that repo and branch |
| `create_pull_request`, `create_issue` | not checked: opening is not an answer |
| Any other tool | a call for Haiku |

## Inside and outside

You are inside, and so are your orgs and bots (a login starting with `app/` or ending in `[bot]`). For a repo or a push, the owner is inside when it is you or one of your orgs.

For the person who opened a PR or issue, aq also asks GitHub for their access to the repo (`author_association`). They are inside when they have owner, member or collaborator access to a repo that you or one of your orgs owns. Everyone else is outside, such as a contributor from outside or a member of some other org. A PR from a fork is inside only when the fork's owner is inside too, or is the PR's inside author.

aq asks `gh` who you are, so `gh` must be logged in as you.

## The voice rule

Anything posted as you on a PR or issue is flagged, whoever opened it, even your own. That covers comments, reviews, replies and body edits. Opening your own PR or issue is not flagged.

The band tells the two causes apart: `AS YOU` when the only reason is your voice, `SOMEONE ELSE` when an outsider is involved.

## The band

The two kinds differ without colour. Someone else starts its band with `▌▌`, and high stakes starts with `▲`. The queue bar shows the same mark on the card's tab.

The band shows the target, such as the repo and PR number. Below it come who is involved, what will happen, what the action does under your name, and anything that contradicts earlier state, such as `you requested changes on 17 Nov; no commits since`. Haiku writes these lines, so until they arrive the band reads `Haiku: checking…`. The conflict line comes from `gh` facts where it can.

## Keys

The keys are the [permission card's](permission.md#keys). `y` still works only once you have scrolled to the end of the request. A refusal tells Claude not to try another way to do the same thing.

## If aq cannot check a call

If `gh` is logged out, offline, or the remote is not GitHub, a call that needs those facts to be placed goes to Haiku to judge. Calls that the rules flag without them, such as a force push, a tag, a branch delete, a release, a repo setting, or anything posted as you, are still flagged. If Haiku also fails or takes more than 30 seconds, the call is held on a card whose band reads `aq could not check this`, with the reason. Silence never lets a call through.

## If aq is not running

With no aq running, the hook prints nothing and Claude Code behaves as normal.

## If the deadline passes

The hook waits up to its deadline, 3570 seconds in the settings in [Hooks](../hooks.md). When the deadline passes, or aq quits while a call waits, the hook refuses the call and tells Claude it is waiting for you in aq. An unanswered call is never allowed.

## The log

Each decision is appended as one line of JSON to `~/.local/state/aq/flags.jsonl`. `XDG_STATE_HOME` moves the folder, as for [`state.json`](../configuration.md). Only you can read the file.

| Field | What it holds |
|---|---|
| `at` | the time, in Unix seconds |
| `tool` | `Bash` or the name of the GitHub tool |
| `command` | the command, or the tool's input |
| `decision` | `cleared`, `flagged`, `grey` (the rules could not place it), `unchecked`, then `allowed` or `denied` for what you answered |
| `label` | the band's label, or `null` |

One call can have several lines: `grey` when the rules cannot place it, then Haiku's verdict (`cleared`, `flagged` or `unchecked`), then your answer.

Lines for [questions and menus](question.md#flagged-questions-and-menus) have `card` (`question` or `choice`) in place of `tool` and `command`, and `by` (`facts` or `haiku`) for a flagged one. Their `decision` is `flagged`, `cleared`, `none` (checked, not flagged) or `error` (the check failed). Read the log to check that flags stay rare and right.

## The hook entry

This needs the second `PreToolUse` entry, with the matcher `Bash|mcp__.*github.*`, in your settings. The hooks in `contrib/settings-hooks.json` have it. Restart sessions after you add it.
