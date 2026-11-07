//! Duplicate-`cargo test` hook pair (backlog 2913b970): `donegate dup-test pre`
//! (PreToolUse, Bash) denies re-running a plain `cargo test ...` that already
//! PASSED with the exact same argv on the exact same tree; `donegate dup-test
//! post` (PostToolUse / PostToolUseFailure, Bash) records such passes.
//!
//! This is a deny-only de-duplicator, not a verdict about the code: its only
//! possible outputs are "deny this re-run" and "say nothing". Every doubt
//! resolves to SAY NOTHING (the command runs, nothing is recorded) — a missed
//! duplicate costs one re-run; a false deny would stop a run whose outcome is
//! not actually known. Concretely, nothing is denied or recorded when:
//!
//! * the command is not a plain single `cargo test ...` (any shell
//!   metacharacter — `&&` `;` `|` `(` `$` quotes, redirects, newlines —, an env
//!   assignment prefix, `cd`, `cargo +toolchain`, `--manifest-path`,
//!   `--config`): see [`plain_cargo_test`];
//! * the payload cannot be parsed, `cwd` is missing / not canonicalizable, the
//!   cwd is not in a git repository, or any git call fails (passcache's
//!   `tree_state` returns `None`);
//! * the store entry is missing, unreadable, unparsable, or its key differs in
//!   any byte;
//! * `post` sees anything but `hook_event_name == "PostToolUse"` with a
//!   `tool_response.interrupted` that is literally `false` (the Bash
//!   `tool_response` carries no exit code; a failing call arrives as
//!   `PostToolUseFailure`), or no matching `pre` snapshot for the
//!   `tool_use_id`, or a tree key at `post` that differs from the `pre`
//!   snapshot (the tree changed during the call).
//!
//! Key: canonical cwd + git toplevel + the whitespace-normalized argv + the
//! passcache [`crate::passcache::TreeState`] (HEAD, `main`, and the
//! working-tree content tree incl. untracked-not-ignored files). Not covered:
//! ignored-and-untracked files (e.g. `target/`), files outside the repository,
//! and the inherited environment.
//!
//! Store: `~/.donegate/dup-test/passes/<hash(key)>.json` (one per key, exact
//! key compared on read), `~/.donegate/dup-test/pending/<tool_use_id>.json`
//! (pre snapshot, removed by post — a pre whose post never fires, e.g. a later
//! PreToolUse hook denied the call, leaves its small snapshot behind), and
//! `~/.donegate/dup-test/tmp/` for passcache's temporary index files.
//!
//! Cost: `pre` / `post` run on EVERY Bash call. Anything that is not a plain
//! `cargo test` returns right after the stdin JSON parse, with no filesystem or
//! git access. A panic exits 101 with the message on stderr (Claude Code
//! treats that as a non-blocking hook error: the command runs, the crash is
//! visible) — consistent with "doubt = do not deny".

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::config::{base_dir, Config};
use crate::model::HookInput;

const STORE_VERSION: u64 = 1;

fn store_dir() -> PathBuf {
    base_dir().join("dup-test")
}

/// The whitespace-normalized argv when `cmd` is provably a single plain
/// `cargo test ...` invocation; `None` on any doubt.
pub(crate) fn plain_cargo_test(cmd: &str) -> Option<String> {
    let t = cmd.trim();
    // Cheap first: almost every Bash call fails here.
    if !t.starts_with("cargo") {
        return None;
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || " \t-_./:,=+@%".contains(c);
    if !t.chars().all(allowed) {
        return None;
    }
    let toks: Vec<&str> = t.split_ascii_whitespace().collect();
    if toks.first() != Some(&"cargo") || toks.get(1) != Some(&"test") {
        return None;
    }
    // Options that make the tested sources / build config come from somewhere
    // the key does not cover.
    let foreign = |a: &&str| {
        *a == "--manifest-path"
            || a.starts_with("--manifest-path=")
            || *a == "--config"
            || a.starts_with("--config=")
    };
    if toks.iter().any(foreign) {
        return None;
    }
    Some(toks.join(" "))
}

/// A parsed Bash plain-`cargo test` payload.
struct Candidate {
    input: HookInput,
    /// `tool_use_id`, which [`HookInput`] does not model.
    tool_use_id: Option<String>,
    argv: String,
}

/// Parse stdin; `Some` only for a Bash plain `cargo test`.
fn read_candidate() -> Option<Candidate> {
    if Config::disabled_env() {
        return None;
    }
    let raw = harness_core::hook::try_read_stdin().ok()?;
    let input = HookInput::parse(&raw)?;
    if input.tool_name != "Bash" {
        return None;
    }
    let cmd = input.tool_input.as_ref()?.get("command")?.as_str()?;
    let argv = plain_cargo_test(cmd)?;
    let tool_use_id = serde_json::from_str::<serde_json::Value>(raw.trim())
        .ok()
        .and_then(|v| v.get("tool_use_id")?.as_str().map(String::from));
    Some(Candidate {
        input,
        tool_use_id,
        argv,
    })
}

/// The exact key for running `argv` in `cwd` now; `None` when undeterminable.
fn compute_key(cwd: &str, argv: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let cwd = Path::new(cwd).canonicalize().ok()?;
    let state = crate::passcache::tree_state(&cwd, &store_dir().join("tmp"))?;
    let mut git = std::process::Command::new("git");
    harness_core::git_env::scrub(&mut git);
    let out = git
        .current_dir(&cwd)
        .args(["rev-parse", "--show-toplevel"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if top.is_empty() {
        return None;
    }
    Some(
        serde_json::json!({
            "cwd": cwd.to_string_lossy(),
            "toplevel": top,
            "argv": argv,
            "state": state.fingerprint(),
        })
        .to_string(),
    )
}

fn pass_path(key: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    store_dir()
        .join("passes")
        .join(format!("{:016x}.json", h.finish()))
}

/// A `tool_use_id` usable as a file name, else `None` (nothing is snapshotted,
/// so nothing can be recorded).
fn pending_path(tool_use_id: Option<&str>) -> Option<PathBuf> {
    let id = tool_use_id?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(store_dir().join("pending").join(format!("{id}.json")))
}

/// Write `body` atomically (temp + rename). Failure leaves nothing / the old
/// file — at worst a later miss.
fn write_atomic(path: &Path, body: &str) {
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    if std::fs::write(&tmp, body).is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// When the stored pass for exactly `key` was recorded; `None` (= no deny) on
/// any read / parse failure or mismatch.
fn lookup(key: &str) -> Option<String> {
    let raw = std::fs::read(pass_path(key)).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    if v.get("v")?.as_u64()? != STORE_VERSION || v.get("key")?.as_str()? != key {
        return None;
    }
    Some(v.get("passed_at")?.as_str()?.to_string())
}

/// The deny line for a proven duplicate.
pub(crate) fn deny_json(argv: &str, passed_at: &str) -> String {
    let reason = format!(
        "donegate dup-test: `{argv}` already PASSED on this exact tree (same command, same \
         HEAD, byte-identical working tree) at {passed_at}. Re-running it cannot produce new \
         information, so this duplicate run is denied. Use the earlier result. If a re-run is \
         genuinely needed, change the tree (edit the code under test) or change the command \
         (e.g. a different filter or flags)."
    );
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
    .to_string()
}

/// `donegate dup-test pre`.
pub fn pre_command() {
    let Some(c) = read_candidate() else {
        return;
    };
    let Some(key) = compute_key(&c.input.cwd, &c.argv) else {
        return;
    };
    if let Some(at) = lookup(&key) {
        println!("{}", deny_json(&c.argv, &at));
        return;
    }
    // Snapshot the pre-call key so post can prove the tree did not change
    // during the call.
    let Some(path) = pending_path(c.tool_use_id.as_deref()) else {
        return;
    };
    let body = serde_json::json!({ "v": STORE_VERSION, "key": key }).to_string();
    write_atomic(&path, &body);
}

/// `donegate dup-test post`.
pub fn post_command() {
    let Some(Candidate {
        input,
        tool_use_id,
        argv,
    }) = read_candidate()
    else {
        return;
    };
    let pending = pending_path(tool_use_id.as_deref()).and_then(|p| {
        let raw = std::fs::read(&p).ok();
        let _ = std::fs::remove_file(&p);
        raw
    });
    let Some(pending) = pending else {
        return;
    };
    if input.hook_event_name != "PostToolUse" {
        return;
    }
    let interrupted = input
        .tool_response
        .as_ref()
        .and_then(|r| r.get("interrupted"))
        .and_then(serde_json::Value::as_bool);
    if interrupted != Some(false) {
        return;
    }
    let Some(pre_key) = serde_json::from_slice::<serde_json::Value>(&pending)
        .ok()
        .filter(|v| v.get("v").and_then(serde_json::Value::as_u64) == Some(STORE_VERSION))
        .and_then(|v| v.get("key").and_then(|k| k.as_str()).map(String::from))
    else {
        return;
    };
    let Some(post_key) = compute_key(&input.cwd, &argv) else {
        return;
    };
    if post_key != pre_key {
        return;
    }
    let body = serde_json::json!({
        "v": STORE_VERSION,
        "key": post_key,
        "passed_at": chrono::Local::now().to_rfc3339(),
    })
    .to_string();
    write_atomic(&pass_path(&post_key), &body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_forms() {
        assert_eq!(
            plain_cargo_test("  cargo   test -p foo --test bar ").as_deref(),
            Some("cargo test -p foo --test bar")
        );
        assert!(plain_cargo_test("cargo test -p foo -- --test-threads=1").is_some());
    }

    #[test]
    fn non_plain_forms_are_rejected() {
        for c in [
            "cargo build -p foo",
            "cargotest",
            "cargo +nightly test",
            "cargo test && echo",
            "cargo test; true",
            "cargo test | tail",
            "(cargo test)",
            "cd /tmp && cargo test",
            "FOO=1 cargo test",
            "cargo test $X",
            "cargo test 'a b'",
            "cargo test > out",
            "cargo test\ncargo build",
            "cargo test --manifest-path /x/Cargo.toml",
            "cargo test --manifest-path=/x/Cargo.toml",
            "cargo test --config foo.toml",
            "cargo test ~/x",
            "echo cargo test",
        ] {
            assert!(plain_cargo_test(c).is_none(), "{c:?}");
        }
    }

    #[test]
    fn pending_id_must_be_a_safe_name() {
        assert!(pending_path(Some("toolu_01AbC-9")).is_some());
        for bad in ["", "../x", "a/b", "a.b", "a b"] {
            assert!(pending_path(Some(bad)).is_none(), "{bad:?}");
        }
        assert!(pending_path(None).is_none());
    }

    #[test]
    fn deny_json_is_a_pretooluse_deny() {
        let v: serde_json::Value =
            serde_json::from_str(&deny_json("cargo test", "t")).expect("json");
        assert_eq!(
            v.pointer("/hookSpecificOutput/permissionDecision")
                .and_then(|d| d.as_str()),
            Some("deny")
        );
    }
}
