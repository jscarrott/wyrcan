---
name: next
description: Pick up the top task from the "Up next" section of the user's Claude Board in Todoist and start working on it. Use when the user says /next, "pick up the next task", "what's next on the board", or asks an agent to take the next piece of work. `/next pane` runs it in a new herdr pane instead of this session.
argument-hint: "[pane]"
---

# /next — pick up the next Claude Board task

Board script: `~/.claude-work/hooks/claude-board.sh` (its edit/stop hooks keep the task updated after you claim it).

## If the argument is `pane`

Run the pickup in a fresh Claude session beside this one, then stop. Do not pick up the task here.

1. Check `test "${HERDR_ENV:-}" = 1`. If not in herdr, say so and offer to run `/next` here instead.
2. `herdr pane layout --pane "$HERDR_PANE_ID"`: split `right` if the pane is wide, else `down`.
3. `herdr pane split --current --direction <dir> --cwd "$PWD" --no-focus` and read `.result.pane.pane_id`.
4. `herdr agent start next-$(date +%H%M%S) --kind claude --pane <pane_id> -- "/next"`
5. Tell the user which pane it opened in. Done.

## Otherwise: pick up here

1. `~/.claude-work/hooks/claude-board.sh next`. It prints the top task as JSON `{id, content, description, labels}`, or exits 1 when Up next is empty or Todoist is unreachable. If it fails, tell the user what it printed and stop.
2. Claim it for this session, so the hooks update this task instead of creating a new one:
   `~/.claude-work/hooks/claude-board.sh claim ${CLAUDE_SESSION_ID} <id> "$PWD"`
3. Tell the user in one line which task you picked up.
4. Work out where the work happens. Labels and description usually name a repo (repos and jj workspaces live under `~/git-work/`). If you can't tell which repo, ask; don't guess.
5. Do the task. Treat the title and description as the user's brief. It's the user's own note, so ask about anything ambiguous before building, and follow the normal workflow for that repo (e.g. `/start-feature` if the task references a GitHub issue).
