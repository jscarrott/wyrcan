# Task types: experiments, PR reviews and views

Board task #20. Status: design, awaiting approval.

## Why

Every board task today runs one flow: claim it, do the work, open a PR per repo touched, write an explainer, gate into Review, close when merged. That fits most work, and it stays the default.

Two kinds of work don't fit it:

- **Experiments.** Work done as a trial or ahead of need. It is real work on a branch, but it shouldn't have to end in a PR. The PR may come weeks later, so the work must stay findable: its explainer and its branch, so it can be resumed and taken to PR.
- **Reviewing other people's PRs.** Today these aren't on the board at all. John wants them in front of him, imported from GitHub, worked in a Claude session like a task, and gone once they merge.

## Goals

- A task carries a type: `default` (today's flow), `experiment`, or `review`.
- A task can carry an optional link: the issue, PR or thread it tracks. A review's link is its PR.
- The board has three views and a key to switch between them: **My work** (today's board), **Reviews**, and **Shelved**.
- Experiments close into **Shelved**, where their explainer and branches stay visible. One can be resumed as a default task and taken to PR.
- PRs where John's review is requested are imported automatically into **Reviews**. Picking one opens a Claude session on it, as with a task. It leaves when the PR merges.
- Type and link can be set on `add`, `edit` and in `/pen`, and come out of `next`, `claim`, `list`, `status` and `export`, so `/next` and `/wrap` can act on them.

## Non-goals

- Separate types for GitHub issues or Slack threads. Where a task came from is just its link. An issue-backed task is a default task with an issue link, and `/next` passes that link to `/start-feature`.
- Reviewing PRs without a Claude session (a plain tracker). Reviews open sessions.
- Any change to how default tasks flow through My work.

## Anti-goals (a failure even if it works)

- The default flow gets slower or noisier: extra prompts, extra required flags, new gate checks on default tasks.
- Reviews eat Active slots. They don't count against `max_active`.
- Finished experiments vanish into Done where they can't be found.
- Imported PRs flood My work or the Queue.

## Constraints

- One SQLite database owned by `claude-board`. Schema changes go through `migrate()` and must cope with older binaries resetting `user_version` (check the schema, not the number).
- Untrusted text: PR titles, bodies and authors come from GitHub. Links are handed to `xdg-open` and `gh`, so they must be `https://`, as `--pr` already requires.
- `gh` calls follow `pr.rs`: time-limited, failures read as "unknown", never crash or refuse.
- Skills live in `skills/` in this repo (symlinked into `~/.claude-work/skills`).

## What John decided

1. **Three views, switched with a key:** My work · Reviews · Shelved. Reviews and Shelved are separate views, not extra columns on My work.
2. **Experiments** are ordinary tasks on My work until wrapped. `/wrap` needs only an explainer and summary, no PR. They then go to Shelved, not Review or Done.
3. **A shelved experiment shows its explainer and its branch per repo**, so he can resume it and get it into PR.
4. **Reviews open a Claude session on the PR, like a task.**
5. **A review finishes when the PR merges.**
6. **Each review gets its own jj workspace** rather than a checkout in a shared repo.
7. **An experiment's bookmark stays local** (not pushed).
8. **Import both direct and team review requests**, plus an option to import all open PRs in repos he's active in.

## The design

### Data

- **Type.** Reuse the existing `tasks.kind` column (today always `'default'`). Values: `default`, `experiment`, `review`. Review-gate checks already key off kind (`gate_<kind>` in config), so the defaults become: `default` → `pr,explainer` (unchanged), `experiment` → `explainer`. A review is never gated.
- **Link.** New nullable `tasks.link` column: an `https://` URL, validated on input.
- **Shelved stage.** A new `Stage::Shelved`. Allowed moves: Active → Shelved (via the gate, experiments only), Shelved → Active (resume), Shelved → Dropped.
- **Reviews** are tasks with `kind = 'review'` and `link` = the PR URL, so sessions, panes, hooks and events work unchanged. The Reviews view's columns come from the stage plus the PR's state:
  - **Requested:** imported, not yet picked up (stage Queue).
  - **Reviewing:** has a session (stage Active).
  - **Waiting on author:** John has submitted a review and isn't re-requested (stage Parked).
  - The PR merges or closes → Done (merged) or Dropped (closed), and it leaves the view.
- Review tasks never count toward `max_active` and never appear in My work, `next`, or the Queue that `/next` reads.

### Importing reviews

- The board's existing background poll also runs `gh search prs --review-requested=@me --state=open --json url,title,author,repository,updatedAt`, which covers both direct and team requests. It uses the same timeout and failure rules as `pr.rs`.
- **Option: all open PRs in active repos.** With `reviews_all_open = true` in the config (off by default), the poll also imports every open PR in repos John is active in, except his own. *My call:* "active in" means a repo in the `repos` table for a task touched in the last 30 days. These land in Requested with a dim "not requested" note.
- *My call:* if a request disappears before John has reviewed it (withdrawn, or a teammate covered a team request), the card stays where it is and shows "no longer requested". It leaves on merge or close like any other, or when John drops it.
- A new URL becomes a review task in Requested. The title is the PR title; the labels are the repo name. A known URL is updated, not duplicated (unique on link for review tasks).
- A review in Waiting on author goes back to Requested when John is re-requested.
- Merged → Done. Closed without merge → Dropped.

### Working a review

- Picking a Requested card (Enter, as for a Queue card) opens a pane and a Claude session, like a task, and the card moves to Reviewing.
- **Directory: its own jj workspace**, never a checkout in a shared repo (active tasks are working in those). The board finds the parent repo at `~/git-work/<repo name>`. If that's missing, it asks with the existing directory prompt and remembers the parent for that repo. It then creates the workspace `~/git-work/<repo>-<pr#>-review` at the PR's head (`jj git fetch`, then `jj workspace add` on the PR branch). This matches folders John already makes by hand, such as `delta-15129-review`.
- When the review finishes (PR merged or closed), the board forgets the workspace (`jj workspace forget`) and deletes its folder. The builder decides whether that happens on the poll or on the next start.
- **Start command:** a new `/review-pr <task id>` skill (in `skills/`). It claims the task, reads the PR, its discussion and diff, and helps John review. It never submits a review without his explicit say-so.
- *My call:* submitting a review doesn't kill the pane. The card moves to Waiting on author only once John has submitted a review **and** the session has ended. Picking a waiting card reopens its session (Parked → Active, as for any parked task).

### Experiments

- Created with `--kind experiment` (CLI, `/pen`, or quick add; see below), or switched from default with `edit --kind`.
- `/wrap` reads the kind from `status`. For an experiment it skips the PR step. In each repo touched, it creates a **local bookmark** `exp/<task id>-<slug>` at the work (not pushed; in a git repo, a branch of that name). It records the bookmarks with the explainer and summary (e.g. `gate --bookmark ROOT=NAME`, stored in place of that repo's `git_ref`). `gate` then moves the task to **Shelved** instead of Review. The hook's jj change id alone isn't enough, because a later restack or squash can make it unfindable.
- **The Shelved view** lists shelved experiments, newest first. Each card shows the title, the summary, the explainer path, and each repo touched with its bookmark.
- **Resume** (Enter on a shelved card) sets kind to `default` and moves it to Active (subject to the cap). *My call:* it starts a **fresh** session, not `--resume` of the weeks-old one. The session runs in the task's directory and starts with a prompt (e.g. `/next <id>`) that names the bookmark per repo, so Claude picks up from those. From then on it's a default task, so `/wrap` requires PRs.
- Drop works on shelved cards as elsewhere.

### CLI and skills

- `add` and `edit` gain `--kind K` (one of the three) and `--link URL` (`https://` only; `--link ""` clears it on edit).
- `task_json` (used by `next`, `claim`, `edit`, `status`) and `list` rows gain `kind` and `link`. `export` picks up the new column by itself.
- `next` returns only non-review Queue tasks.
- `/pen`: proposes a kind per task (`default` unless the source says otherwise) and passes the source link with `--link`.
- `/next`: if `link` is a GitHub issue, use `/start-feature` with it. If the task is an experiment, tell the user that `/wrap` will shelve it rather than ask for a PR.
- `/wrap`: branches on kind as above.
- `/board`: document `--kind` and `--link`.

### The TUI

- A key cycles My work → Reviews → Shelved. The current view's name shows in the header with the other two dimmed.
- My work is today's board, minus review tasks.
- Reviews: three columns (Requested · Reviewing · Waiting on author). Cards show title, author, `repo#number` and age.
- Shelved: one list or column. Cards as above.
- Task view (the detail modal) gains **Type** and **Link** fields.
- Quick add accepts a kind marker for experiments (syntax is the builder's call, matching how quick add already parses labels).

## Left to the builder

- The view-switch key and header styling.
- Whether Shelved is one column or a list layout.
- How often the review import runs, within the existing poll.
- The quick-add syntax for an experiment.
- Todoist sync for the new stage and for review tasks. Reviews should not sync. For Shelved, pick the least surprising mapping or don't sync it.
- How review tasks leave Active/Parked for Done without the `prs`-based close check, and keeping them out of `active_count`.
- Where the remembered parent folder per repo is stored.
- Whether merged reviews appear in `closed` and `/board-log`.

## Already in the working tree (uncommitted)

Partial edits from before this design: the `link` column and migration, a `KINDS` list (to be changed to `default, experiment, review`), `Store::set_kind`, and a gate default of `explainer` for kind `local` (to be renamed `experiment`). Keep or redo them as fits the plan.
