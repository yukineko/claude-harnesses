//! User ruling 2026-11-19: the main session no longer adjudicates relayed Asks.
//! Verbatim: 「relayの判定はすべてやらない。上記の方針通り。他のsessionにも通達」,
//! 「この機能そのものを削除するように、起票」, 「sesssion間relayはやってもいいが裁定はしない。」
//!
//! The adjudication duty is wired as the askrelay Stop hook, which blocks the
//! main session until it re-runs each queued subagent Ask or dismisses it.
//! Removing the feature means that hook is gone from the plugin's hooks.json.
//! Cross-session relay itself stays, for task splitting only (「session間relayは
//! タスク分割のためにやることとする」).

use std::path::Path;

#[test]
#[ignore = "RED for the relay-adjudication removal task; un-ignore when the Stop hook is removed"]
fn askrelay_has_no_stop_hook_that_demands_adjudication() {
    let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json");
    let text = std::fs::read_to_string(&hooks).expect("read askrelay hooks.json");
    let json: serde_json::Value = serde_json::from_str(&text).expect("parse askrelay hooks.json");
    let stop = json.get("hooks").and_then(|h| h.get("Stop"));
    let wires_stop = stop
        .map(|s| s.to_string().contains("askrelay stop"))
        .unwrap_or(false);
    assert!(
        !wires_stop,
        "askrelay still registers the Stop hook `askrelay stop`, which makes the main session adjudicate relayed Asks: {stop:?}"
    );
}
