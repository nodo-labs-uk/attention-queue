# Modes

Each project (git checkout) has one mode. `M` on a card from that project, or on its tab in the sessions view, moves it to the next mode:

review → step by step → quiz → vibe → review

The tab shows the mode when it is not review. A new mode applies from the session's next finished turn; the card on screen stays as it is.

| Mode | What needs you |
|---|---|
| Review (the default) | When a session finishes a turn, its diff becomes a [review card](cards/review.md). |
| Step by step | Each edit waits for you before it is applied, as a [step card](cards/step.md). A shell command that changed files becomes a [shell change card](cards/shell-change.md). |
| Quiz | When a session finishes, you explain pieces of its diff in your own words on a [quiz card](cards/quiz.md). |
| Vibe | Nothing is checked. A finished turn makes no card. |

In every mode, permission prompts, questions, option menus and messages between sessions still come to the queue.

## Choosing a mode

- **Review** suits most work: the session codes a whole turn, and you read the change and comment once.
- **Step by step** suits code where every change matters, such as a payments service. Each edit waits for a yes.
- **Quiz** checks that you understand a change before you build on it.
- **Vibe** suits throwaway scripts and experiments.

Step mode needs the aq hooks. Restart a session started before the hooks were added.

The only way back from vibe mode is `M` on the project's tab in the sessions view, because vibe mode makes no cards to press `M` on.
