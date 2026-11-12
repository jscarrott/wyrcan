//! wyrcan: zellij 0.46 with the board's layout, keybind and pane-control plugin built in.
//! It has its own config, sessions and data folders (~/.config/wyrcan, ...), never ~/.config/zellij.
use zellij::{Distribution, DistributionLayout};

fn main() {
    // Panes inherit this: the board knows it runs inside wyrcan, and which binary to pipe through
    // (not a PATH lookup, which another `wyrcan` could win).
    if let Ok(exe) = std::env::current_exe() {
        std::env::set_var("WYRCAN", exe);
    }
    skip_setup_wizard();
    zellij::run(
        Distribution::new("wyrcan", env!("CARGO_PKG_VERSION"))
            .with_plugin("wyrcan", include_bytes!("../plugin/target/wasm32-wasip1/release/wyrcan-plugin.wasm"))
            .with_config(include_str!("../assets/config.kdl"))
            .with_layout(DistributionLayout::new("board", include_str!("../assets/layout.kdl"))),
    )
}

/// zellij 0.46 floats its desktop-integration wizard over every new session until it is resolved.
/// The board needs none of it, so add "never show" to wyrcan's own state file, keeping the rest.
fn skip_setup_wizard() {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/share")));
    let Some(dir) = data.map(|d| d.join("wyrcan")) else { return };
    let file = dir.join("desktop_integration.json");
    let mut state: serde_json::Value = match std::fs::read_to_string(&file) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return, // Not ours to overwrite.
        },
        Err(_) => serde_json::json!({}),
    };
    let flag = serde_json::json!("SetupNeverShow");
    let Some(obj) = state.as_object_mut() else { return };
    let flags = obj.entry("flags").or_insert_with(|| serde_json::json!([]));
    let Some(list) = flags.as_array_mut() else { return };
    if list.contains(&flag) {
        return;
    }
    list.push(flag);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&file, state.to_string());
}
