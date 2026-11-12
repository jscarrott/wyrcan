---
name: board
description: Read or change tasks on the user's Claude Board from any session — list tasks, add one, edit a task's title/description/labels/dir, put a task back in the Queue, or drop it. Use when the user asks to add, edit, rename, relabel, requeue, move or drop a board task, asks what is on the board, or when you notice follow-up work worth recording as a task.
---

# Claude Board commands

The board is the `claude-board` binary; it owns the database, so never edit it any other way. Commands print JSON or a short message on success and exit 1 with the reason on stderr when refused; exit 2 means a usage error and prints the full usage line.

Find ids first: `claude-board list` prints open tasks (`queue`, `active`, `review`, `parked`) as `[{id, title, stage, dir, labels}]` in board order. Add `--stage S` (repeatable) for specific stages, including `done` and `dropped`. This session's own task: `claude-board status ${CLAUDE_SESSION_ID}` (`task.id`).

| Want | Command |
|------|---------|
| Add a task to the Queue | `claude-board add "TITLE" --desc "D" --dir DIR --label L --label L2` → prints the new id |
| Change a task in place (keeps id, stage, position) | `claude-board edit ID --title T --desc D --dir DIR --label L..` — only the flags given change; any `--label` replaces the whole set; `--dir ""` clears it |
| Release an Active or Parked task back to the bottom of the Queue | `claude-board requeue ID` |
| Drop a task (closes it for good) | `claude-board drop ID` — ask the user first |
| Everything, including sessions and events | `claude-board export` |

Flag values may start with `-`. Quote titles and descriptions. Prefer `edit` over drop-and-add: it keeps the id and queue position.

Other skills own the rest of the workflow, so use them instead of the raw commands: `/next` (claim a task), `/pen` (turn a note into several tasks), `/wrap` (gate for review), `/board-log` (closed-work report).
