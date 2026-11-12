# Spike: a pane-control plugin bundled into the board's zellij distribution

Board task 24, 2026-11-12. Follows task 22
([2026-11-12-zellij-distribution-spike.md](2026-11-12-zellij-distribution-spike.md)).
Question: can a wasm plugin bundled into a zellij 0.46 distribution replace
the CLI pane control in `src/zellij.rs`?

## Verdict: go

All three checks passed in a session started from the throwaway distribution
`cbdist` with the plugin bundled as `zellij:cbctl`. `~/.config/zellij` was
checksummed before and after the run and did not change.

1. **Open and get the pane id back.** `cbdist pipe --plugin zellij:cbctl
   --name open --args name=agent2,stack=terminal_2 -- "<command>"` prints
   `terminal_3` and exits 0, in about 25 ms. The first agent lands right of
   the board and later ones stack on it. The board keeps focus.
2. **Events without polling.** A long-lived `cbdist pipe ... --name events`
   prints one line per event as it happens: `opened terminal_3`,
   `focused terminal_1` (when the user moved there with Alt+→),
   `focused terminal_0` (after `Alt b`), `exited terminal_2 3`.
3. **Write without moving focus.** `--name write --args pane=terminal_2 --
   "/wrap"` typed `/wrap` and Enter into the agent. The agent read it and
   exited with code 3, and the attached client's focus stayed on the board
   throughout (checked with `action list-clients`).

`src/zellij.rs` can become a thin client: one `cbdist pipe` call per action
plus one long-lived `events` child, in place of `list-panes` polling, the
open, stack and focus-back sequence, and matching the "already focused"
error text.

The spike code (`cbplugin` and `cbdist`) became `claude-board/wyrcan/`: the
plugin is `wyrcan/plugin`, the distribution is `wyrcan`.

## Plugin API calls used (zellij-tile 0.46)

| Need | Call |
|---|---|
| Open the first agent right of the board | `open_command_pane_in_place_of_pane_id(placeholder, cmd, close_replaced=true, ctx)`. The layout puts a `cat` pane titled `agents` right of the board. The plugin finds it in `PaneUpdate` and replaces it. Returns `Option<PaneId>` straight away. |
| Open later agents | `open_command_pane_background(cmd, ctx)`, then `show_pane_with_id(id, false, false)`. Returns `Option<PaneId>`. |
| Join the stack | `stack_panes(vec![existing.., new])` |
| Keep focus where it was | `focus_pane_with_id(prev, false, false)` after stacking, because stacking focuses the new pane. No error when the pane is already focused. |
| Name the pane | `rename_pane_with_id(id, name)` |
| Type a prompt | `write_chars_to_pane_id(text, id)`, then `write_to_pane_id(vec![13], id)` for Enter |
| Events | `subscribe(&[PaneUpdate, CommandPaneOpened, CommandPaneExited, PaneClosed])` |
| Which pane the user is on | `get_focused_pane_info()` on each `PaneUpdate`. Not `PaneInfo.is_focused`, which is per layer, so the open pane of a stack also reads as focused. |
| Reply to a CLI caller | `cli_pipe_output(pipe_id, text)`, `set_cli_pipe_exit_code(pipe_id, 1)` on error, `unblock_cli_pipe_input(pipe_id)` |
| Keep a CLI caller attached | `block_cli_pipe_input(pipe_id)` |

The plugin is loaded headless with `load_plugins { "zellij:cbctl" }` in the
bundled config. As a bundled plugin it got no permission prompt and needed no
`request_permission` call.

## How replies reach a caller outside zellij

`cbdist [--session S] pipe --plugin zellij:cbctl --name <cmd> --args k=v,..
-- <payload>` sends one message to the plugin's `pipe()` with
`PipeSource::Cli(pipe_id)`. The plugin writes the reply to the CLI's stdout
with `cli_pipe_output(pipe_id, ..)`. The CLI exits when the plugin unblocks
the pipe, using the exit code set with `set_cli_pipe_exit_code`.

For events, the plugin keeps the `pipe_id`, calls `block_cli_pipe_input` and
never unblocks it. Each `cli_pipe_output` then reaches that process's stdout
as it happens. The board would hold this as a child process and read it line
by line.

Things learned the hard way:

- **Give the CLI a null stdin.** If stdin is not a terminal, `zellij pipe`
  streams stdin lines as further messages and waits for EOF. A caller with an
  open stdin pipe hangs forever. The board must spawn it with
  `Stdio::null()`.
- **One extra message per call.** After the plugin unblocks, the CLI sends a
  final message with `payload: None` to mark end of input. The plugin must
  just unblock it. Before this was handled, every `open` ran twice.
- **Pipes are released automatically.** Unless `pipe()` calls
  `block_cli_pipe_input`, zellij releases the pipe when `pipe()` returns and
  the events subscriber exits 0.
- **`--args` splits on commas**, so a list inside an arg needs another
  separator. The spike uses `stack=terminal_2:terminal_3`.
- **Events go to a plugin instance per client.** In a session started with
  `attach --create-background` and no client attached, `CommandPaneOpened`
  never arrived. With a real client it does. The board always has a client
  (it runs in one), so this is fine, but tests need an attached client.
- **`CommandPaneOpened` does not fire for an in-place open**, so the plugin
  reports that open itself.
- **CLI actions count as their own client.** `cbdist action focus-pane-id`
  moves the focus of the CLI's own short-lived client, not the user's. That is
  another reason to stop driving focus through the CLI.

## Wasm build setup

- `rustup target add wasm32-wasip1`, then `cargo build --release --target
  wasm32-wasip1` in `cbplugin`. The crate is a binary (`src/main.rs`), since
  `register_plugin!` defines `main`. The only dependency is
  `zellij-tile = "0.46.0"`, which must match the distribution's zellij
  version.
- Cold build is 39 s and gives a 2.5 MB `.wasm`. Incremental builds take a
  few seconds.
- The distribution embeds it with
  `.with_plugin("cbctl", include_bytes!("../../cbplugin/target/wasm32-wasip1/release/cbplugin.wasm"))`,
  so the wasm must be built first. Re-linking `cbdist` after a plugin change
  took 2 to 40 s here. The binary is 106 MB.
- Both crates sit outside any workspace with claude-board (the rusqlite pin
  from task 22). They live under `claude-board/wyrcan/` but claude-board's
  `Cargo.toml` declares no workspace, so each resolves on its own. The real
  version needs a build script or `just` recipe that builds the wasm and then
  the distribution.

## `Alt b`

It works. The bundled config's `bind "Alt b" { ToggleFocusFullscreen;
MoveFocus "Left"; }` took the real client from an agent pane back to the
board, so `install` can stop printing `RETURN_KEYBIND`. One existing quirk
remains: pressed from an agent that isn't fullscreen, the toggle leaves the
board fullscreen. That is how the keybind behaves today with stock zellij
too. The rewrite could have `Alt b` send a pipe message to the plugin
(`MessagePlugin "zellij:cbctl" { name "back"; }`) and let it restore focus and
fullscreen exactly.

## Also found

- **A setup wizard appears on every new session.** 0.46 floats its
  desktop-integration wizard (`zellij:configuration`, titled "Welcome to
  Zellij") until it has been completed or dismissed once for that
  distribution. `show_startup_tips false` doesn't stop it. Options: dismiss
  it once on install, or `.without_plugin("configuration")` (zellij then shows
  an error in that pane instead, so test it first).
- `write` to a pane id that doesn't exist still answers `ok`: the write
  calls return nothing. The rewrite should check the id against the last
  `PaneUpdate` first.
- Subscribers are never removed. The rewrite should drop a pipe id once
  `cli_pipe_output` to it fails, or on a `PaneClosed`/session event, and
  should report `exited` for panes from earlier board runs. The `board`
  context key only marks panes this plugin instance opened.

## Next

Rewrite `src/zellij.rs` as a client of `zellij:cbctl`:

- a `Pipe` runner that calls `cbdist pipe --plugin zellij:cbctl` with a null
  stdin, keeping today's `Runner` trait so the tests still use a recorder;
- one events child started with the board, feeding agent state in place of
  `live_panes` polling;
- a launcher binary (`claude-board-zellij` or similar) built from the
  distribution crate, outside the claude-board crate;
- `install` drops `RETURN_KEYBIND`.

Open choices for that task: the payload format (the spike splits the command
on whitespace; the real version should send JSON argv), and whether
`fullscreen`, `close` and `toggle_board_fullscreen` also move into the plugin
(yes, for one control path).
