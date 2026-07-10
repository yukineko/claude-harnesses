//! Stop-hook entry helpers shared by the gates: the never-break-a-turn panic
//! guard and the one-shot skip-marker consumer.

use std::path::Path;

/// Run a Stop-hook body under the never-break-a-turn panic guard.
///
/// `body` is the gate logic; it returns `!` because it always ends in a
/// `process::exit`. A real `process::exit` inside `body` terminates the process
/// directly and never unwinds here — so only a genuine *panic* reaches this
/// guard:
///   * hook mode (`interactive == false`) → swallow it and exit 0 (allow the
///     stop; a hook must never break the user's turn).
///   * interactive/manual mode → print `<name>: internal error` and exit 1.
///
/// `body` is wrapped in `AssertUnwindSafe`: on a panic we exit the process
/// immediately, so no possibly-inconsistent captured state is ever observed.
///
/// `body` returns `!` in practice (it always ends in `process::exit`), making
/// the inferred `R` the never type; the signature stays generic over `R` so the
/// `!` type need not be named.
pub fn run_guarded<R, F: FnOnce() -> R>(name: &str, interactive: bool, body: F) -> R {
    match guard(interactive, body) {
        Ok(value) => value,
        Err(code) => {
            if interactive {
                eprintln!("{name}: internal error");
            }
            std::process::exit(code);
        }
    }
}

/// Testable core of [`run_guarded`]: run `body`, returning its value on success
/// or the exit code to use (1 interactive, 0 hook) if it panicked.
fn guard<R, F: FnOnce() -> R>(interactive: bool, body: F) -> Result<R, i32> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(v) => Ok(v),
        Err(_) => Err(if interactive { 1 } else { 0 }),
    }
}

/// Consume a one-shot skip marker `<root>/<marker>`: if present, return its
/// trimmed one-line reason (or `"(no reason given)"` when empty) and delete the
/// file so it only applies once. Returns `None` when the marker is absent.
pub fn consume_skip(root: &Path, marker: &str) -> Option<String> {
    let p = root.join(marker);
    if !p.exists() {
        return None;
    }
    let reason = std::fs::read_to_string(&p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(no reason given)".to_string());
    let _ = std::fs::remove_file(&p);
    Some(reason)
}

/// Append `entry` as one JSON line to `<state_dir>/log.jsonl`, creating the
/// directory if needed. The shared event-log sink for the Stop gates
/// (donegate/reviewgate/tdd): each builds its own crate-specific `entry`, this
/// owns the write. Best-effort — a serialization or IO failure is swallowed,
/// since an observability log must never break the turn it records.
pub fn append_jsonl(state_dir: &Path, entry: &serde_json::Value) {
    let path = state_dir.join("log.jsonl");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(line) = serde_json::to_string(entry) {
        // Single atomic append (body + '\n' in one write) — see issue #15.
        crate::append::append_line(&path, &line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_passes_value_through() {
        assert_eq!(guard(true, || 7), Ok(7));
        assert_eq!(guard(false, || 7), Ok(7));
    }

    #[test]
    fn guard_maps_panic_to_exit_code() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let interactive: Result<(), i32> = guard(true, || panic!("boom"));
        let hook: Result<(), i32> = guard(false, || panic!("boom"));
        std::panic::set_hook(prev);
        assert_eq!(interactive, Err(1)); // manual CLI surfaces the error
        assert_eq!(hook, Err(0)); // hook mode swallows it, allows the stop
    }

    fn skip_root(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hc-gate-skip-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn skip_marker_absent_is_none() {
        let root = skip_root("absent");
        assert!(consume_skip(&root, ".x-skip").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skip_marker_with_reason_is_consumed_once() {
        let root = skip_root("reason");
        std::fs::write(root.join(".x-skip"), "  because\n").unwrap();
        assert_eq!(consume_skip(&root, ".x-skip").as_deref(), Some("because"));
        // consumed: a second call sees nothing.
        assert!(consume_skip(&root, ".x-skip").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skip_marker_empty_gives_default_reason() {
        let root = skip_root("empty");
        std::fs::write(root.join(".x-skip"), "   \n").unwrap();
        assert_eq!(
            consume_skip(&root, ".x-skip").as_deref(),
            Some("(no reason given)")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn append_jsonl_creates_dir_and_appends_lines() {
        let dir = std::env::temp_dir()
            .join(format!("hc-gate-log-{}", std::process::id()))
            .join("nested"); // parent does not exist yet
        let _ = std::fs::remove_dir_all(&dir);
        append_jsonl(&dir, &serde_json::json!({ "verdict": "pass" }));
        append_jsonl(&dir, &serde_json::json!({ "verdict": "fail" }));
        let body = std::fs::read_to_string(dir.join("log.jsonl")).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "each call appends exactly one line");
        assert!(lines[0].contains("\"pass\"") && lines[1].contains("\"fail\""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
