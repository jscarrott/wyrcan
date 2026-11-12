# Spike: claude-board as a zellij 0.46 distribution

Board task 22, 2026-11-12. Question: should claude-board ship as a zellij
"distribution" (https://zellij.dev/documentation/distributions.html), so the
board layout and the `Alt b` return keybind come built in instead of
`install` printing a keybind to paste?

## Verdict: go, with a bundled plugin, after one more spike

A distribution on its own (config, layout and keybind) isn't worth its cost.
It is worth it as the base for two things the current setup lacks:

1. **Isolation.** The board gets its own zellij: its own config, sockets,
   sessions and data folders, with its layout and keybind built into the
   binary. The user's `~/.config/zellij` is never edited, and their own
   sessions never see the board's. We can also strip zellij down to what
   agents need (no built-in plugins, a minimal keybind set).
2. **Real APIs.** Today `src/zellij.rs` drives zellij the way a user would.
   It polls `list-panes`, then for each step runs a CLI command (open, then
   stack, then focus back to the board), and it treats zellij's "already
   focused" error message as success. A plugin bundled into the distribution
   runs inside the session and gets real calls and events instead. A bundled
   plugin skips the permission prompts and starts with the layout.

The next step is a plugin spike (see the end of this note), before rewriting
`src/zellij.rs`.

## What was tested

A throwaway crate depending on `zellij = "0.46.0"` and calling
`zellij::run(Distribution::new("cbdist", ..).with_config(..).with_layout(..))`,
built and run on this machine (stock zellij here is 0.45.1).

| Question from the task | Finding |
|---|---|
| Build cost | Cold `cargo build --release`: 3m41s wall, 24m CPU. |
| Binary size | 104 MB, against 7.4 MB for claude-board today. |
| Can it live in the claude-board crate? | **No.** zellij-utils 0.46 pins `rusqlite 0.30` (`libsqlite3-sys 0.27`); claude-board uses `rusqlite 0.39` (`libsqlite3-sys 0.37`). Both declare `links = "sqlite3"`, so cargo refuses to resolve. A cargo workspace shares one lockfile and hits the same error. It has to be a separate crate outside any workspace (or claude-board downgrades to rusqlite 0.30). |
| `zellij::run` once, before config is read | It takes over `main` and argv and never returns. The binary *is* a full zellij CLI (`attach`, `run`, `action`, `ls`, …), version line `cbdist 0.1.0 (zellij 0.46.0)`. So the launcher is a separate binary, not a claude-board subcommand. |
| What else the library exposes | Nothing for runtime control. `zellij/src/lib.rs` exports only the `Distribution` builder types, `run` and `run_zellij`. Session control (`send_action_to_session`, `subscribe_to_session`, `start_client`) is `pub(crate)`. Runtime control has to come from a plugin. |
| Own config/data/cache folders | Yes, named after the distribution: sockets in `$XDG_RUNTIME_DIR/cbdist` and `/tmp/cbdist-<uid>`, config at `~/.config/cbdist`. Per the docs, a user `config.kdl` there layers on top of the bundled one. The user's `~/.config/zellij` is not used. A fresh session opened zellij's welcome screen, which the bundled config should turn off. |
| Do the CLI calls in `src/zellij.rs` reach a distribution session? | **Not as written.** Stock `zellij ls` / `zellij --session cbspike action list-panes` can't see the session ("Session 'cbspike' not found"), because the socket dir differs. `cbdist --session cbspike action list-panes` and `cbdist run` work and return `terminal_<n>`. Panes get the usual `ZELLIJ`, `ZELLIJ_SESSION_NAME`, `ZELLIJ_PANE_ID`, so `Zellij::from_env` still works. |
| Plugin API coverage (grep of zellij-utils 0.46 plugin protos) | Has `NewStackedPane` and `StackPanes`, and the events `PaneUpdate`, `PaneClosed` and `CommandPaneExited`. Not yet confirmed: the exact focus, fullscreen and write-to-pane calls. |

## What stays text in a terminal

The agents are interactive Claude TUIs. The only way to give one a prompt
such as `/wrap` is to write it to its terminal input. A plugin does that by
pane id without moving focus, which removes the fragile part, but the input
is still text. A typed input API would mean running agents headless through
the Agent SDK and losing the interactive Claude screen. Not proposed.

## Options considered and set aside

- **Distribution without a plugin.** Gives isolation, but every pane command
  still goes through the CLI, with the binary name changed. Doesn't address
  the fragility.
- **claude-board runs the terminals itself** (a PTY library plus a terminal
  widget in ratatui, no zellij). Gives full control and needs no config, but
  means building rendering, scrollback, mouse and copy for Claude's TUI.
  More risk than the plugin route.
- **Stock zellij with `--config` / `--layout`.** No new crate, but `--config`
  replaces the user's config instead of layering on it, and it still drives
  zellij through the CLI.

## Next: plugin spike

Done when, in a session started from a throwaway distribution with a bundled
plugin and without touching `~/.config/zellij`:

1. the board, from outside the plugin, asks it over a zellij pipe to open a
   command pane in the agent stack, and gets the pane id back;
2. the plugin reports that pane's open, focus and exit events back to the
   board without polling;
3. the board asks the plugin to write `/wrap` + Enter into that pane without
   moving focus.

Record the exact plugin API calls used, how replies get back to a caller
outside zellij over a pipe, the wasm build setup, and whether `Alt b` still
works. If it holds, `src/zellij.rs` becomes a thin client to the plugin and
`install` stops printing `RETURN_KEYBIND`.
