# Claude Board Workstreams Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn Claude Board into the primary way to run Claude Code: a SQLite-backed board of workstreams with a cap on active work, a gated Review stage, Claude sessions in a zellij stack beside it, and a Todoist-like UI.

**Architecture:** One Rust binary, `claude-board`, is the only thing that touches `board.db`. It is the TUI (no args), the Claude Code hook handler (`hook <event>`), and the CLI the skills call (`add`, `next`, `claim`, `gate`, …). All stage changes go through one function, `Store::transition`, which enforces the cap and the gate inside a transaction. The TUI reads the store once a second, shells out to `zellij` for panes, polls `gh` for PR state, and optionally syncs with Todoist on background threads.

**Tech Stack:** Rust 2021, ratatui 0.30 + crossterm, rusqlite (`bundled`), ureq 3, serde/serde_json, anyhow. No clap; args are parsed by hand as today. zellij 0.45.1, `gh`.

**Spec:** `docs/2026-11-10-workstreams-design.md` (read it first; `docs/design.md` is the superseded first version).

## Global Constraints

- Work in `~/git-personal/experiments/claude-board/`, branch `claude-board`. Commit per task. **Never push.** Leave the untracked `hyperlocal/place-proxy/places.sqlite-*` files alone.
- Commit trailer on every commit: `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01GeJMCyFXmoZoz3uF1sooo2`.
- Only the binary reads or writes `board.db`. Database path: `$CLAUDE_BOARD_STATE/board.db`, default `~/.local/state/claude-board/board.db`. Explainers: `<state dir>/explainers/<task id>.html`.
- **Hooks never slow or break Claude:** `claude-board hook …` always exits 0, prints nothing to stdout, uses a 200 ms SQLite busy timeout, makes no network calls, and reads all of stdin before exiting.
- **Every path into Active checks the cap; every path into Review checks the gate.** Both live in `Store::transition` and nowhere else. Hooks never change a stage.
- Stages: `Queue`, `Active`, `Review`, `Parked`, `Done`, `Dropped`. `max_active` default `3`.
- Claude dir: `CLAUDE_CONFIG_DIR`, then `claude_dir` in `~/.config/claude-board/config.toml`, then `~/.claude` (already in `src/config.rs`). Panes are started with `env CLAUDE_CONFIG_DIR=<dir> claude …`.
- Todoist is off unless `todoist = true` in the config file. With it off, no token is read and no network call is made. Never print or log the token.
- The live system keeps working until Task 3 swaps the hooks: don't delete `hooks/claude-board.sh` before then.
- No new dependency beyond `rusqlite`.
- `cargo test` passes and `cargo build --release` is warning-free at the end of every task.

## Review Focus

1. **The database is locked, missing or corrupt when a hook fires** → the hook exits 0 within its timeout and Claude carries on. (Task 3 test `hook_survives_unusable_db`.)
2. **A recorded zellij pane no longer exists, or belongs to another zellij session** (zellij restarted; pane ids are reused) → the board treats the stream as having no pane and never focuses, closes or types into the wrong one. (Task 5 test `stale_pane_is_not_used`.)
3. **Two writers race for the last slot** (board key and `/next` in a pane, or two boards) → exactly one enters Active; the other gets `Refusal::CapFull`. (Task 1 test `last_slot_goes_to_exactly_one`.)
4. **Awkward titles and tiny terminals**: an 80-character single word, emoji/CJK, a title containing a newline, a pane 10 columns wide → cards wrap, nothing panics, nothing is cut off at normal widths. (Task 6 test `cards_survive_awkward_titles_and_sizes`.)
5. **`gh` is missing, logged out or offline, or the PR URL is nonsense** → PR state shows "unknown", the board keeps working, and the gate refuses with "couldn't verify PR <url>" instead of passing. (Task 2 test `gate_refuses_unverifiable_pr`; Task 8 test `gh_failure_is_unknown`.)

---

## File Structure

```
src/
  main.rs        arg dispatch only: tui | install | hook | cli subcommands
  config.rs      (modify) + max_active, todoist, todoist_project, gate_<kind>; db_path(), explainer_dir()
  model.rs       (new) Stage, AgentState, Task, Session, Pr, PrState, Event, Refusal
  store.rs       (new) Store: schema, queries, transition() — the cap lives here
  gate.rs        (new) the checks ("pr", "explainer") and evaluate()
  hook.rs        (new) `hook <event>`: stdin JSON → session rows
  cli.rs         (new) add, next, claim, register-pen, status, gate, drop, export, sync
  zellij.rs      (rewrite) pane commands and the stack
  pr.rs          (new) gh → PrState
  sync.rs        (new) optional Todoist sync
  todoist.rs     (keep) + create_task, add_comment, create_section
  app.rs         (rewrite) state, keys/mouse → effects
  ui/mod.rs      (new) draw(): picks wide or narrow form
  ui/theme.rs    (new) colours, glyphs
  ui/card.rs     (new) card widget + wrap()
  ui/wide.rs     (new) columns form
  ui/narrow.rs   (new) grouped list form
  ui/modal.rs    (new) task view, quick add, prompts
  install.rs     (modify) hooks → binary; ~/.local/bin link; prints zellij keybind
skills/next/SKILL.md, skills/pen/SKILL.md (modify), skills/wrap/SKILL.md (new)
delete: hooks/claude-board.sh, tests/hook-state.sh, tests/smoke-todoist.sh, src/state.rs, src/board.rs, src/ui.rs
```

---

### Task 1: Store, stages and the cap

**Files:**
- Create: `src/model.rs`, `src/store.rs`
- Modify: `Cargo.toml` (`rusqlite = { version = "0.37", features = ["bundled"] }`; use the newest 0.3x that resolves), `src/config.rs`, `src/main.rs` (`mod` lines)

**Interfaces — Produces:**

```rust
// model.rs
pub enum Stage { Queue, Active, Review, Parked, Done, Dropped }   // as_str()/from_str(): "queue","active",…
pub enum AgentState { Working, Waiting, Blocked, Ended }
pub enum SessionKind { Task, Pen }
pub struct Task { pub id: i64, pub title: String, pub description: String, pub kind: String, pub stage: Stage,
    pub position: f64, pub dir: Option<String>, pub labels: Vec<String>, pub explainer: Option<String>,
    pub todoist_id: Option<String>, pub created_at: i64, pub stage_at: i64 }
pub struct Session { pub id: String, pub task_id: Option<i64>, pub kind: SessionKind, pub cwd: String,
    pub state: AgentState, pub state_at: i64, pub zellij_session: Option<String>, pub zellij_pane: Option<String>,
    pub edited: bool, pub current: bool }
pub struct Repo { pub session_id: String, pub root: String, pub git_ref: String }
pub enum PrState { Unknown, Waiting, ChangesRequested, Approved, Merged, Closed }   // ordering used for "worst": ChangesRequested > Closed > Unknown > Waiting > Approved > Merged
pub struct Pr { pub task_id: i64, pub root: String, pub url: Option<String>, pub waiver: Option<String>, pub state: PrState, pub reviewers: String }
pub struct Event { pub task_id: i64, pub at: i64, pub kind: String, pub detail: String }   // kinds: "stage","summary","pr","comment","flag"
pub enum Refusal { CapFull { max: usize }, Gate(Vec<String>), NotAllowed { from: Stage, to: Stage }, NotMerged, NoSuchTask }
// Display for Refusal — exact copy:
//   CapFull      → "All {max} active slots are in use. Park or finish a stream first."
//   Gate(m)      → "Not ready for review: {m joined with '; '}"
//   NotAllowed   → "Can't move from {from} to {to}."
//   NotMerged    → "Not merged yet. Drop it if you want to abandon it."

// store.rs
pub struct NewTask<'a> { pub title: &'a str, pub description: &'a str, pub dir: Option<&'a str>, pub labels: &'a [String], pub todoist_id: Option<&'a str> }
impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Store>;            // creates dir + schema, WAL, busy_timeout 2000 ms
    pub fn open_with_timeout(path: &Path, ms: u64) -> anyhow::Result<Store>;
    pub fn open_in_memory() -> Store;                              // tests
    pub fn add_task(&self, t: NewTask) -> anyhow::Result<i64>;     // stage Queue, position = max+1
    pub fn task(&self, id: i64) -> anyhow::Result<Option<Task>>;
    pub fn tasks(&self) -> anyhow::Result<Vec<Task>>;              // all stages; Queue ordered by position, others by stage_at desc
    pub fn update_task(&self, id: i64, title: &str, description: &str, dir: Option<&str>, labels: &[String]) -> anyhow::Result<()>;
    pub fn set_position(&self, id: i64, position: f64) -> anyhow::Result<()>;
    pub fn active_count(&self) -> anyhow::Result<usize>;
    pub fn transition(&self, id: i64, to: Stage, max_active: usize, gate: &dyn Fn(&Store, &Task) -> Vec<String>) -> anyhow::Result<Result<(), Refusal>>;
    pub fn add_event(&self, task_id: i64, kind: &str, detail: &str) -> anyhow::Result<()>;
    pub fn events(&self, task_id: i64) -> anyhow::Result<Vec<Event>>;
    pub fn export_json(&self) -> anyhow::Result<serde_json::Value>;   // {"tasks":[…],"sessions":[…],"repos":[…],"prs":[…],"events":[…]}
}
// config.rs
pub max_active: usize   // file key `max_active`, default 3
pub fn db_path(&self) -> PathBuf; pub fn explainer_dir(&self) -> PathBuf;
```

Schema (tables `tasks`, `sessions`, `repos(session_id, root, git_ref, UNIQUE(session_id, root))`, `prs(task_id, root, url, waiver, state, reviewers, UNIQUE(task_id, root))`, `events`), versioned with `PRAGMA user_version = 1`. `labels` is a JSON array in a TEXT column. Session/repo/pr methods are added by the tasks that need them.

**Allowed transitions** (anything else is `NotAllowed`):

| From | To |
|---|---|
| Queue | Active (cap), Dropped |
| Active | Review (gate), Parked, Queue, Dropped |
| Parked | Active (cap), Queue, Dropped |
| Review | Active (cap), Done (only if every `prs` row is Merged or waived, else `NotMerged`), Dropped |
| Done, Dropped | nothing |

`transition` runs in one `BEGIN IMMEDIATE` transaction: read task, check table, check cap (`active_count() >= max_active` → `CapFull`) or call `gate` (non-empty → `Gate`), update `stage` and `stage_at`, insert a `"stage"` event with detail `"<from> → <to>"`. A task sent back to Queue goes to the bottom.

- [ ] **Step 1: Write failing tests** in `store.rs`:
  - `transitions_follow_the_table`: for every (from, to) pair of the six stages, with cap 99 and a gate returning `vec![]` and all PRs merged, assert `Ok(())` exactly for the pairs in the table and `NotAllowed` otherwise.
  - `every_way_into_active_checks_the_cap`: cap 1 with one Active task; Queue→Active, Parked→Active and Review→Active each return `CapFull { max: 1 }` and leave the stage unchanged.
  - `last_slot_goes_to_exactly_one`: a temp-file database, cap 1, two `Store` handles on two threads each moving a different Queue task to Active; exactly one `Ok(())`, one `CapFull`.
  - `review_needs_the_gate`: gate returning `vec!["no explainer".into()]` → `Refusal::Gate(["no explainer"])`, stage still Active.
  - `stage_change_is_logged`: after Queue→Active, `events()` has one `"stage"` event with detail `"queue → active"`.
  - `queue_order_and_requeue`: three added tasks come back in insertion order; `set_position` reorders; Active→Queue lands last.
  - `refusal_messages`: the four `Display` strings above, verbatim.
  - `export_has_every_table`: keys `tasks, sessions, repos, prs, events`.
- [ ] **Step 2:** `cargo test store::` → fails to compile (no `Store`).
- [ ] **Step 3:** Implement `model.rs`, `store.rs`, and the config additions.
- [ ] **Step 4:** `cargo test` → all pass (existing tests untouched).
- [ ] **Step 5:** Commit: `claude-board: SQLite store, stages and the active cap`.

---

### Task 2: Sessions, repos, PRs and the Review gate

**Files:**
- Create: `src/gate.rs`
- Modify: `src/store.rs`, `src/config.rs`, `src/model.rs`

**Interfaces — Consumes:** Task 1's `Store`, `Refusal`, `Pr`, `PrState`. **Produces:**

```rust
// store.rs
pub struct SessionUpdate<'a> { pub id: &'a str, pub cwd: &'a str, pub state: AgentState, pub zellij_session: Option<&'a str>, pub zellij_pane: Option<&'a str> }
impl Store {
    pub fn session(&self, id: &str) -> anyhow::Result<Option<Session>>;
    pub fn sessions(&self) -> anyhow::Result<Vec<Session>>;
    pub fn touch_session(&self, u: SessionUpdate, create: bool) -> anyhow::Result<bool>;  // false if absent and !create; state_at changes only when state changes
    pub fn link_session(&self, session: &str, task_id: i64, cwd: &str) -> anyhow::Result<()>;  // creates if needed; becomes the task's current session
    pub fn register_pen(&self, session: &str, cwd: &str) -> anyhow::Result<()>;
    pub fn mark_edited(&self, session: &str, root: &str, git_ref: &str) -> anyhow::Result<()>;  // edited=1, upsert repo
    pub fn task_repos(&self, task_id: i64) -> anyhow::Result<Vec<String>>;   // distinct roots across the task's sessions
    pub fn set_pr(&self, task_id: i64, root: &str, url: Option<&str>, waiver: Option<&str>) -> anyhow::Result<()>;
    pub fn set_pr_state(&self, task_id: i64, root: &str, state: PrState, reviewers: &str) -> anyhow::Result<bool>;  // true if changed; logs a "pr" event when it does
    pub fn prs(&self, task_id: i64) -> anyhow::Result<Vec<Pr>>;
    pub fn set_explainer(&self, task_id: i64, path: &str) -> anyhow::Result<()>;
    pub fn untracked(&self) -> anyhow::Result<Vec<Session>>;   // kind Task, task_id NULL, edited, state != Ended
}
// gate.rs
pub type PrVerifier = dyn Fn(&str) -> bool;            // url → exists; production: `gh pr view <url>` exit status
pub fn evaluate(store: &Store, task: &Task, checks: &[String], verify_pr: &PrVerifier) -> Vec<String>;
pub fn worst(prs: &[Pr]) -> Option<PrState>;           // by the ordering in model.rs; waived rows count as Merged
pub fn ready_to_close(prs: &[Pr]) -> bool;             // all Merged or waived (true for no rows)
// config.rs
pub fn gate_checks(&self, kind: &str) -> Vec<String>;  // file key `gate_<kind>`, comma-separated; default "pr,explainer"
```

Checks and their exact missing-messages:
- `pr`: for each root in `task_repos`: no `prs` row → `"no PR for <basename of root>"`; row with a URL that `verify_pr` rejects → `"couldn't verify PR <url>"`; a waiver (non-empty reason) passes.
- `explainer`: `task.explainer` unset or the file doesn't exist → `"no explainer"`.
- An unknown check name → `"unknown check <name>"` (so a config typo can't open the gate).

- [ ] **Step 1: Failing tests** in `gate.rs`:
  - `gate_passes_with_pr_and_explainer`; `gate_names_each_missing_piece` (two repos, one PR set → `["no PR for b", "no explainer"]`).
  - `waiver_stands_in_for_a_pr`; `gate_refuses_unverifiable_pr` (verifier returns false → `["couldn't verify PR https://x/pr/1"]`).
  - `unknown_check_fails_closed`.
  - `worst_state_across_repos`: Approved + ChangesRequested → ChangesRequested; Merged + waived → Merged; `ready_to_close` true only for the second.
  - `review_to_done_needs_merge` (store): Review→Done is `NotMerged` with a Waiting PR, `Ok` once Merged.
  - `state_at_only_moves_on_change` (store): two `touch_session` calls with the same state keep `state_at`.
  - `untracked_lists_only_edited_unlinked_live_sessions`.
- [ ] **Step 2:** `cargo test gate::` → compile failure.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test` passes.
- [ ] **Step 5:** Commit: `claude-board: sessions, per-repo PRs and the review gate`.

---

### Task 3: Hooks in the binary; installer; retire the bash hook

**Files:**
- Create: `src/hook.rs`
- Modify: `src/main.rs`, `src/install.rs`
- Delete: `hooks/claude-board.sh`, `tests/hook-state.sh`, `tests/smoke-todoist.sh`

**Interfaces — Consumes:** `Store::{open_with_timeout, touch_session, mark_edited, session, add_event}`. **Produces:**

```rust
// hook.rs
pub fn run(event: &str, cfg: &Config) -> ();                         // reads stdin to EOF first; swallows every error; main exits 0
pub fn apply(store: &Store, event: &str, input: &serde_json::Value, zellij: (Option<&str>, Option<&str>)) -> anyhow::Result<()>;
```

Events (`claude-board hook <event>`), all sync, timeout 5 s, in `install::HOOKS`:

| Claude hook | matcher | event | effect |
|---|---|---|---|
| UserPromptSubmit | – | `working` | touch existing session → Working |
| PostToolUse | `*` | `working` | same |
| PostToolUse | `Edit\|Write\|MultiEdit\|NotebookEdit` | `edit` | create the session if absent (kind Task, no task); `mark_edited` with the repo root and ref of the edited file |
| Stop | – | `waiting` | → Waiting; if the session has a task and `last_assistant_message` is non-empty, add a `"summary"` event with its last 1500 bytes (cut on a char boundary) |
| Notification | `permission_prompt\|elicitation_dialog` | `blocked` | → Blocked |
| SessionEnd | – | `ended` | → Ended |

- `working`/`waiting`/`blocked`/`ended` only touch sessions that already exist (question-only sessions stay off the board). Pen sessions ignore `edit`.
- Repo root and ref: port `repo_info` from `hooks/claude-board.sh` (read it before deleting; it handles git and jj).
- Session id from `.session_id`, cwd from `.cwd`, file from `.tool_input.file_path // .tool_input.notebook_path`. zellij pane from env `ZELLIJ_SESSION_NAME` / `ZELLIJ_PANE_ID` (store the pane as `terminal_<id>`), refreshed on every event.
- Installer: link `~/.local/bin/claude-board` → `<repo>/target/release/claude-board` (same backup rules as other links); hook command is `<that link's absolute path> hook <event>`; `is_board` also matches the old `claude-board.sh` entries so they're replaced; remove the stale `hooks/claude-board.sh` link from the Claude dir; link `skills/*` as today. Print the zellij keybinding to add (Task 5 defines it).

- [ ] **Step 1: Failing tests** in `hook.rs` (drive `apply` with `Store::open_in_memory()`):
  - `first_edit_creates_untracked_session_with_repo`; `second_repo_is_added_once`.
  - `state_events_ignore_unknown_sessions`.
  - `stop_records_summary_for_linked_session` (and not for an unlinked one); a 4000-byte multibyte message is cut to ≤ 1500 bytes without panicking.
  - `pen_session_ignores_edit`.
  - `hook_survives_unusable_db`: spawn the built binary (`env!("CARGO_BIN_EXE_claude-board")` from `tests/hook_cli.rs`) with `CLAUDE_BOARD_STATE` pointing at a directory where `board.db` is a directory, feed it JSON on stdin → exit status 0, empty stdout, returns within 2 s. Also: garbage stdin → exit 0.
  - `install.rs`: update the existing merge tests for the new command shape; add `merge_replaces_old_bash_entries`.
- [ ] **Step 2:** `cargo test hook` → fails.
- [ ] **Step 3:** Implement; wire `Some("hook")` in `main.rs`; delete the three files.
- [ ] **Step 4:** `cargo test` passes; `cargo build --release`.
- [ ] **Step 5:** Run `./target/release/claude-board install` and check `~/.claude-work/settings.json` (or the configured Claude dir) has only the six new entries for the board and the previous file was backed up. Then, in this session, run any Edit and confirm with `sqlite3 ~/.local/state/claude-board/board.db 'select id,state,edited from sessions'` (or `claude-board export` once Task 4 exists) that a row appeared.
- [ ] **Step 6:** Commit: `claude-board: hooks handled by the binary; drop the bash hook`.

---

### Task 4: CLI and skills

**Files:**
- Create: `src/cli.rs`, `skills/wrap/SKILL.md`
- Modify: `src/main.rs`, `skills/next/SKILL.md`, `skills/pen/SKILL.md`

**Interfaces — Consumes:** Tasks 1–2. **Produces** (stdout is JSON where noted; refusals print the `Refusal` message to stderr and exit 1):

| Command | Does |
|---|---|
| `add TITLE [--desc D] [--dir DIR] [--label L]…` | add to Queue; prints the id. Values may start with `-` (everything after a flag's name is its value). |
| `next` | prints the top Queue task as JSON `{id,title,description,dir,labels}`; exit 1 with "Queue is empty" if none |
| `claim SESSION TASK_ID CWD` | `transition(→Active)` if the task is in Queue or Parked (a task already Active is just linked); `link_session`; prints the task JSON |
| `register-pen SESSION CWD` | registers a writer session |
| `status SESSION` | prints `{task:{…}, repos:[root…], prs:[…], missing:[…]}` for the session's task; exit 1 if the session has no task |
| `gate SESSION [--pr ROOT=URL]… [--no-pr ROOT=REASON]… [--explainer FILE] [--summary TEXT]` | records each PR/waiver; copies FILE to `explainer_dir()/<task id>.html` and records it; adds a `"summary"` event; then `transition(→Review)` with the real gate (`gh pr view URL` as verifier). Success prints "In review."; refusal prints the missing list and exits 1 |
| `drop TASK_ID` | `transition(→Dropped)` |
| `export` | prints `export_json()` |

`cli::run(args: &[String], cfg: &Config) -> anyhow::Result<i32>` (exit code). `gate` takes the verifier as a parameter internally so tests can stub it.

Skills (keep each skill's existing safety wording):
- **next**: commands become `claude-board next` / `claude-board claim ${CLAUDE_SESSION_ID} <id> "$PWD"`. Drop the herdr branch. `/next pane` is removed (the board starts sessions). If `claim` is refused (cap), tell the user what it printed and stop.
- **pen**: `claude-board register-pen …`, `claude-board add "$title" --desc "$desc" --label '<repo>' [--dir …]`. The confirm-before-adding rules and heredoc quoting stay.
- **wrap** (new; `description`: wrap up this workstream for review): 1) `claude-board status ${CLAUDE_SESSION_ID}` to get repos and what's missing; 2) for each repo decide whether a PR is appropriate; if yes open it with that repo's normal workflow (e.g. `/finish-feature`) and note the URL, if no write a one-line reason; 3) write a self-contained HTML explainer of the change for a reviewer (what changed, why, how to verify, risks) to a temp file; 4) `claude-board gate ${CLAUDE_SESSION_ID} --pr … --no-pr … --explainer <file> --summary "<two sentences>"`; 5) if it prints missing items, fix them and run it again; never claim review-ready until it prints "In review.".

- [ ] **Step 1: Failing tests** in `cli.rs` (in-memory store, call the command functions directly):
  - `add_accepts_dash_values` (`--desc "- a list"`), `next_on_empty_queue_exits_1`.
  - `claim_respects_the_cap` (exit 1, message is the `CapFull` text, session not linked); `claim_links_and_activates`; `claim_of_parked_task_resumes_it`.
  - `gate_moves_to_review_and_copies_explainer`; `gate_lists_what_is_missing` (exit 1, task still Active); `gate_with_waiver_enters_review`.
  - `status_reports_missing`.
- [ ] **Step 2:** `cargo test cli::` → fails.
- [ ] **Step 3:** Implement; update the three skills.
- [ ] **Step 4:** `cargo test`; `cargo build --release`; `claude-board add "plan smoke test" && claude-board next && claude-board export | head -c 300`, then `claude-board drop <id>`.
- [ ] **Step 5:** Commit: `claude-board: CLI for skills; /wrap skill; /next and /pen use the binary`.

---

### Task 5: zellij stack

**Files:** Rewrite `src/zellij.rs`.

Verified on zellij 0.45.1 (headless spike, 2026-11-10): `zellij run --name N --cwd D --no-focus [--direction right] -- CMD` prints the new pane id; `zellij action stack-panes -- A B C` stacks them; `focus-pane-id X` then `focus-pane-id <board>` leaves X expanded in the stack; `close-pane`, `toggle-fullscreen`, `write-chars` and `write` all take `--pane-id`. **Not verified** (needs a real client): fullscreen behaviour and typing `/wrap` + Enter into a pane; Step 4 checks them.

**Interfaces — Produces:**

```rust
pub struct Zellij { pub session: Option<String>, pub board_pane: Option<String> }   // from ZELLIJ, ZELLIJ_SESSION_NAME, ZELLIJ_PANE_ID
pub trait Runner { fn run(&self, cmd: &[String]) -> anyhow::Result<String>; }        // stdout; real impl shells out, tests record
impl Zellij {
    pub fn from_env() -> Zellij;
    pub fn live_panes(&self, r: &dyn Runner) -> anyhow::Result<Vec<String>>;         // `list-panes`, terminal ids only
    /// A session's pane, only if it's in this zellij session and still exists.
    pub fn pane_of<'a>(&self, s: &'a Session, live: &[String]) -> Option<&'a str>;
    pub fn open(&self, r: &dyn Runner, name: &str, cwd: &str, claude_dir: &str, claude_args: &[&str], stack: &[String]) -> anyhow::Result<String>;
        // run … -- env CLAUDE_CONFIG_DIR=<dir> claude <args>; first pane uses --direction right; then stack-panes with `stack` + the new id; refocus board. Returns the pane id.
    pub fn show(&self, r: &dyn Runner, pane: &str) -> anyhow::Result<()>;            // focus pane, focus board
    pub fn enter(&self, r: &dyn Runner, pane: &str) -> anyhow::Result<()>;           // focus pane
    pub fn fullscreen(&self, r: &dyn Runner, pane: &str) -> anyhow::Result<()>;      // focus + toggle-fullscreen --pane-id
    pub fn toggle_board_fullscreen(&self, r: &dyn Runner) -> anyhow::Result<()>;     // toggle-fullscreen --pane-id <board>
    pub fn close(&self, r: &dyn Runner, pane: &str) -> anyhow::Result<()>;
    pub fn type_line(&self, r: &dyn Runner, pane: &str, text: &str) -> anyhow::Result<()>;   // write-chars text, then `write 13`
}
pub const RETURN_KEYBIND: &str;   // KDL snippet: bind "Alt b" { ToggleFocusFullscreen; MoveFocus "Left"; } for the locked/normal modes; printed by install
```

Every method errors with `"not inside zellij"` when `session` or `board_pane` is `None`. Pane names are the task title cut to 30 chars.

- [ ] **Step 1: Failing tests** with a recording `Runner`:
  - `open_first_pane_goes_right_and_refocuses_board` (exact argv sequence); `open_later_pane_joins_the_stack` (`stack-panes -- terminal_1 terminal_2 terminal_5`).
  - `stale_pane_is_not_used`: `pane_of` is `None` when the session's `zellij_session` differs from ours, when the pane isn't in `live`, or when it has no pane.
  - `outside_zellij_is_refused`.
  - `type_line_sends_text_then_enter`.
- [ ] **Step 2:** `cargo test zellij::` → fails.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Manual check, by the user in a real zellij session (ask them; you can't drive a client): a scratch `cargo run --example stack_spike` (or a `#[test] #[ignore]`) that opens two `cat` panes, shows each, fullscreens one, types a line into one. Confirm: fullscreen works, the typed line arrives with Enter, and the `Alt b` binding returns to the board. **If fullscreen or the return key doesn't work, stop and report** — don't build Task 6's keys on it.
- [ ] **Step 5:** Commit: `claude-board: zellij session stack`.

---

### Task 6: App core and the narrow board

**Files:**
- Rewrite: `src/app.rs`, `src/main.rs` (TUI loop)
- Create: `src/ui/mod.rs`, `src/ui/theme.rs`, `src/ui/card.rs`, `src/ui/narrow.rs`
- Delete: `src/ui.rs`, `src/board.rs`, `src/state.rs`

**Interfaces — Consumes:** Store (Tasks 1–2), `Zellij` (Task 5). **Produces:**

```rust
// app.rs
pub enum Group { Active, Review, Parked, Queue, Untracked }
pub struct Card { pub task: Option<Task>, pub session: Option<Session>, pub prs: Vec<Pr>, pub missing: Vec<String>, pub flag: Option<String> }
    // Untracked cards have task None. flag: "working outside Active" | "changes requested, waiting for a slot" | "ready to close" | "PR closed"
pub struct Snapshot { pub groups: Vec<(Group, Vec<Card>)>, pub active: usize, pub max_active: usize, pub blocked: usize, pub writers: Vec<Session> }
pub fn snapshot(store: &Store, cfg: &Config) -> anyhow::Result<Snapshot>;   // Active sorted blocked first, then waiting, then working
pub enum Effect { Start(i64), Resume(i64), Park(i64), Requeue(i64), ToReview(i64), Done(i64), Drop(i64), Adopt { session: String, title: String },
    Show(i64), Enter(i64), Fullscreen(i64), ToggleBoardWide, Wrap(i64), SpawnPen, OpenExplainer(i64), OpenPr(i64), Add { title: String, dir: Option<String>, labels: Vec<String> }, Quit }
pub struct App { pub snap: Snapshot, pub selected: Option<CardKey>, pub message: Option<String>, pub prompt: Option<Prompt>, pub modal: Option<i64>, … }
impl App { pub fn on_key_event(&mut self, k: KeyEvent) -> Vec<Effect>; pub fn on_mouse(&mut self, m: MouseEvent) -> Vec<Effect>; pub fn set_snapshot(&mut self, s: Snapshot); }
// ui/card.rs
pub fn wrap(s: &str, width: usize) -> Vec<String>;                 // move from the old ui.rs, with its tests; newlines in s count as spaces
pub fn card_lines(c: &Card, width: usize, selected: bool, now: i64) -> Vec<Line<'static>>;   // rounded border ╭╮╰╯, state circle, full wrapped title, status line, label chips, flag/missing lines
// ui/mod.rs
pub const WIDE_MIN: u16 = 100;
pub fn draw(f: &mut Frame, app: &App) -> Vec<Hit>;                 // narrow form below WIDE_MIN columns, wide form (Task 7) at or above
```

Behaviour:
- Selection follows the card (by task id or session id) across refreshes, as the current build does.
- Keys (footer shows them): `↑↓/jk` card, `←→/hl` group (narrow: previous/next group), `n` start the top of Queue, `s` start/resume the selected Queue or Parked card, `P` park, `w` wrap up, `H`/`L` or Shift+←/→ move one stage along `Queue ↔ Active ↔ Review` (`L` from Review = Done), `x` Done, `D` drop, `a` adopt (Untracked), `⏎` go to the session, `f` session full screen, `v` board wide/narrow, `p` pen, `e` explainer, `o` PR, `space` task view (Task 7), `+` quick add (Task 7), `q`/Ctrl-C quit.
- Confirms (y/n, naming the task): Drop always; Park and Done when the agent is Working or Blocked.
- Every stage effect goes through `Store::transition`; a `Refusal` becomes `app.message` verbatim. `ToReview` from the board uses the same gate as the CLI, so a card can't be moved into Review past it.
- Effects in `main.rs`:
  - `Start`/`Resume`: need the task's dir; if unset, open a prompt "Directory for '<title>':" pre-filled with the most recent other task's dir, save it with `update_task`. Then `transition(→Active)`; on success `Zellij::open` with `/next <id>` (Start) or `--resume <current session id>` (Resume; falls back to `/next <id>` if the task has no session). On refusal nothing is opened.
  - `Park`: `transition(→Parked)`, then close its live pane.
  - On every 1 s tick: close the live pane of any Review, Parked, Done or Dropped task whose session is not Working (this is how a wrapped-up stream's pane goes away after its turn ends). Set flag "working outside Active" when such a session is Working.
  - `Show` is emitted when the selection changes to a card with a live pane, in the narrow form only.
  - `Wrap`: `type_line(pane, "/wrap")`; with no live pane, message "No live session. Resume it first."
  - `Adopt`: prompt for a title (pre-filled with the cwd's last component), `add_task` + `transition(→Active)` (refused at cap → offer nothing else; message is the `CapFull` text) + `link_session`.
- Theme (`ui/theme.rs`): accent `Color::Rgb(220, 76, 62)` (Todoist red) for selection and the header count when blocked; working green, waiting yellow, blocked red, dim `DarkGray` for secondary text; label chips as ` label ` on a dim background. Circles: `◉` live session, `○` none.
- Narrow form: one scrolling column of groups in the order Active, Review, Parked, Queue, Untracked; group header `ACTIVE 2/3`, `REVIEW 1`, …; empty groups show the header only. Header line: `Claude Board` + `2/3 active · 1 blocked`. Writers line and footer as today.
- The loop keeps today's structure: draw → poll 250 ms → effects; snapshot re-read every 1 s; slow work (`gh`, Todoist) stays off the UI thread via the existing `background`/`Update` channel pattern.

- [ ] **Step 1: Failing tests**:
  - `app.rs`: `start_at_cap_is_refused_and_opens_nothing` (effect handler is given a stub runner; asserts no zellij command and the `CapFull` message); `move_right_from_active_hits_the_gate`; `move_right_from_review_needs_merge`; `drop_asks_first`; `park_asks_when_working`; `selection_follows_card_across_refresh`; `snapshot_sorts_blocked_first_and_counts`; `snapshot_flags` (each of the four flags from its cause); `untracked_card_can_be_adopted`; `ctrl_c_quits_even_while_confirming`.
  - `ui/card.rs`: the moved `wrap` tests; `cards_survive_awkward_titles_and_sizes` — render cards with an 80-char single word, `"修复 登录 超时 🚀"`, a title containing `"\n"`, at widths 10, 24 and 60 and a 10×5 terminal: no panic; at width 24 every character of the long word appears.
  - `ui/narrow.rs`: `narrow_shows_groups_counts_and_full_titles` (TestBackend 40×40: `ACTIVE 1/3`, every word of a long title, the hits cover each card).
- [ ] **Step 2:** `cargo test` → fails.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test`; `cargo build --release` with no warnings; run the board outside zellij and confirm it draws and `n` says "not inside zellij".
- [ ] **Step 5:** Commit: `claude-board: workstream app core and narrow board`.

---

### Task 7: Wide board, task view, quick add, mouse

**Files:**
- Create: `src/ui/wide.rs`, `src/ui/modal.rs`
- Modify: `src/ui/mod.rs`, `src/app.rs`

**Interfaces — Consumes:** Task 6's `App`, `Card`, `card_lines`, `Hit`. **Produces:** `ui::wide::draw`, `ui::modal::{draw_task, draw_quick_add, draw_prompt}`, `app::parse_quick_add(&str) -> (String, Option<String>, Vec<String>)`.

- **Wide form** (≥ `WIDE_MIN` columns), matching the sketch in the spec: four equal columns `Active n/max`, `Review n`, `Parked n`, `Queue n`, cards with a blank line between them, each column scrolling independently to keep its selection visible; ` + Add task` in dim text under the Queue (clicking it or `+` opens quick add); Untracked appears as a fifth, narrower column only when non-empty; header rule under the title line.
- **Task view** (`space`): centred modal, 80% of the screen. Left two-thirds: title (bold), description, then the thread — every `events` row, oldest first, as `<age> <kind>: <detail>`. Right third, a side panel: Stage, Directory, Repos, PRs (each with state and reviewers), Explainer (path or "none"), Session (id, state), Created. `↑↓` scroll, `esc` closes; `e`, `o`, `w`, `P`, `x`, `D` work from inside it.
- **Quick add** (`+`): one-line box at the bottom of the board. `@word` → label, `#path` → directory (`~` expanded), everything else is the title. As the user types, a preview line under the box shows the parsed title, labels as chips, and the directory. `⏎` adds to the bottom of the Queue; `esc` cancels; an empty title adds nothing.
- **Mouse**: click selects; dragging a card onto another column (wide) or group header (narrow) requests that stage through the same `transition`, so refusals show in the footer; the target highlights while dragging; wheel scrolls the column under the pointer, or the task view when open.

- [ ] **Step 1: Failing tests**:
  - `parse_quick_add`: `"Fix login @delta @api #~/git-work/delta"` → (`"Fix login"`, `Some("<home>/git-work/delta")`, `["delta","api"]`); `"email me@example.com"` keeps the address in the title (`@` only counts at the start of a word); `"   "` → empty title.
  - `wide_shows_four_columns_with_counts` (TestBackend 140×40); `untracked_column_only_when_present`; `wide_cards_show_full_titles`.
  - `task_view_shows_thread_and_side_panel`; `quick_add_preview_shows_parsed_parts`; `empty_quick_add_adds_nothing`.
  - `drag_to_review_is_refused_by_gate` (message is the `Gate` text; card stays); `drag_to_parked_moves`.
- [ ] **Step 2:** `cargo test` → fails.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test`; `cargo build --release`.
- [ ] **Step 5:** Commit: `claude-board: wide board, task view, quick add, mouse`.

---

### Task 8: PR state from GitHub

**Files:** Create `src/pr.rs`; modify `src/main.rs`.

**Interfaces — Produces:**

```rust
pub fn parse(json: &str) -> (PrState, String);        // from `gh pr view URL --json state,reviewDecision,reviewRequests`
pub fn fetch(url: &str) -> (PrState, String);         // any gh failure → (PrState::Unknown, "")
pub fn exists(url: &str) -> bool;                     // the gate's verifier
```

Mapping: `state` MERGED → Merged; CLOSED → Closed; OPEN with `reviewDecision` CHANGES_REQUESTED → ChangesRequested, APPROVED → Approved, otherwise Waiting. Reviewers: logins/names from `reviewRequests`, comma-joined.

A background thread refreshes every Review task's PRs every 120 s and right after a task enters Review, calling `set_pr_state`. `snapshot` turns the states into card text and flags: Waiting → `⧗ waiting on <reviewers>` (or `⧗ waiting on review`), ChangesRequested → flag "changes requested, waiting for a slot", Closed → flag "PR closed", all merged/waived → flag "ready to close".

- [ ] **Step 1: Failing tests**: `parse` for the five outcomes from fixture JSON in `tests/fixtures/pr_*.json`; `gh_failure_is_unknown` (`fetch` with `PATH` emptied → `Unknown`, no panic); `exists` false for `"not a url"`.
- [ ] **Step 2:** `cargo test pr::` → fails. **Step 3:** Implement. **Step 4:** `cargo test`.
- [ ] **Step 5:** Commit: `claude-board: PR state on review cards`.

---

### Task 9: Optional Todoist sync

**Files:** Create `src/sync.rs`; modify `src/todoist.rs` (`create_task`, `add_comment`, `create_section`, reuse `move_task`/`complete`), `src/config.rs` (`todoist: bool`, `todoist_project`), `src/store.rs` (`synced_stage` column via `user_version = 2` migration), `src/main.rs`, `src/cli.rs` (`sync` subcommand).

**Interfaces — Produces:**

```rust
pub trait Remote { /* tasks, create_task, move_task, complete, add_comment — implemented by todoist::Client and a test fake */ }
pub fn sync_once(store: &Store, remote: &dyn Remote, sections: &Sections) -> anyhow::Result<SyncReport>;
```

Rules (the board owns stages; Todoist owns new tasks and queue order):
- **Pull**: a Todoist task in "Up next" with an unknown id → new Queue task. Queue order of synced tasks follows Todoist's `child_order`. **First run only** (no task has a `todoist_id` yet): tasks in "In progress" or "Needs review" import as **Parked**. After that, a Todoist-side section change is ignored and corrected on the next push.
- **Push**: a local task with no `todoist_id` is created there. When `stage != synced_stage`: Queue → "Up next", Active → "In progress", Review → "Needs review", Parked → "Parked" (section created if missing), Done/Dropped → completed. On entering Review, add one comment with the PR URLs and the explainer path.
- Runs every 30 s on a background thread, only when `todoist = true`. Errors go to the footer; the board keeps working.

- [ ] **Step 1: Failing tests** with a fake `Remote`: `pull_adds_new_up_next_tasks`; `queue_order_follows_todoist`; `first_run_imports_in_progress_as_parked`; `later_remote_stage_change_is_ignored_and_corrected`; `push_moves_and_completes`; `review_gets_one_comment`; `sync_disabled_makes_no_calls` (with `todoist = false` the thread is never started and `Config::token` is never read).
- [ ] **Step 2:** `cargo test sync::` → fails. **Step 3:** Implement. **Step 4:** `cargo test`.
- [ ] **Step 5:** Commit: `claude-board: optional Todoist sync`.

---

### Task 10: Migration and hands-on run-through (with the user)

- [ ] **Step 1:** Update `docs/design.md` with a one-line pointer to the new spec; update the README-level notes in the spec's "Kept, changed, dropped" if anything changed during the build.
- [ ] **Step 2:** With the user: set `todoist = true`, run `claude-board sync`, confirm the five existing tasks arrive as Parked. Old `~/.local/state/claude-board/*.json` files are left in place, unused.
- [ ] **Step 3:** Run-through in the user's zellij session: add via quick add; `n` starts a stream beside the board; selecting cards switches the pane; `f` and `Alt b`; fill the cap and see the refusal; park and resume; `/wrap` to Review (gate refusing first, then passing); explainer opens with `e`; an untracked session appears and is adopted; drop. Fix what breaks.
- [ ] **Step 4:** `cargo test`, `cargo build --release`, commit. Don't push; ask.
