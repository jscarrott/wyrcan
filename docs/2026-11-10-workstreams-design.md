# Claude Board: workstreams — design

Supersedes the screen and storage parts of `design.md`. Lines marked **(call)** are Claude's decisions, not the user's; the user can overrule them.

## Why

I'm overloaded because starting Claude Code work is too easy: any directory plus `claude` is another stream, and nothing tells me how many I have going or what state each is in. The first version of Claude Board was a view onto a Todoist project. It helped me see finished work, but it didn't change the habit, and its three-column screen was hard to read.

The board becomes **the primary way I use Claude Code**. Every piece of Claude work starts from it, so it always has the full picture, and it limits how much I have open at once.

A typical day: open the board. See what's active and who needs me. Deal with those. Look at what's in review. If there's a free slot, resume something parked or start the next thing from the queue. I never run `claude` in a bare directory.

## The unit: a workstream

A workstream is one task: a title, a description, the directory and repos it touches, the Claude sessions that have worked on it, and (later) its PR and explainer. It is not a Claude session; a stream can have several sessions over its life, one current at a time.

## Stages

`Queue → Active → Review → Done`, plus `Parked`.

- **Queue**: ordered list of work not started.
- **Active**: streams I'm working on. This is what the cap limits.
- **Review**: my side is done and it's **waiting on someone else** (a PR reviewer). Doesn't count against the cap.
- **Parked**: started, set aside by hand. Frees a slot. Keeps its session and directory so resuming carries on where it left off.
- **Done**: finished. Hidden by default.

### The cap

- A setting, `max_active` (default 3 **(call)**). It counts streams in Active.
- With no free slot the board refuses to start from the queue, resume a parked stream, or adopt an untracked session into Active. It says why.
- A Review stream whose PR comes back with changes requested **waits**: the card is marked "changes requested, waiting for a slot" and stays in Review until I free a slot and pull it back to Active myself.
- The cap is a rule of the board, checked in one place, for every way a stream can enter Active (keys, mouse, CLI, `/next`).

### The Review gate

A stream can only enter Review when its gate passes. The built-in gate has two checks:

1. **PR**: a PR exists for the work, *if a PR is appropriate*. **(call)** The agent decides at wrap-up; if it judges no PR is needed (for example a dotfile change) it records a one-line reason, which shows on the card so I can send it back.
2. **Explainer**: an HTML explainer of the change exists, saved with the task.

How it happens **(call)**: I tell a stream to wrap up (a key on the board, or `/wrap` typed in its session). Its Claude session does the work: opens the PR using that repo's normal workflow, writes the explainer, then reports both to the board (`claude-board gate …`). The board verifies each check and moves the stream to Review, or leaves it in Active showing what's missing. I don't do either step by hand, and I can't move a card into Review past a failing gate.

**Claude finishing a turn no longer moves anything to Review** **(call)**. It only marks the stream "waiting on me" inside Active. (Today the Stop hook moves the task to Needs review; that's what made Review a dumping ground.)

In Review the card shows the PR's state: waiting on reviewer, changes requested, approved, merged. Merged streams are marked ready to close; one key marks them Done **(call: not automatic)**.

### Only Active streams have a live session **(call)**

- A stream's Claude pane is closed when it enters Review or is parked, and reopened (`claude --resume`) when it comes back to Active. So answering review comments means pulling the stream back to Active, which needs a slot.
- If a session of a Review or Parked stream is seen working anyway (I resumed it by hand), the card is flagged "working outside Active". Hooks never change a stream's stage.

### Leaving without Review **(call)**

- **Done is only reachable from Review.** There is no Active → Done.
- **Drop** abandons a stream from any stage, after a confirm. It is recorded as Dropped (kept in history, hidden like Done), not as Done.
- A stream can go back to the Queue from Active or Parked (it keeps its sessions).
- A stream whose PR check was waived has no one to wait on: it enters Review already marked ready to close.
- A PR closed without merging is flagged on the card like changes requested.

### Several repos **(call)**

A stream can touch several repos, so it has a PR (or a recorded waiver) **per repo**. The gate needs one for every repo the stream touched. The card shows the worst state across them (changes requested > waiting on reviewer > approved > merged), and the stream is ready to close when all are merged or waived.

### Later: other workflows

I may want different gates for different kinds of task, and more complex workflows. Not now. To leave room: a task has a `kind` (only `default` exists), and a kind's gate is a named list of checks (`["pr", "explainer"]`) read from config, with each check a small function. Adding a kind or a check later must not need a schema change or a rewrite. No workflow editor, no custom stages yet.

## The screen

The board runs in a zellij pane and uses zellij for the Claude sessions. It does not draw Claude itself.

```
┌ Board ───────────────────┐┌ auth-timeout · working ──────────────┐
│ ACTIVE 2/3               ││                                      │
│ ▌fix auth timeout  ● work││  (the live Claude Code session for   │
│  ingest drops      ■ blkd││   the selected stream)               │
│                          ││                                      │
│ REVIEW 1                 ││                                      │
│  pcap parser   ⧗ reviewer││                                      │
│                          ││                                      │
│ PARKED 2                 │├ ingest drops · blocked ──────────────┤
│  …                       │└──────────────────────────────────────┘
│ QUEUE 5                  │
│  …                       │
│ UNTRACKED 1              │
└──────────────────────────┘
```

The sketch above is the board's **narrow form**, used beside a session. See "Look and feel" below for the full-width form, which is the home screen.

- **Left: the board**, a narrow single list grouped by stage (Active, Review, Parked, Queue, Untracked). Cards show their **full title, wrapped**, never cut off, with the agent's state and how long it's been in it, repo labels, PR state, and any missing gate checks. Blocked agents stand out and the header counts them.
- **Right: each stream's Claude session is a real zellij pane**, in a stack. Selecting a card brings that stream's pane to the front of the stack. One key makes it full screen; one key gets back to the board.
- **Expand a card** (kept from the current build): title, description, session, directory, repos, PR link and state, gate status, history of stage changes.
- **Start** the top (or selected) queue item: opens its Claude pane in the stack, in the task's directory, and the stream becomes Active.
- **Resume** a parked stream: reopens its last session (`claude --resume`) in its directory.
- **Park** an active stream: frees the slot and closes its pane (asking first if the agent is working) **(call)**.
- **Move** a card between stages with keys or mouse drag, wherever the cap and the gate allow.
- **Add to the queue** from the board: type a title (and optionally a directory), or start a `/pen` writer session as today.
- **Open the explainer** or the PR in the browser with one key each.
- **Untracked**: Claude sessions started outside the board. **(call)** A session appears here once it has edited a file (question-only sessions stay off the board, as today). I can adopt one as a stream; it then counts against the cap. The board can't stop me running `claude` by hand, but those sessions are visible instead of invisible.

### Look and feel: like Todoist (user's decision)

The UI should be much richer than the current build and look like Todoist. The board has two forms, like Todoist's board and list views, switched with one key:

**Full width** — the home screen, for deciding what to do:

```
 Claude Board                                        2/3 active · 1 blocked
 ──────────────────────────────────────────────────────────────────────────────────────
  Active  2/3              Review  1                Parked  2               Queue  5
 ╭──────────────────────╮ ╭──────────────────────╮ ╭──────────────────────╮ ╭────────────────────╮
 │ ◉ Fix auth timeout   │ │ ○ Pcap parser drops  │ │ ○ Tidy ingest retry  │ │ ○ Rate-limit the   │
 │   on token refresh   │ │   truncated frames   │ │   logic              │ │   export endpoint  │
 │   ● working · 4m     │ │   ⧗ waiting on Sam   │ │   parked 2d          │ │   delta            │
 │   delta   api        │ │   delta   #12692     │ │   delta              │ ╰────────────────────╯
 ╰──────────────────────╯ ╰──────────────────────╯ ╰──────────────────────╯ ╭────────────────────╮
 ╭──────────────────────╮                                                   │ ○ …                │
 │ ◉ Ingest drops events│                                                   ╰────────────────────╯
 │   ■ blocked · 12m    │                                                    + Add task
 │   pipeline           │
 ╰──────────────────────╯
```

**Narrow list** — the same cards in one grouped column, beside a Claude session (the first sketch).

- **Cards** have a rounded border and space around them: full title, a status line, labels as coloured chips. The circle at the left is coloured by state: red blocked, green working, yellow waiting on me.
- **Column headers with counts**, and a quiet "+ Add task" at the foot of the Queue.
- **Quick add**: a one-line box; type a title with `@label` and `#directory` inline, parsed as I type.
- **Task view is a modal** like Todoist's: title and description on the left with a thread underneath (stage changes, the agent's wrap-up summary, PR events, comments); a side panel on the right with stage, repos, PRs, explainer, session and dates.
- **Restrained**: one accent colour, dim secondary text, thin dividers rather than heavy boxes.

### Directory for a new stream

A task can carry a directory. **(call)** If it doesn't, the board asks for one when I start it, offering directories used by earlier streams.

## Storage: SQLite (user's decision)

- One database, `~/.local/state/claude-board/board.db`. It is the source of truth.
- It holds streams, their sessions (agent state, zellij pane), repos, PR and explainer, gate results, queue order, and a history of stage changes with timestamps.
- **Only the `claude-board` binary touches the database.** The Claude Code hooks become `claude-board hook <event>` (reading the hook's JSON on stdin). The bash hook script and its tests are replaced, not extended.
- `claude-board export` dumps everything as JSON so the data is never trapped.
- Explainers are files beside the database (`explainers/<task>.html`).

## Todoist: optional sync

- Off by default; everything works without it and without a token.
- When on: tasks added to the Todoist project's Up next appear in the Queue, and queue order follows Todoist, so I can add and reorder from my phone. Stage changes made on the board are pushed to Todoist (section moves, completion), and the explainer's location and PR link are added to the task as a comment.
- The board owns stages; Todoist owns new tasks and queue order. A stage change made in Todoist does not move a stream on the board **(call)**; in particular it can't get around the cap or the gate.
- The existing "Claude Board" project is imported once on first sync. **(call)** Its In progress and Needs review tasks (five today, with no PR or explainer on record) land in **Parked**, not Review: by the new rules they aren't through the gate, so I resume and wrap them up one at a time under the cap.

## Kept, changed, dropped

- **Kept**: Rust + ratatui; the Todoist client; the installer (now registers the binary's hooks); the agent states (`working`, `waiting`, `blocked`, `ended`) and how the hooks detect them; the Claude-dir rules (`CLAUDE_CONFIG_DIR`, then `claude_dir` in `~/.config/claude-board/config.toml`, then `~/.claude`); `/next` and `/pen` (now calling the binary); the card detail view; mouse support.
- **Changed**: storage (state files + Todoist → SQLite); the screen (three columns → grouped list beside the session stack); Stop no longer moves to Review; agents open in the stack beside the board, not in separate tabs.
- **Dropped**: the bash hook script; Todoist as a requirement; herdr fallback.

## Constraints

- **Hooks must never slow Claude down or break it.** Every hook exits 0 whatever happens, does one short database write, and gives up quickly if the database is busy. No network in hooks.
- **zellij 0.45.1.** Known to exist: `zellij run --stacked --no-focus --name --cwd`, `zellij action focus-pane-id`, `toggle-fullscreen`, `stack-panes`, `list-panes`. **Not yet verified, and the screen depends on it**: that focusing a stacked pane and returning focus to the board leaves that pane on show, and how to get from a full-screen session back to the board with one key when the board isn't focused (probably a zellij keybinding shipped in a layout). **Spike this first**; if it doesn't work, come back to me before building the rest.
- Panes are started by the zellij server, so new sessions get `CLAUDE_CONFIG_DIR` passed explicitly (as now).
- The board stays usable outside zellij for looking; starting and focusing sessions says "not inside zellij".
- Lives in `~/git-personal/experiments/claude-board/`, branch `claude-board`. Nothing is pushed without asking.
- The Todoist token is never printed or sent anywhere but Todoist.

## Non-goals

- A workflow editor, custom stages, or more than the one built-in kind.
- Drawing Claude sessions inside the TUI (an embedded terminal).
- Stopping me from running `claude` by hand.
- Agents that start themselves, or keeping N agents busy.
- Managing worktrees or jj workspaces per stream.
- Tracking my manual edits; several machines; herdr; Capacities.

## Anti-goals (a failure even if it works)

- The cap can be got around by accident (a path into Active that doesn't check it).
- A stream reaches Review without its PR and explainer.
- Hooks make Claude sessions slower or flakier.
- The board needs Todoist or the network to work.
- I still can't read a card's title.
- Data I can't get out of the database.

## Left to the builder

- The schema, and the SQLite crate.
- Key bindings, colours, exact card layout, how wide the board is.
- How the board tells a session to wrap up (typing `/wrap` into its pane, or just telling me to); the `/wrap` skill's wording; what the explainer contains beyond "explains the change to a reviewer".
- How PR state is fetched (`gh`) and how often.
- How the zellij layout is set up (a layout file, a launcher, or the board arranging panes itself).
- Sync timing and how Todoist sections map to stages (Parked needs somewhere to go).
- How existing state files are migrated, or whether they're just left behind.
- Tests: the cap and the gate each need tests that try every way into Active and Review.
