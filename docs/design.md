# Claude Board TUI — design

## Why

I run many Claude Code sessions in parallel across several repos. Work finishes and then never gets reviewed or turned into a PR, because by then my attention has moved on (the "AI brain fry" problem from https://sergeykuhne.com/blog/fighting-ai-brain-fry). A first version already exists: a Todoist project, "Claude Board", kept up to date by Claude Code hooks, plus a `/next` skill that picks up the top queued task. What's missing is one place to **see the queue and which agents are blocked, and act on it**: start the next task, jump to an agent that needs me, or get new tasks written. That place is a terminal UI running in a zellij pane.

A typical day: open the TUI, see the queue and who's blocked, deal with blocked agents, start the next task. When new work turns up (a rough note, a Slack thread, a GitHub issue), start a "writer" agent that talks it through with me and adds well-formed tasks to Up next.

## What exists today (to be moved into this repo)

- `~/.claude-work/hooks/claude-board.sh` (bash, curl + jq, Todoist API v1 `https://api.todoist.com/api/v1`). Token in `~/.config/todoist/token`.
  - `edit` (PostToolUse on Edit|Write|MultiEdit|NotebookEdit, async): the first edit in a session creates a task in **In progress**; editing a further repo adds it to the description and labels.
  - `stop` (Stop, async): moves the task to **Needs review** and, if there were edits since the last stop, comments with Claude's last message.
  - `next`: prints the top **Up next** task as JSON (ordered by `child_order`); exits 1 if empty.
  - `claim SESSION TASK CWD`: moves a task to In progress and links it to a session.
  - State: `~/.local/state/claude-board/<session_id>.json` = `{session, cwd, task, repos:[{root,ref}], dirty, section, note, labels0}`; board ids are cached in `board-<hash>.json` `{p, un, ip, nr}`.
  - Smoke test against a throwaway Todoist project: `scratchpad/test-board.sh` (move it into the repo).
- `~/.claude-work/skills/next/SKILL.md`: `/next` picks up and claims the top task in the current session; `/next pane` opens a new herdr pane running `claude "/next"`.
- Hooks are registered in `~/.claude-work/settings.json` (not chezmoi-managed). The Claude config dir is `~/.claude-work`.

## Goals

1. **The board at a glance.** Three lists: Up next, In progress, Needs review. In progress and Needs review rows show the agent's live state, the repo labels and how long the task has been in that state.
2. **Blocked agents stand out.** They're highlighted and sorted to the top, and the header counts them ("2 blocked").
3. **Start the next task with one key.** Opens a new zellij pane running `claude "/next"` in a sensible working directory.
4. **Jump to an agent with one key.** Focuses the agent's zellij pane. If the pane is gone, offers to resume the session in a new pane (`claude --resume <session>` in the task's cwd).
5. **Start a writer agent with one key.** Opens a new zellij pane running `claude "/pen"`. The `/pen` skill holds a conversation: I paste a note, a Slack link or an issue number, it reads the source (gh CLI, Slack MCP), asks what it needs, proposes one or more tasks (title, description, repo label), and only after I confirm adds them to the bottom of Up next.
6. **Complete a reviewed task from the TUI.** Completes it in Todoist.
7. **Open a task in Todoist,** for editing or reordering there.
8. **Stays current without me refreshing.** Agent state from local state files (fast, about 1–2 s); Todoist data by polling (about 15–30 s), plus an immediate refresh after the TUI's own actions.

## Agent state (new hook work)

The hooks track agent state in the session state file so the TUI doesn't depend on a multiplexer to know it:

- `working`: set on UserPromptSubmit and PostToolUse.
- `waiting`: set on Stop. Claude finished its turn and the ball is in my court.
- `blocked`: set on Notification. Claude needs a permission or input. If the hook input distinguishes a permission prompt from an idle reminder, only permission prompts count as blocked.
- `ended`: set on SessionEnd.
- Also recorded: `ZELLIJ_SESSION_NAME` and `ZELLIJ_PANE_ID` from the hook environment, and a `state_at` timestamp.

The state hooks only update sessions that already have a state file (claimed by `/next`, created by a first edit, or registered by `/pen`), so question-only sessions stay off the board. State stays local and isn't written to Todoist.

Writer (`/pen`) sessions register themselves with a state file `kind: "pen"` and no task, so the TUI can list running writers and their state.

## Constraints and decisions made

- **Rust**, with ratatui + crossterm for the TUI. This was the user's choice.
- **Lives in** `~/git-personal/experiments/claude-board/`, a subfolder of an existing git monorepo of experiments (remote `jscarrott/experiments`). The Rust crate, the hook script, the skills (`next`, `pen`) and the smoke test all live here.
- **Install step.** One command (e.g. `claude-board install`) symlinks the hook script and skills into `~/.claude-work` and registers the hooks in `~/.claude-work/settings.json` without disturbing existing entries. It's idempotent.
- **Todoist is the source of truth** for tasks and their order. The TUI is a view and a remote control. I still reorder Up next from my phone.
- **Multiplexer: zellij** (0.45.1: `zellij run --cwd DIR -- cmd` prints the new pane id; `zellij action focus-pane-id`; `zellij action list-panes`). I'm leaning away from herdr, so the TUI targets zellij only. `/next pane` should work under zellij, keeping herdr only as a fallback if trivial.
- **The hook script stays bash** (it works and is tested). The TUI reads the same state files and calls the Todoist API directly.
- **Hooks must never slow Claude down or break it.** They run async, exit 0 on every error, and use short timeouts.

## Non-goals

- An autonomous agent that works the board on a schedule (the article's cycles). Every agent is started by me.
- Running several agents at once by policy ("keep three busy").
- Tracking my own manual edits. Only Claude sessions are tracked.
- herdr integration beyond a trivial fallback.
- Capacities.
- Multiple machines.

## Anti-goals (a failure even if it works)

- The TUI becomes the source of truth, or holds state I can't see or fix in Todoist.
- Hooks make Claude sessions slower or flakier.
- The writer agent adds tasks without my confirmation.
- Noise: question-only sessions or writer sessions clutter the task lists.

## Decisions from the builder check (Claude's calls; the user can overrule)

- **Age shown on a row** is the time since the agent's last state change (`state_at`). Time in a Todoist section isn't tracked, because phone moves would make it wrong.
- **Resume is only offered for sessions marked `ended`.** A session whose pane simply can't be found may be live somewhere else, and resuming it would put two processes on one session. Those rows show "running elsewhere" instead.
- **Completing a task from the TUI** asks for confirmation if its agent is `working` or `blocked`. It then completes the task in Todoist and marks the state file `closed`, so a later edit in that session starts a new task rather than reviving the completed one. The agent's pane is never closed by the TUI.
- **Ended writer sessions and closed or ended task sessions** drop out of the TUI's agent display. The task itself stays visible for as long as it's open in Todoist.

## zellij layout (user's answer)

- **One zellij session, agents in tabs.** The TUI runs in that same session and "jump" uses `zellij action focus-pane-id` (switching tabs as needed). An agent recorded in a different zellij session, or with no zellij pane id, shows as "running elsewhere" and isn't jumped to.
- **New `/next` and `/pen` agents open in a new tab** in the same session (named after the task or "pen"), so the board's tab isn't crowded. Whether focus follows the new tab is left to the builder; staying on the board is the better default.

## Left to the builder

- Layout: three columns vs. tabs vs. one grouped list; colours; key bindings beyond "one key each".
- The HTTP client and async runtime (blocking client on a background thread is fine).
- Exact poll intervals within the ranges above.
- The `/pen` skill's wording and how it phrases proposed tasks.
- How the TUI picks the cwd for a new `/next` pane (its own cwd is a fine default).
- Tests: unit tests for state parsing, sorting and the hook state transitions; keep the Todoist smoke test.
