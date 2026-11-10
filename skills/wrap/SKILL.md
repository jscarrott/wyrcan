---
name: wrap
description: Wrap up this workstream for review. Use when the user says /wrap, "wrap this up", or "ready for review": opens PRs (or records why not), writes an explainer, and moves the task to Review on the Claude Board.
---

# /wrap — hand this workstream to review

The board is the `claude-board` binary. It refuses review until every repo you touched has a PR or a stated reason, and an explainer exists.

1. `claude-board status ${CLAUDE_SESSION_ID}`. It prints `{task, repos, prs, missing}` for this session's task, or exits 1 if the session has no task (then tell the user and stop).
2. For each repo in `repos`, decide whether a PR is appropriate. If yes, open it with that repo's normal workflow (e.g. `/finish-feature`) and note the URL. If no, write a one-line reason. If `repos` is empty (changes made through Bash, or outside a repo), still record one for the task's directory (`task.dir`, else `$PWD`): pass `--pr '<dir>=<url>'` or `--no-pr "<dir>=$reason1"` with that directory as the root.
3. Write a self-contained HTML explainer of the change for a reviewer (what changed, why, how to verify, risks) to a temp file, e.g. `/tmp/explainer-$$.html`.
4. Record it and move to review. The summary and every no-PR reason are free text (backticks, `$`, apostrophes), so never put them inline in quotes. Put each in a variable through a flush-left single-quoted heredoc, as below, with the values filled in. If a value contains a line equal to `CLAUDE_BOARD_WRAP_END`, reword that line first. Repeat `--pr` / `--no-pr` once per repo, using the repo root exactly as listed in `repos`; add one `reason` heredoc per waived repo (reason1, reason2, ...) and omit `--pr`/`--no-pr` flags you don't need.

```bash
summary=$(cat <<'CLAUDE_BOARD_WRAP_END'
<two sentences>
CLAUDE_BOARD_WRAP_END
)
reason1=$(cat <<'CLAUDE_BOARD_WRAP_END'
<one-line reason>
CLAUDE_BOARD_WRAP_END
)
claude-board gate ${CLAUDE_SESSION_ID} --pr '<root>=<url>' --no-pr "<root>=$reason1" --explainer <file> --summary "$summary"
```

5. If it prints missing items, fix them and run it again. Never tell the user the work is review-ready until it prints "In review."
