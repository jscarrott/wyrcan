---
name: pen
description: Turn a note, GitHub issue/PR, or Slack thread into tasks on the user's Claude Board in Todoist (the "Up next" section). Use when the user says /pen, "write tasks for the board", or "turn this note/thread/issue into tasks". Planning only; never edits code.
---

# /pen — write tasks for the Claude Board

Board script: `~/.claude-work/hooks/claude-board.sh`. This is a scratch session: **never edit code or files here.** Your only output is tasks on the board.

1. Register this session as a pen, so the board TUI shows it:
   `~/.claude-work/hooks/claude-board.sh register-pen ${CLAUDE_SESSION_ID} "$PWD"`
2. If the user didn't give a source, ask what it is (a pasted note, a GitHub issue/PR, a Slack link).
3. Read it. GitHub: `gh issue view <n-or-url> --comments` / `gh pr view <n-or-url> --comments`. Slack links: the Slack MCP read tools (thread or channel). Pasted notes: use as given.
4. Ask what you need to write good tasks: which repo each belongs to, and what "done" means. Skip what the source already answers.
5. Propose the tasks as a numbered list. Each has:
   - a title: imperative, at most 80 characters;
   - a description: context, the source link, and a definition of done;
   - one repo label (the repo's directory name).
6. **Only after the user confirms** (and in the order they agree), add each one:
   `~/.claude-work/hooks/claude-board.sh add "<title>" "<description>" "<repo-label>"`
   It prints the new task's id and exits 1 on failure; on failure, say so and stop.
7. Report the created ids, in order.
