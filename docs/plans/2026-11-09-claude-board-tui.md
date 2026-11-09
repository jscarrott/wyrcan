# Claude Board TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust TUI, running in a zellij tab, that shows the Todoist "Claude Board" with each agent's live state and starts, jumps to and completes work with one key.

**Architecture:** The existing bash hook script (moved into this repo) stays the writer: it talks to Todoist and now also records each session's agent state and zellij pane in `~/.local/state/claude-board/<session>.json`. The Rust binary is a reader and remote control: it polls Todoist for tasks on a background thread, re-reads the state dir every second, joins the two, and shells out to `zellij` for tabs and focus. `claude-board install` links the hook and skills into `~/.claude-work` and registers the hooks.

**Tech Stack:** Rust (edition 2024), ratatui + crossterm, ureq (blocking HTTP, `json` feature), serde/serde_json, anyhow. Bash + curl + jq for the hook. zellij 0.45.1.

**Spec:** `claude-board/docs/design.md`

## Global Constraints

- All paths below are relative to `~/git-personal/experiments/claude-board/`. Branch `claude-board`. Never push.
- Todoist API base `https://api.todoist.com/api/v1`; token file `~/.config/todoist/token` (trim whitespace).
- Env overrides shared by script and binary: `CLAUDE_BOARD_TOKEN_FILE`, `CLAUDE_BOARD_PROJECT` (default `Claude Board`), `CLAUDE_BOARD_STATE` (default `~/.local/state/claude-board`).
- Section names exactly: `Up next`, `In progress`, `Needs review`.
- Claude config dir is `~/.claude-work`; hooks go in `~/.claude-work/settings.json`, skills in `~/.claude-work/skills/<name>/`.
- Hooks must never fail or slow Claude: every hook mode exits 0; API hooks are `async: true`; state hooks are synchronous, local-only, `timeout: 5`.
- Todoist is the source of truth; the TUI never caches tasks to disk.
- zellij only. No herdr code in the TUI.
- Commits end with:
  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01GeJMCyFXmoZoz3uF1sooo2
  ```

## Review Focus

1. **Todoist unreachable or token missing while the TUI is open**: show an error line, keep the last good data, keep agent state live, don't exit. (Task 6: `fetch_error_keeps_last_tasks`)
2. **A malformed or half-written state file**, or one for a task that's no longer open: skip it silently; never crash or show a ghost row. (Task 3: `bad_state_file_is_skipped`, `orphan_state_is_ignored`)
3. **TUI started outside zellij**: `n`, `p` and Enter show "not inside zellij" instead of erroring. (Task 5: `outside_zellij_is_refused`)
4. **`n` with an empty Up next**: say "Up next is empty" and don't open a tab. (Task 6: `next_with_empty_queue_spawns_nothing`)
5. **Async hook ordering**: a late async API hook must not overwrite agent state set by the synchronous state hooks. (Task 2: `api_hooks_preserve_agent_state` in `tests/hook-state.sh`)

---

## File Structure

```
claude-board/
  Cargo.toml
  src/main.rs        CLI dispatch: no args -> TUI, `install` -> install
  src/config.rs      env/paths (token, project, state dir)
  src/state.rs       AgentRecord: read/parse the state dir
  src/board.rs       pure join of tasks + agents -> rows, sorting, counts, age text
  src/todoist.rs     API client: resolve board, list tasks, complete task
  src/zellij.rs      pure command builders + jump decision; thin runner
  src/app.rs         App state machine: keys -> Effects, confirm prompts, messages
  src/ui.rs          ratatui rendering
  src/install.rs     symlinks + settings.json hook merge
  hooks/claude-board.sh   (moved from ~/.claude-work/hooks)
  skills/next/SKILL.md    (moved from ~/.claude-work/skills/next)
  skills/pen/SKILL.md     (new)
  tests/smoke-todoist.sh  (moved from the session scratchpad test-board.sh; live API)
  tests/hook-state.sh     (new; offline)
```

---

### Task 1: Crate scaffold, move existing pieces in, `install`

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `src/config.rs`, `src/install.rs`
- Move: `~/.claude-work/hooks/claude-board.sh` → `hooks/claude-board.sh`; `~/.claude-work/skills/next/SKILL.md` → `skills/next/SKILL.md`; `/tmp/claude-1000/-home-johns/26eecf39-43d3-462f-a70d-5a9c879d837b/scratchpad/test-board.sh` → `tests/smoke-todoist.sh` (fix its `H=` to `$(dirname "$0")/../hooks/claude-board.sh`)
- Test: unit tests in `src/install.rs`

**Interfaces:**
- Produces: `config::Config { token_file: PathBuf, project: String, state_dir: PathBuf }`, `Config::from_env() -> Config`, `Config::token() -> anyhow::Result<String>`.
- Produces: `install::HOOKS: &[(&str /*event*/, Option<&str> /*matcher*/, &str /*script arg*/, bool /*async*/)]` and `install::merge_hooks(settings: serde_json::Value, script: &Path) -> serde_json::Value`.
- Produces: `install::run(repo_root: &Path) -> anyhow::Result<()>`.

The HOOKS table for this task holds the two existing entries only (`PostToolUse`, `Edit|Write|MultiEdit|NotebookEdit`, `edit`, async; `Stop`, none, `stop`, async). Task 2 adds rows. Command string format: `bash <abs path to ~/.claude-work/hooks/claude-board.sh> <arg>`.

- [ ] **Step 1: Write failing tests in `src/install.rs`**
  - `merge_adds_hooks_to_empty`: `merge_hooks(json!({}), p)` has `hooks.PostToolUse[0].hooks[0].command == "bash <p> edit"` with `async: true`, and `hooks.Stop[0].hooks[0].command == "bash <p> stop"`.
  - `merge_is_idempotent`: merging twice equals merging once.
  - `merge_keeps_unrelated_hooks`: an existing `SessionStart` herdr entry and an unrelated `PostToolUse` entry survive unchanged.
  - `merge_replaces_stale_board_entries`: an existing entry whose command contains `claude-board.sh` but with a different path or arg is removed. Identify board entries by the `claude-board.sh` substring.
- [ ] **Step 2: Run `cargo test install`** and check it fails because the functions are missing.
- [ ] **Step 3: Implement `merge_hooks` and `run`.** `run` does the following:
  - symlinks `hooks/claude-board.sh` → `~/.claude-work/hooks/claude-board.sh`, and every `skills/*` dir → `~/.claude-work/skills/<name>`
  - replaces existing regular files after backing them up to `<path>.bak-<unix-ts>`; an existing correct symlink is a no-op
  - backs up `settings.json` the same way, then writes the merged JSON pretty-printed.
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Run `cargo run -- install`**, then check:
  - `readlink ~/.claude-work/hooks/claude-board.sh` points into the repo
  - `jq '.hooks' ~/.claude-work/settings.json` shows the herdr SessionStart entry still present
  - `bash tests/smoke-todoist.sh` prints `PASS`.
- [ ] **Step 6: Commit:** `git add claude-board && git commit -m "Add claude-board crate with install; move hook, /next and smoke test in"`

### Task 2: Hook script — agent state, writers, `add`

**Files:**
- Modify: `hooks/claude-board.sh`, `src/install.rs` (HOOKS rows), `tests/smoke-todoist.sh`
- Create: `tests/hook-state.sh`

**Interfaces:**
- Produces the state-file fields every later task reads. These are **added**: `agent` (`"working"|"waiting"|"blocked"|"ended"|"closed"`), `state_at` (unix seconds, number), `zellij_session` (string or null), `zellij_pane` (string, e.g. `"terminal_3"`, or null), `kind` (`"task"` default, `"pen"`).
- Produces these script modes:
  - `state <working|waiting|blocked|ended>` (stdin hook JSON). It only touches an existing state file and never calls the API.
  - `register-pen SESSION CWD`: creates `{session, cwd, kind:"pen", agent:"working", ...}`.
  - `add TITLE DESCRIPTION [LABEL...]`: creates a task at the bottom of Up next and prints its id.
  - `close SESSION`: sets `agent:"closed"` (used by the TUI on complete).

New HOOKS rows (synchronous, `timeout: 5`):

| Event | Matcher | Mode |
|---|---|---|
| UserPromptSubmit | — | `state working` |
| PostToolUse | `*` | `state working` |
| Stop | — | `state waiting` |
| Notification | `permission_prompt\|elicitation_dialog` | `state blocked` |
| SessionEnd | — | `state ended` |

`state` writes `zellij_pane` as `terminal_$ZELLIJ_PANE_ID` when `ZELLIJ_PANE_ID` is set, plus `zellij_session` from `ZELLIJ_SESSION_NAME`. The first `edit` creating a state file also sets `agent:"working"`, `kind:"task"`, `state_at` and the zellij fields. `claim` sets the same. `edit` and `stop` exit early for `kind == "pen"` or when `agent == "closed"`. When `edit` runs on a `closed` file, it deletes the file first and then follows the normal new-task path, so a closed session starts a new task. `edit` and `stop` must read-modify-write only the fields they own (`repos`, `dirty`, `section`, `task`, `note`, `labels0`), so they don't clobber `agent` and `state_at`. The `state`, `register-pen`, `close` and `add` modes must be handled **before** the board-id lookup, except `add`, which needs it. Only `state` must work with no network.

- [ ] **Step 1: Write `tests/hook-state.sh`.** It's offline, using `CLAUDE_BOARD_STATE=$(mktemp -d)` and `CLAUDE_BOARD_TOKEN_FILE` pointing at a dummy token. Cases:
  - `state_ignores_unknown_session`: `state working` with no file creates nothing.
  - `state_transitions`: seed a file with `{session:"s",task:"t1",kind:"task"}`. Run `state working`, `state blocked`, `state waiting` and `state ended` in turn, with `ZELLIJ_PANE_ID=7 ZELLIJ_SESSION_NAME=main`. After each, `.agent` matches, `.state_at` is a number, `.zellij_pane == "terminal_7"` and `.zellij_session == "main"`.
  - `register_pen_creates_pen_record`: `.kind == "pen"`, `.agent == "working"`, no `.task`.
  - `api_hooks_preserve_agent_state`: seed `{..., agent:"waiting", state_at:123}`. Run `edit` against a file in a temp git repo (with an unreachable API it exits 0). Then `.agent == "waiting"` and `.state_at == 123`.
  - `close_marks_closed`: `close s` sets `.agent == "closed"`.

  Use plain `if ...; then fail; fi` checks; don't rely on `! cmd` under `set -e`.
- [ ] **Step 2: Run `bash tests/hook-state.sh`.** Expected: it fails on the first case.
- [ ] **Step 3: Implement the modes above in `hooks/claude-board.sh`.**
- [ ] **Step 4: Run `bash tests/hook-state.sh`.** Expected: `PASS`.
- [ ] **Step 5: Add an `add` check to `tests/smoke-todoist.sh`.** `add "Pen job" "desc" mine` returns an id whose task is in Up next, ordered after the existing ones.
- [ ] **Step 6: Add the HOOKS rows and extend the install test** `merge_adds_hooks_to_empty` to assert the Notification matcher string. Run `cargo test && bash tests/smoke-todoist.sh && cargo run -- install`.
- [ ] **Step 7: Commit:** `"Track agent state and zellij pane in the hook; add register-pen, add, close"`

### Task 3: Agent records and the board model

**Files:**
- Create: `src/state.rs`, `src/board.rs` (unit tests inline)

**Interfaces:**
- Consumes: the state-file fields from Task 2.
- Produces:
  - `state::AgentState` enum `Working|Waiting|Blocked|Ended|Closed`, deserialized from lowercase.
  - `state::AgentRecord { session: String, cwd: String, task: Option<String>, kind: Kind /*Task|Pen*/, agent: AgentState, state_at: i64, zellij_session: Option<String>, zellij_pane: Option<String>, repos: Vec<Repo{root,ref_}> }`.
  - `state::read_dir(dir: &Path) -> Vec<AgentRecord>`, which skips files that fail to parse and the `board-*.json` cache files.
  - `todoist::Task { id: String, content: String, description: String, labels: Vec<String>, section_id: Option<String>, child_order: i64 }`. Define it in `src/todoist.rs` now, with serde only; Task 4 adds the client.
  - `board::Section` enum `UpNext|InProgress|NeedsReview`.
  - `board::Row { task: Task, agent: Option<AgentRecord> }`.
  - `board::Board { up_next: Vec<Row>, in_progress: Vec<Row>, needs_review: Vec<Row>, writers: Vec<AgentRecord>, blocked: usize }`.
  - `board::build(tasks: &[Task], ids: &SectionIds, agents: &[AgentRecord]) -> Board`, with `SectionIds { up_next, in_progress, needs_review: String }`.
  - `board::age(now: i64, since: i64) -> String`.

Rules:
- Join on `agent.task == task.id`, ignoring `Closed` agents.
- If several agents claim one task, the one with the newest `state_at` wins.
- Up next is sorted by `child_order`.
- In progress and Needs review are sorted Blocked first, then by `state_at` descending; rows without an agent go last, by `child_order`.
- `writers` holds `kind == Pen` agents that aren't `Ended` or `Closed`.
- `blocked` counts Blocked agents across task rows and writers.
- Tasks in other sections or with no section are ignored.

- [ ] **Step 1: Write failing tests:**
  - `read_dir_parses_records`
  - `bad_state_file_is_skipped`: one valid file, one `{"session":` truncated file and one `board-abc.json`; result length is 1.
  - `blocked_sorts_first_and_is_counted`
  - `orphan_state_is_ignored`: an agent whose task isn't in the task list adds no row and isn't counted.
  - `closed_agent_not_joined`
  - `pen_writers_listed_until_ended`
  - `age_formats`: `age(1000,1000)=="now"`, `age(1000,940)=="1m"`, `age(10000,2800)=="2h"`, `age(200000,0)=="2d"`.
- [ ] **Step 2: Run `cargo test board state`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Commit:** `"Add agent records and board model"`

### Task 4: Todoist client

**Files:**
- Modify: `src/todoist.rs`

**Interfaces:**
- Consumes: `Config::token()`, `Task`, `SectionIds`.
- Produces:
  - `todoist::Client::new(token: String) -> Client`
  - `Client::resolve(&self, project: &str) -> anyhow::Result<(String /*project_id*/, SectionIds)>`. It finds the project and its three sections by exact name, without creating anything. On a missing project or section it returns an error whose message tells the user to run `hooks/claude-board.sh next` once to create it.
  - `Client::tasks(&self, project_id: &str) -> anyhow::Result<Vec<Task>>`, which follows `next_cursor` until it's null.
  - `Client::complete(&self, task_id: &str) -> anyhow::Result<()>`, using `POST /tasks/{id}/close`.
  - A pure `parse_page(body: &str) -> anyhow::Result<(Vec<Task>, Option<String>)>`.
- Use a 10 s timeout on every request.

- [ ] **Step 1: Write failing tests for `parse_page`** against a fixture copied from a real `/tasks` response (strip personal content):
  - `parse_page_reads_cursor`
  - `parse_page_null_cursor`
  - `parse_page_ignores_unknown_fields`

  Also add `#[ignore] live_resolve_finds_board` for running against the real board with `cargo test -- --ignored`.
- [ ] **Step 2: Run `cargo test todoist`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test && cargo test live_resolve -- --ignored`.** Expected: PASS.
- [ ] **Step 5: Commit:** `"Add Todoist client"`

### Task 5: zellij integration

**Files:**
- Create: `src/zellij.rs`

**Interfaces:**
- Consumes: `AgentRecord`, `AgentState`.
- Produces:
  - `zellij::current_session() -> Option<String>`, from env `ZELLIJ_SESSION_NAME`, and only when `ZELLIJ` is set.
  - `zellij::Jump` enum `Focus(String /*pane id*/) | Resume { session: String, cwd: String } | Elsewhere | NoAgent`.
  - `zellij::jump_target(agent: Option<&AgentRecord>, current: Option<&str>) -> Jump`, with these rules:
    - `Ended` → `Resume`
    - pane present and `zellij_session == current` → `Focus`
    - anything else with an agent → `Elsewhere`
    - none → `NoAgent`
  - `zellij::new_tab_cmd(name: &str, cwd: &str, claude_args: &[&str]) -> Vec<String>`, producing `["zellij","action","new-tab","-n",name,"-c",cwd,"--","claude",args...]`.
  - `zellij::focus_cmd(pane: &str) -> Vec<String>`
  - `zellij::back_cmd() -> Vec<String>`, producing `["zellij","action","go-to-previous-tab"]`. Run it after `new-tab` so focus stays on the board.
  - `zellij::run(cmd: &[String]) -> anyhow::Result<()>`
  - `zellij::spawn_claude(name: &str, cwd: &str, args: &[&str]) -> anyhow::Result<()>`. It returns `Err("not inside zellij")` when `current_session()` is None; otherwise it runs new_tab then back.

- [ ] **Step 1: Write failing tests:**
  - `jump_focus_same_session`
  - `jump_elsewhere_other_session`
  - `jump_elsewhere_no_pane`
  - `jump_resume_when_ended`: Ended wins even if a pane is recorded.
  - `jump_no_agent`
  - `new_tab_cmd_shape`: exact vec for `("next","/home/x",&["/next"])`.
  - `outside_zellij_is_refused`: with `ZELLIJ` unset, `spawn_claude` errors with "not inside zellij". Use a test-only env guard or factor `spawn_claude_in(current: Option<&str>, ...)`.
- [ ] **Step 2: Run `cargo test zellij`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Commit:** `"Add zellij jump and spawn helpers"`

### Task 6: App and UI

**Files:**
- Create: `src/app.rs`, `src/ui.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `app::App { board: Board, selected: (Section, usize), message: Option<String>, confirm: Option<Pending>, fetch_error: Option<String> }`.
  - `App::on_key(&mut self, key: KeyCode) -> Vec<Effect>`.
  - `Effect` enum `SpawnNext | SpawnPen | Jump(Jump) | Resume{session,cwd} | Complete{task_id, session: Option<String>} | OpenInTodoist(String) | Refresh | Quit`.
  - `App::on_tasks(&mut self, r: anyhow::Result<Vec<Task>>)`: on Err it sets `fetch_error` and keeps the old tasks.
  - `App::on_agents(&mut self, agents: Vec<AgentRecord>)`.
  - `ui::draw(f: &mut Frame, app: &App)`.

Keys:

| Key | Action |
|---|---|
| ←/→ or h/l | switch section |
| ↑/↓ or k/j | move within section |
| `n` | SpawnNext; if Up next is empty, message "Up next is empty" and no effect |
| `p` | SpawnPen |
| Enter | Jump for the selected row; `Resume` asks "Resume session? y/n" first; `Elsewhere` gives message "running elsewhere"; `NoAgent` gives message "no agent" |
| `x` | Complete; asks "Agent is <state> — complete anyway? y/n" first when the agent is Working or Blocked |
| `o` | OpenInTodoist (`xdg-open https://app.todoist.com/app/task/<id>`) |
| `r` | Refresh |
| `q` | Quit |

Main loop (`main.rs`):
- Set up crossterm raw mode and the alternate screen, and restore them on panic too.
- Run `Client::resolve` once. If it fails, show the error full-screen and quit on any key.
- A fetch thread sends `Result<Vec<Task>>` over an mpsc channel every 20 s and whenever it gets a refresh request on a second channel.
- The main loop polls keys with a 250 ms timeout and re-reads `state::read_dir` every 1 s.

Effect handling:
- **SpawnNext:** `spawn_claude("next", cwd, &["/next"])`, where cwd is the TUI's own cwd.
- **SpawnPen:** `spawn_claude("pen", cwd, &["/pen"])`.
- **Resume:** `spawn_claude("resume", cwd, &["--resume", session])`.
- **Complete:** `client.complete`, then run `hooks/claude-board.sh close <session>` (resolve the path via `~/.claude-work/hooks/claude-board.sh`), then refresh.
- **Errors** from any effect go to `message`.

Layout (builder's choice within this):
- The header shows `Claude Board`, `N blocked` (in red when N > 0) and the fetch error if any.
- Three columns: Up next, In progress, Needs review.
- Each row shows the task title, agent state, labels and age.
- A writers line at the bottom.
- A key-help footer.

- [ ] **Step 1: Write failing tests in `src/app.rs`**, building `Board` values directly:
  - `next_with_empty_queue_spawns_nothing`
  - `complete_working_agent_asks_first`: `x` gives no effects and sets `confirm`; then `y` gives `[Complete{..}]`.
  - `complete_waiting_agent_no_confirm`
  - `enter_on_ended_asks_to_resume`
  - `enter_elsewhere_sets_message`
  - `fetch_error_keeps_last_tasks`
  - `selection_clamps_when_rows_shrink`
- [ ] **Step 2: Write a failing test in `src/ui.rs`:** `header_shows_blocked_count`. Render into `ratatui::backend::TestBackend` 100×30 with 2 blocked agents and assert the buffer contains `2 blocked`.
- [ ] **Step 3: Run `cargo test app ui`.** Expected: FAIL.
- [ ] **Step 4: Implement `app.rs`, `ui.rs` and the main loop.**
- [ ] **Step 5: Run `cargo test && cargo clippy -- -D warnings`.** Expected: PASS with no warnings.
- [ ] **Step 6: Commit:** `"Add TUI app, rendering and main loop"`

### Task 7: Skills — `/next` on zellij, new `/pen`

**Files:**
- Modify: `skills/next/SKILL.md`
- Create: `skills/pen/SKILL.md`

**Interfaces:**
- Consumes: script modes `next`, `claim`, `register-pen`, `add`; `${CLAUDE_SESSION_ID}` substitution.

`/next pane`: if `$ZELLIJ` is set, run `zellij action new-tab -n next -c "$PWD" -- claude "/next"` then `zellij action go-to-previous-tab`. Otherwise, if `HERDR_ENV=1`, keep the existing herdr steps. Otherwise say it's not inside a multiplexer and offer to run here. Invoke the hook as `~/.claude-work/hooks/claude-board.sh` (the installed symlink).

`/pen`:
- Frontmatter description triggers on "/pen", "write tasks for the board", "turn this note/thread/issue into tasks".
- Steps:
  1. Run `register-pen ${CLAUDE_SESSION_ID} "$PWD"`.
  2. Ask what the source is, if it wasn't given.
  3. Read it: `gh issue view` / `gh pr view` for GitHub, the Slack MCP read tools for Slack links, or the note as pasted.
  4. Ask what's needed to write good tasks: which repo, and what "done" means.
  5. Propose tasks as a numbered list, each with a title (imperative, ≤ 80 chars), a description (context, the source link, a definition of done) and one repo label.
  6. **Only after the user confirms**, run `add` for each, in the agreed order.
  7. Report the ids.
- Never edit code in a `/pen` session.

- [ ] **Step 1: Write both skill files.**
- [ ] **Step 2: Run `cargo run -- install`** and confirm `~/.claude-work/skills/pen` is a symlink into the repo.
- [ ] **Step 3: Manual check, inside zellij.**
  - Run `claude "/pen"` in a scratch dir, give it the note "test task for the board, repo experiments", and confirm. It must ask before adding.
  - Then check the Up next tail in Todoist and delete the test task.
- [ ] **Step 4: Commit:** `"Add /pen skill; /next pane opens a zellij tab"`

### Task 8: End-to-end check in zellij

No new code unless a defect turns up; fixes go back into the owning task's files with a test.

- [ ] **Step 1: Run `cargo build --release && cargo run -- install`.** Run `target/release/claude-board` in a tab of the main zellij session.
- [ ] **Step 2:** Add a task to Up next in Todoist. Within 20 s (or on `r`), it shows.
- [ ] **Step 3:** Press `n`. A `next` tab opens, focus stays on the board, and within a few seconds the task moves to In progress with state `working`.
- [ ] **Step 4:** In that agent, trigger a permission prompt (e.g. ask it to run a command that isn't allowed). The board shows `1 blocked` and the row is red. Enter focuses that pane.
- [ ] **Step 5:** Let the agent finish. The row goes to Needs review with state `waiting`. Press `x`, then the task disappears and its state file shows `agent: "closed"`.
- [ ] **Step 6:** Press `p`. A `pen` tab opens and the writers line shows it. End that session: the writer disappears.
- [ ] **Step 7:** Run the TUI outside zellij. `n` shows "not inside zellij".
- [ ] **Step 8:** Run `bash tests/hook-state.sh && bash tests/smoke-todoist.sh && cargo test`. All pass.
- [ ] **Step 9: Commit** any fixes; update the memory note `~/.claude-work/projects/-home-johns/memory/project_claude_board_todoist.md` with the repo path and binary.
