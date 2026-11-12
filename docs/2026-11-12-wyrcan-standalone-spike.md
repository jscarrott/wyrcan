# Spike: wyrcan as its own terminal workspace, without zellij

2026-11-12, branch `wyrcan-standalone`. Question: can `wyrcan` be one
standalone binary, like herdr, instead of a zellij 0.46 distribution?

## Verdict: promising, not yet proven for daily use

The mechanics work and the cost is far lower than the zellij build. What
isn't proven is how a real Claude Code session feels inside the emulator.
That needs an hour of normal use by a person (see "Not tested").

| | wyrcan on zellij (`wyrcan/`) | standalone (`wyrcan-native/`) |
|---|---|---|
| Binary | 106 MB | 1.7 MB |
| Dependency crates | 531 | 108 |
| Crates and build | 2 (plus wasm target, `build.sh`), outside any workspace because of the rusqlite pin | 1, plain `cargo build`; no rusqlite clash, so it could live in the claude-board crate |
| Own code | plugin 160 lines + launcher | 1,074 lines (server 549, pane 218, protocol 153, main 154) |
| Board changes needed | the plugin client | none: same `pipe` syntax |

## What was built

`wyrcan-native/` builds a binary called `wyrcan` with three roles:

- **`wyrcan [--session S]`** (default session `board`) attaches, first
  starting the session's server in its own process session (`setsid`) if it
  isn't running. The client is a thin relay: it puts the terminal in raw mode
  with SGR mouse and bracketed paste, forwards input bytes and resizes, and
  copies the server's output to the screen.
- **The server** owns the panes. Each pane is a PTY child parsed by
  `alacritty_terminal`, the emulator core of Alacritty, also used by Zed. The
  server composes the screen with ratatui (board left, agent stack right,
  status line) and sends only the changed cells to the client, because
  ratatui diffs each frame. It runs one event loop fed by a channel, and
  draws at most once every 16 ms.
- **`wyrcan pipe --name open|focus|fullscreen|close|write|events …`** takes
  the same syntax as the zellij plugin. That's why `claude-board` runs
  unchanged in pane 0: it already calls `$WYRCAN pipe …`, and the server sets
  `WYRCAN`, `ZELLIJ_SESSION_NAME` and `ZELLIJ_PANE_ID` for every pane, so the
  hooks still work. `dump` (screen text) and `kill` were added for testing.

Keys: `Alt b`/`Alt h` board, `Alt l` agents, `Alt j`/`Alt k` next/previous
agent in the stack, `Alt d` detach. A click focuses a pane, or expands a
stacked agent from its title row. The mouse wheel scrolls a pane's
scrollback unless the app in it handles the mouse itself.

## What was tested (scripted, in a pseudo-terminal, scratch board state)

1. `wyrcan --session spike` started the server and showed the real board,
   drawn through the emulator, with "Agents open here." on the right.
2. `n` twice in the board opened two Claude panes on the right. Claude's
   welcome screen rendered, and the board kept focus.
3. `Alt l`, `Alt k`, `Alt b` moved focus as expected.
4. `w` on a waiting stream typed `/wrap` + Enter into its pane.
5. **Persistence.** `Alt d` detached: the client printed "detached from spike;
   run `wyrcan` to come back", and the server and both agents kept running. A
   new client at a different size (160×40) got the whole screen redrawn at
   that size. SIGHUP to the client's terminal session, which is what closing
   the window sends, left the server and every pane running.
6. **Numbers.** With no client attached, the server used 0 ms of CPU in 5 s.
   2,000,000 lines of `seq` went through the emulator in about 0.6 s, using
   about 0.5 s of server CPU. Build time wasn't measured: the shared compile
   cache here answers even a fresh target dir in 1 s.

## Not tested, and known gaps

- **A real Claude session.** The test Claude had no login, so only its
  welcome screen rendered. Typing, long streaming output, permission prompts,
  `/` menus, resizing mid-stream and scrollback need a person to try them.
- **Shift+Enter and other modified keys.** The client forwards whatever
  legacy bytes the outer terminal sends. The kitty keyboard protocol isn't
  negotiated through to kitty, so Claude can't tell Shift+Enter from Enter.
  herdr re-encodes keys with libghostty for this; it's the biggest gap.
- **Theme detection.** Colour queries (OSC 10/11) are answered with black,
  because the server can't see the real terminal's colours. Named colours
  are passed through as palette colours, so panes still follow kitty's
  theme.
- **Clipboard and selection.** OSC 52 copy from inside a pane is dropped.
  Selecting text needs Shift+drag in kitty, because mouse reporting is on,
  and a selection runs across pane boundaries.
- **Shortcuts in the code:** hotkeys must arrive as their own read (true for
  typed keys); one client at a time (a new attach takes over); the session
  ends when every pane has exited; the cursor-shape escape is re-sent every
  frame.
- The emulator could be swapped for libghostty-vt later (herdr uses it).
  It's a Zig library, so the build would need a Zig toolchain.

## Next, if this goes ahead

1. A person runs it for an hour with real agents:
   `wyrcan-native/target/release/wyrcan --session try`. It runs the
   installed `claude-board`, on the real board database.
2. Kitty keyboard protocol passthrough, and OSC 52 to the client.
3. Move it into the claude-board crate, so one `wyrcan` binary is both the
   board and the workspace, and retire `wyrcan/` (zellij).
