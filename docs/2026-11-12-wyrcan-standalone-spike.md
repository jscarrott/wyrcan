# Spike: wyrcan as its own terminal workspace, without zellij

2026-11-12, branch `wyrcan-standalone`. Question: can `wyrcan` be one
standalone binary, like herdr, instead of a zellij 0.46 distribution?

## Verdict: promising, not yet proven for daily use

The mechanics work, keys now reach agents the way kitty would send them, and
the cost is far lower than the zellij build. What isn't proven is how a real,
logged-in Claude Code session feels inside it. That needs an hour of normal
use by a person (see "Not tested").

| | wyrcan on zellij (`wyrcan/`) | standalone (`wyrcan-native/`) |
|---|---|---|
| Binary | 106 MB | 4.4 MB (libghostty linked in) |
| Dependency crates | 531 | 109 |
| Build | 2 crates, a wasm target and `build.sh`; outside any workspace because of the rusqlite pin | 1 crate; needs Zig on PATH (libghostty is built from Ghostty's source, +3 min cold); no rusqlite clash, so it could live in the claude-board crate |
| Own code | plugin 160 lines + launcher | 1,297 lines |
| Board changes needed | the plugin client | none: same `pipe` syntax |

## What was built

`wyrcan-native/` builds a binary called `wyrcan`:

- **`wyrcan [--session S]`** (default session `board`) attaches, first
  starting the session's server if it isn't running. With the systemd unit
  installed it runs `systemctl --user start wyrcan@S`; otherwise it starts the
  server in its own process session (`setsid`). Closing the window only
  detaches.
- **The client** reads input with crossterm, with the kitty keyboard protocol
  switched on in the outer terminal (`DISAMBIGUATE_ESCAPE_CODES`), and sends
  each key, mouse, paste, focus or resize event to the server as a JSON line.
  It copies the server's output to the screen.
- **The server** owns the panes. Each pane is a PTY child (`portable-pty`)
  parsed by **libghostty-vt** (the `ghostty-vt` crate, pinned to a Ghostty
  1.4 main-branch commit). ghostty's encoders turn each event into the bytes
  the program in the pane asked for: legacy, modifyOtherKeys or kitty
  protocol for keys, the program's mouse mode for clicks, and bracketed
  paste. The server composes the screen with ratatui (board left, agent stack
  right, status line) and sends only what changed. Everything runs on one
  event loop, because ghostty's handles aren't thread-safe; PTY reader
  threads hand bytes to it.
- **`wyrcan pipe --name open|focus|fullscreen|close|write|events …`** takes
  the same syntax as the zellij plugin. That's why `claude-board` runs
  unchanged in pane 0: the server sets `WYRCAN`, `ZELLIJ_SESSION_NAME` and
  `ZELLIJ_PANE_ID` for every pane, so the board's calls and its hooks work as
  before. `dump`, `info` and `kill` were added.
- **`wyrcan ls`** lists sessions, e.g. `board  3 panes (3 running),
  attached 200x48`, and removes sockets left by a dead server.
- **`wyrcan service install|uninstall`** writes or removes
  `~/.config/systemd/user/wyrcan@.service`. That's a template unit, one
  session per instance (`wyrcan@board`). systemd starts services with a bare
  environment, so the unit records `PATH` (for claude-board, claude, git, gh)
  and `SSH_AUTH_SOCK` (so agents can push) at install time. Run it again after
  either changes. It is installed, not enabled. `systemctl --user enable --now
  wyrcan@board` starts the board session at login, and `loginctl
  enable-linger` keeps sessions running after logout.

Keys: `Alt b`/`Alt h` board, `Alt l` agents, `Alt j`/`Alt k` next/previous
agent in the stack, `Alt d` detach. Only an Alt+key with no other modifier is
caught, so `Alt Shift b` still reaches the pane. A click focuses a pane, or
expands a stacked agent from its title row. The mouse wheel scrolls a pane's
scrollback unless the program in it tracks the mouse.

## What was tested (scripted, in a pseudo-terminal, scratch board state)

1. `wyrcan --session gspike` started the server and showed the real board,
   drawn through libghostty. `n` twice opened two Claude panes; Claude's
   welcome screen rendered and the board kept focus. `Alt l`, `Alt k`,
   `Alt b` moved focus. `wyrcan ls` showed the session attached at 200×48.
2. **Keys, legacy program** (`cat -v`): Shift+Enter arrived as
   `ESC[27;2;13~` (modifyOtherKeys), not a plain Enter. A kitty-encoded
   Ctrl+C became `^C` and stopped `cat`.
3. **Keys, kitty program** (one that sends `CSI > 1 u`, as Claude does in
   kitty): Shift+Enter arrived as `ESC[13;2u` and Enter as `^M`. `Alt b`
   was caught by wyrcan and never reached the program.
4. **Persistence.** `Alt d` detached ("detached from gspike; run `wyrcan`
   to come back"); `ls` then showed the session detached with all panes
   running. In the earlier alacritty version of this spike, reattaching at a
   new size redrew the whole screen, and SIGHUP to the client's terminal
   session (what closing the window sends) left every pane running.
5. **systemd.** With the unit installed, `wyrcan --session sdtest` started
   `wyrcan@sdtest.service`, using 4.7 MB of memory. A per-instance drop-in,
   since removed, pointed it at scratch state. The board ran and `n`
   started Claude, found through the unit's PATH. After a detach the unit
   stayed active. `wyrcan --session sdtest kill` stopped it, and it went
   inactive without restarting.
6. **Numbers, alacritty version.** No client attached: 0 ms of CPU in 5 s.
   2,000,000 lines of output in about 0.6 s. Not yet re-measured with
   libghostty.

## Performance (after the first hands-on try felt slow)

Measured by typing into a pane through a client in a pseudo-terminal and
timing until the echo reached the client's output, 30 keys each:

| | keystroke → screen, median / p90 |
|---|---|
| First version | ~2,000 ms (most echoes only appeared when later output pushed them out) |
| Fixed, idle | 5 / 5 ms |
| Fixed, two agents streaming in the background | 4 / 5 ms |

What was wrong, and what changed:

- **The client's stdout is line-buffered**, and screen updates rarely contain
  a newline, so small updates (a key's echo) sat in the buffer. It now
  flushes after every chunk. This was nearly all of it.
- **The server wrote each escape sequence to the socket separately.** It now
  buffers a frame and sends it in one write.
- **Hidden panes caused redraws.** Output from a collapsed agent redrew the
  screen about 40 times a second for nothing. Now only visible panes mark it
  dirty: with two agents streaming in the background, the server dropped
  from 38 to 6 ms of CPU per second.
- **Frame pacing.** The loop waited up to a whole extra frame before drawing.
  It now waits only for the rest of the current frame, and draws once for
  everything already queued (at most 256 events, so a flood can't stop it
  drawing).
- **Synchronized output (mode 2026).** Claude brackets its redraws so the
  terminal shows them whole. The server now waits for a visible pane to
  finish such a redraw before drawing (at most 150 ms), and wraps each of its
  own frames the same way, so kitty never shows a half-painted frame.

A frame costs about 0.8 ms to compose with one agent visible, and redraws
run at up to 60 a second. `WYRCAN_PROFILE=1` in the server's environment logs
the frame count and cost every 5 s.

## Not tested, and known gaps

- **A real Claude session.** The test Claude had no login, so only its
  welcome screen ran. Typing, streaming output, permission prompts, `/`
  menus, resizing mid-stream and scrollback need a person to try them.
- **Theme detection.** Nothing passes the outer terminal's colours to the
  panes. How libghostty answers Claude's colour queries (OSC 10/11) without
  them wasn't checked. Palette colours pass through, so panes still follow
  kitty's theme.
- **Clipboard and selection.** libghostty reports OSC 52 copies
  (`Event::ClipboardWrite`), but they aren't forwarded to the client yet.
  Selecting text needs Shift+drag in kitty, because mouse reporting is on,
  and a selection runs across pane boundaries.
- **The binding.** `ghostty-vt` is maintained by a third party (`gpui-cn`),
  pinned to a Ghostty main-branch commit, and needs Rust 1.96 and Zig. Expect
  churn until libghostty has a stable release.
- **Shortcuts in the code:** one client at a time (a new attach takes over);
  the session ends when every pane has exited; the server never removes an
  `events` subscriber until a write to it fails.

## Next, if this goes ahead

1. A person runs it for an hour with real agents:
   `wyrcan-native/target/release/wyrcan --session try`. It runs the
   installed `claude-board`, on the real board database.
2. Forward OSC 52 to the client, and pass the outer terminal's colours to
   libghostty so theme detection is right.
3. Move it into the claude-board crate, so one `wyrcan` binary is both the
   board and the workspace, and retire `wyrcan/` (zellij).
