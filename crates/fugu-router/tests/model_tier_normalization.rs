//! `record --model <full Claude ID>` must normalize to the routing tier
//! (haiku|sonnet|opus), keeping the full ID as `model_id` provenance; legacy
//! raw-ID store lines must aggregate under their tier; import dedup must see
//! the legacy raw form and the normalized form as the same episode.
//! These tests observe the real binary and the real on-disk store.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Per-process counter so two tests that call `temp_home` with the same tag in
/// the same clock tick (the macOS clock is microsecond-grained, and parallel
/// test threads share the pid) never share one store.
static NEXT_HOME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn temp_home(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let seq = NEXT_HOME.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("fugu-router-tier-{tag}-{pid}-{seq}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp home");
    dir
}

fn run_in(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_fugu-router")))
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn store_path(home: &Path) -> PathBuf {
    home.join(".fugu-router").join("episodes.jsonl")
}

fn write_store(home: &Path, body: &str) {
    let p = store_path(home);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
}

/// Record one episode with `--model <model>` and return the single stored line.
fn record_one(model: &str) -> serde_json::Value {
    let home = temp_home("rec");
    let (code, out, err) = run_in(
        &home,
        &[
            "record", "--title", "t", "--model", model, "--status", "verified",
        ],
    );
    assert_eq!(code, 0, "record --model {model} failed: {out} {err}");
    let text = std::fs::read_to_string(store_path(&home)).expect("store exists");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one stored line: {text}");
    serde_json::from_str(lines[0]).expect("line is JSON")
}

#[test]
fn full_model_id_is_normalized_to_tier_and_kept_as_model_id() {
    let v = record_one("claude-opus-5-5");
    assert_eq!(v["model"], "opus", "model must be the tier: {v}");
    assert_eq!(
        v["model_id"], "claude-opus-5-5",
        "full ID kept as provenance: {v}"
    );
}

#[test]
fn full_ids_map_by_tier_word() {
    let table = [
        ("claude-haiku-4-5-20251001", "haiku"),
        ("claude-sonnet-5-5", "sonnet"),
        ("claude-opus-5", "opus"),
        ("claude-3-opus-20240229", "opus"),
    ];
    for (id, tier) in table {
        let v = record_one(id);
        assert_eq!(v["model"], tier, "{id} must map to {tier}: {v}");
        assert_eq!(v["model_id"], id, "{id} must be kept as model_id: {v}");
    }
}

#[test]
fn bare_tier_is_stored_without_model_id() {
    let v = record_one("opus");
    assert_eq!(v["model"], "opus");
    assert!(
        v.as_object().unwrap().get("model_id").is_none(),
        "version unknown: model_id must be absent, not fabricated: {v}"
    );
}

#[test]
fn non_tier_names_are_kept_verbatim_without_model_id() {
    for name in [
        "gpt-5",
        "none",
        "haiku+sonnet+haiku panel",
        "claude-fable-5-1",
    ] {
        let v = record_one(name);
        assert_eq!(v["model"], name, "{name} must be kept verbatim: {v}");
        assert!(
            v.as_object().unwrap().get("model_id").is_none(),
            "{name}: no model_id for a non-tier name: {v}"
        );
    }
}

#[test]
fn legacy_raw_id_lines_aggregate_under_their_tier_in_stats() {
    let home = temp_home("legacy");
    let line = |model: &str, pass: bool, i: u32| {
        format!(
            "{{\"ts\":{},\"title\":\"e{i}\",\"touched_files\":[],\"class\":\"\",\"model\":\"{model}\",\"role\":\"worker\",\"pass\":{pass}}}\n",
            1000 + i
        )
    };
    let body = [
        line("opus", true, 1),
        line("opus", true, 2),
        line("claude-opus-5-5", false, 3),
    ]
    .concat();
    write_store(&home, &body);

    let (code, out, err) = run_in(&home, &["stats"]);
    assert_eq!(code, 0, "stats failed: {err}");
    assert!(
        !out.contains("claude-opus-5-5"),
        "no raw-ID row may appear in stats:\n{out}"
    );
    assert!(
        out.lines()
            .any(|l| l.trim_start().starts_with("opus") && l.contains("2/3")),
        "opus must aggregate to 2/3:\n{out}"
    );

    let (code, jout, err) = run_in(&home, &["stats", "--json"]);
    assert_eq!(code, 0, "stats --json failed: {err}");
    let j: serde_json::Value = serde_json::from_str(&jout).expect("json");
    let models = j["models"].as_object().expect("models object");
    assert!(
        !models.contains_key("claude-opus-5-5"),
        "raw ID must not be its own model key: {jout}"
    );
    assert_eq!(models["opus"]["count"], 3, "{jout}");
    assert_eq!(models["opus"]["passes"], 2, "{jout}");
}

#[test]
fn import_dedups_legacy_raw_form_against_normalized_destination() {
    let home = temp_home("import");
    // Destination: normalized form (model_id right after model).
    write_store(
        &home,
        "{\"ts\":1234,\"title\":\"same\",\"touched_files\":[\"a.rs\"],\"class\":\"c\",\"model\":\"opus\",\"model_id\":\"claude-opus-5-5\",\"role\":\"worker\",\"pass\":true}\n",
    );
    // Source: legacy raw form of the very same episode.
    let src = home.join("src.jsonl");
    std::fs::write(
        &src,
        "{\"ts\":1234,\"title\":\"same\",\"touched_files\":[\"a.rs\"],\"class\":\"c\",\"model\":\"claude-opus-5-5\",\"role\":\"worker\",\"pass\":true}\n",
    )
    .unwrap();

    let (code, out, err) = run_in(&home, &["import", "--episodes", src.to_str().unwrap()]);
    assert_eq!(code, 0, "import failed: {out} {err}");
    assert!(
        out.contains("0 new appended") && out.contains("1 duplicates skipped"),
        "legacy raw episode must be a duplicate, not new:\n{out}"
    );
    let n = std::fs::read_to_string(store_path(&home))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(n, 1, "destination must not gain a duplicate line");
}
