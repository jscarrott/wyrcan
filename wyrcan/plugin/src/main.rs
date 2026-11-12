//! The board's pane controller, bundled into wyrcan as `zellij:wyrcan` and loaded headless.
//!
//! The board talks to it with `wyrcan pipe --plugin zellij:wyrcan --name <command>`:
//! - `open`, payload `{"name","cwd","argv","stack"}`: opens a command pane, prints `terminal_<n>`.
//!   The first agent replaces the layout's `agents` pane; later ones join `stack`. Focus stays put.
//! - `focus` / `fullscreen` / `close` with `--args pane=terminal_<n>`.
//! - `write` with `--args pane=terminal_<n>`, payload text: types it, then Enter.
//! - `events`: never returns; prints `panes terminal_0 terminal_3 ...` now and whenever that set changes.
//!
//! Errors print `error: ...` and exit 1.
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use zellij_tile::prelude::*;

#[derive(Default)]
struct Wyrcan {
    /// `events` callers, kept blocked so each line reaches their stdout.
    // shortcut: never pruned, so a restarted board leaves a dead entry; prune if sessions see many restarts.
    subscribers: Vec<String>,
    /// The last `panes` line sent.
    panes: String,
    /// The layout's `agents` placeholder, until the first agent replaces it.
    placeholder: Option<u32>,
    /// Pipes whose command ran from a payload: the CLI follows up with an empty end-of-input message.
    awaiting_end: HashSet<String>,
}

register_plugin!(Wyrcan);

#[derive(Deserialize)]
struct Open {
    name: String,
    cwd: String,
    argv: Vec<String>,
    stack: Vec<String>,
}

fn terminal(id: &str) -> Option<PaneId> {
    id.strip_prefix("terminal_")?.parse().ok().map(PaneId::Terminal)
}

fn pane_arg(args: &BTreeMap<String, String>) -> Result<PaneId, String> {
    args.get("pane").and_then(|p| terminal(p)).ok_or_else(|| "needs --args pane=terminal_<n>".to_string())
}

impl Wyrcan {
    fn open(&mut self, payload: Option<String>) -> Result<String, String> {
        let o: Open = serde_json::from_str(&payload.unwrap_or_default()).map_err(|e| format!("bad open payload: {e}"))?;
        let (prog, args) = o.argv.split_first().ok_or("empty argv")?;
        let mut cmd = CommandToRun::new_with_args(prog, args.to_vec());
        cmd.cwd = Some(o.cwd.into());
        let mut stack: Vec<PaneId> = o.stack.iter().filter_map(|s| terminal(s)).collect();
        let focused = get_focused_pane_info().ok().map(|(_, p)| p);
        let ctx = BTreeMap::new();
        let id = match self.placeholder {
            Some(p) if stack.is_empty() => {
                self.placeholder = None;
                open_command_pane_in_place_of_pane_id(PaneId::Terminal(p), cmd, true, ctx)
            }
            _ => open_command_pane_background(cmd, ctx).inspect(|&id| show_pane_with_id(id, false, false)),
        }
        .ok_or("zellij opened no pane")?;
        let PaneId::Terminal(n) = id else { return Err("opened a plugin pane".into()) };
        rename_pane_with_id(id, &o.name);
        if !stack.is_empty() {
            stack.push(id);
            stack_panes(stack);
        }
        // Stacking focuses the new pane; hand focus back.
        if let Some(f) = focused {
            focus_pane_with_id(f, false, false);
        }
        Ok(format!("terminal_{n}"))
    }

    fn run(&mut self, name: &str, payload: Option<String>, args: &BTreeMap<String, String>) -> Result<String, String> {
        match name {
            "open" => return self.open(payload),
            "focus" => focus_pane_with_id(pane_arg(args)?, false, false),
            "fullscreen" => toggle_pane_id_fullscreen(pane_arg(args)?),
            "close" => {
                let PaneId::Terminal(n) = pane_arg(args)? else { unreachable!("pane_arg only gives terminals") };
                close_terminal_pane(n);
            }
            "write" => {
                let pane = pane_arg(args)?;
                write_chars_to_pane_id(&payload.unwrap_or_default(), pane);
                write_to_pane_id(vec![b'\r'], pane);
            }
            other => return Err(format!("unknown command {other}")),
        }
        Ok("ok".into())
    }
}

impl ZellijPlugin for Wyrcan {
    fn load(&mut self, _config: BTreeMap<String, String>) {
        subscribe(&[EventType::PaneUpdate]);
    }

    fn update(&mut self, event: Event) -> bool {
        if let Event::PaneUpdate(m) = event {
            let terminals: Vec<&PaneInfo> = m.panes.values().flatten().filter(|p| !p.is_plugin).collect();
            if self.placeholder.is_none_or(|p| !terminals.iter().any(|t| t.id == p)) {
                self.placeholder = terminals.iter().find(|t| t.title == "agents").map(|t| t.id);
            }
            let mut ids: Vec<u32> = terminals.iter().map(|t| t.id).collect();
            ids.sort();
            let line: String = std::iter::once("panes".to_string()).chain(ids.iter().map(|n| format!("terminal_{n}"))).collect::<Vec<_>>().join(" ");
            if line != self.panes {
                self.panes = line;
                for s in &self.subscribers {
                    cli_pipe_output(s, &format!("{}\n", self.panes));
                }
            }
        }
        false
    }

    fn pipe(&mut self, msg: PipeMessage) -> bool {
        let PipeSource::Cli(pipe) = msg.source else { return false };
        if msg.payload.is_none() && self.awaiting_end.remove(&pipe) {
            unblock_cli_pipe_input(&pipe);
            return false;
        }
        if msg.payload.is_some() {
            self.awaiting_end.insert(pipe.clone());
        }
        if msg.name == "events" {
            // Blocked for good: zellij would otherwise release the pipe when this returns.
            block_cli_pipe_input(&pipe);
            if !self.panes.is_empty() {
                cli_pipe_output(&pipe, &format!("{}\n", self.panes));
            }
            self.subscribers.push(pipe);
            return false;
        }
        match self.run(&msg.name, msg.payload, &msg.args) {
            Ok(out) => cli_pipe_output(&pipe, &format!("{out}\n")),
            Err(e) => {
                cli_pipe_output(&pipe, &format!("error: {e}\n"));
                set_cli_pipe_exit_code(&pipe, 1);
            }
        }
        unblock_cli_pipe_input(&pipe);
        false
    }
}
