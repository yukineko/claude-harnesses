// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! End-to-end tests for specforge driving the built binary against a throwaway
//! git repo with a *fake* normalize agent (a `bash -c` script), so no real LLM
//! is required. Exercises the entry gate: rigor pass → draft, rigor fail →
//! sentinel (HOTL escalation), the harness contract floor, and ratify.

use std::fs;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "seed\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
}

/// Write a config whose "agent" is a bash script that drains stdin and prints
/// `agent_output` verbatim.
fn write_config(repo: &Path, agent_output: &str) {
    let script = format!("cat >/dev/null; cat <<'FORGE_EOF'\n{agent_output}\nFORGE_EOF");
    let cfg = format!(
        r#"
[project]
name = "Demo"
root = "."

[agent]
command = "bash"
args = ["-c", {script:?}]

[output]
spec_dir = "specs"
sentinel = ".forge-pending"
"#,
    );
    fs::write(repo.join("specforge.toml"), cfg).unwrap();
}

fn forge(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_specforge"))
        .current_dir(repo)
        .args(["--config", "specforge.toml", "--date", "2026-01-01"])
        .args(args)
        .output()
        .expect("specforge runs")
}

const GOOD_DRAFT: &str = r#"[[requirement]]
id = "R1"
statement = "同一IPから60s内に5回失敗で429"
acceptance = ["5回目まで通る", "6回目は429", "Retry-Afterヘッダ"]
canon = ["docs/auth.md#rate-limit"]
falsifiable = true

<<<SPEC_DRAFT>>>
rigor: pass
needs_user: no
summary: rate-limit を1要求に正規化"#;

#[test]
fn rigor_pass_writes_draft_no_sentinel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, GOOD_DRAFT);
    fs::write(repo.join("req.md"), "ログインを制限したい\n").unwrap();

    let out = forge(
        repo,
        &[
            "draft",
            "--id",
            "login",
            "--title",
            "ログイン制限",
            "--req",
            "req.md",
            "--canon",
            "docs/auth.md#rate-limit",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let spec = fs::read_to_string(repo.join("specs/login.toml")).unwrap();
    assert!(spec.contains("status = \"draft\""), "draft status:\n{spec}");
    assert!(spec.contains("id = \"login\""));
    assert!(
        spec.contains("provenance_commit = "),
        "pinned to canon commit"
    );
    assert!(spec.contains("[[requirement]]"));
    assert!(spec.contains("Retry-After"));
    // The trailer must not leak into the persisted spec.
    assert!(!spec.contains("<<<SPEC_DRAFT>>>"));
    // Rigor passed cleanly -> no escalation sentinel.
    assert!(
        !repo.join(".forge-pending").exists(),
        "no sentinel on clean rigor"
    );
}

/// What a REAL model actually emits (the staged-C PoC surfaced this): a reasoning
/// preamble, then the requirement TOML inside a ```toml fence, then the trailer.
/// The earlier fake (`GOOD_DRAFT`) emitted bare clean TOML, so the harness's
/// whole-body `toml::from_str` looked fine in tests yet failed on the real agent.
const REALISTIC_DRAFT: &str = r#"canon/clamp.md を読みました。全決定点 (下限・上限・中間・型) が
確定しており、各ゲートを判定します。G1 接地 OK / G2 沈黙なし / G3 矛盾なし / G4 反証可能。

```toml
[[requirement]]
id = "R1"
statement = "clamp_score(n) は入力を閉区間 0..=100 にクランプする"
acceptance = ["n<0 は 0", "n>100 は 100", "0..=100 は n のまま", "戻り値は int"]
canon = ["canon/clamp.md"]
falsifiable = true
```

以上で全 requirement が G1–G4 を満たすため draft を出します。

<<<SPEC_DRAFT>>>
rigor: pass
needs_user: no
summary: clamp_score を1要求に正規化"#;

#[test]
fn rigor_pass_extracts_toml_from_prose_and_fence() {
    // Regression for the bug the PoC found: the draft path must extract the
    // requirement TOML out of a prose+fence body, not parse the whole body.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, REALISTIC_DRAFT);
    fs::write(repo.join("req.md"), "スコアを範囲に収めたい\n").unwrap();

    let out = forge(
        repo,
        &[
            "draft",
            "--id",
            "clamp",
            "--req",
            "req.md",
            "--canon",
            "canon/clamp.md",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let spec = fs::read_to_string(repo.join("specs/clamp.toml")).unwrap();
    assert!(
        spec.contains("status = \"draft\""),
        "draft written:\n{spec}"
    );
    assert!(spec.contains("id = \"R1\""));
    assert!(spec.contains("clamp_score"));
    assert!(spec.contains("falsifiable = true"));
    // The extraction worked, not just "didn't crash": neither the prose preamble
    // nor the fence markers leak into the persisted spec.
    assert!(
        !spec.contains("読みました"),
        "prose preamble must not leak:\n{spec}"
    );
    assert!(
        !spec.contains("```"),
        "fence markers must not leak:\n{spec}"
    );
    assert!(!spec.contains("<<<SPEC_DRAFT>>>"), "trailer must not leak");
    assert!(
        !repo.join(".forge-pending").exists(),
        "clean rigor -> no sentinel"
    );
}

#[test]
fn rigor_fail_raises_sentinel_and_writes_no_draft() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    // Agent cannot rigorously specify: escalation, not fabrication.
    write_config(
        repo,
        "canon が閾値について沈黙している。docs/auth.md に rate-limit の回数/窓を追加してほしい。\n\n<<<SPEC_DRAFT>>>\nrigor: fail\nneeds_user: yes\nsummary: 閾値が canon に未定義 (G2 沈黙)",
    );
    fs::write(repo.join("req.md"), "ログインを制限したい\n").unwrap();

    let out = forge(repo, &["draft", "--id", "login", "--req", "req.md"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // HOTL: no draft fabricated.
    assert!(
        !repo.join("specs/login.toml").exists(),
        "no draft on rigor fail"
    );
    // Sentinel raised with the request body for the human to pull.
    let sentinel = fs::read_to_string(repo.join(".forge-pending")).unwrap();
    assert!(sentinel.contains("spec: login"));
    assert!(
        sentinel.contains("summary: 閾値が canon に未定義 (G2 沈黙)"),
        "sentinel:\n{sentinel}"
    );
    assert!(
        sentinel.contains("docs/auth.md に rate-limit"),
        "request body carried:\n{sentinel}"
    );
}

#[test]
fn harness_rejects_overclaimed_rigor() {
    // Agent claims rigor:pass but emits an ungrounded requirement (no canon, not
    // falsifiable). The deterministic floor must catch it and escalate.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    let overclaim = "[[requirement]]\nid = \"R1\"\nstatement = \"なんとなく速くする\"\nacceptance = []\ncanon = []\nfalsifiable = false\n\n<<<SPEC_DRAFT>>>\nrigor: pass\nneeds_user: no\nsummary: over-claim";
    write_config(repo, overclaim);
    fs::write(repo.join("req.md"), "速くしたい\n").unwrap();

    let out = forge(repo, &["draft", "--id", "x", "--req", "req.md"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !repo.join("specs/x.toml").exists(),
        "no draft persisted for over-claim"
    );
    let sentinel = fs::read_to_string(repo.join(".forge-pending")).unwrap();
    assert!(
        sentinel.contains("過大主張") || sentinel.contains("契約"),
        "sentinel:\n{sentinel}"
    );
}

#[test]
fn missing_marker_exits_3_no_draft_no_sentinel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, "I forgot the trailer entirely");
    fs::write(repo.join("req.md"), "x\n").unwrap();

    let out = forge(repo, &["draft", "--id", "x", "--req", "req.md"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(!repo.join("specs/x.toml").exists());
    assert!(!repo.join(".forge-pending").exists());
}

#[test]
fn agent_nonzero_exit_maps_to_4() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    let cfg = r#"
[project]
name = "Demo"
root = "."
[agent]
command = "bash"
args = ["-c", "cat >/dev/null; exit 7"]
[output]
spec_dir = "specs"
sentinel = ".forge-pending"
"#;
    fs::write(repo.join("specforge.toml"), cfg).unwrap();
    fs::write(repo.join("req.md"), "x\n").unwrap();

    let out = forge(repo, &["draft", "--id", "x", "--req", "req.md"]);
    assert_eq!(out.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("code 7"),
        "true agent code on stderr: {stderr}"
    );
}

#[test]
fn ratify_promotes_draft_and_pins_consent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, GOOD_DRAFT);
    fs::write(repo.join("req.md"), "x\n").unwrap();
    assert!(forge(
        repo,
        &[
            "draft",
            "--id",
            "login",
            "--req",
            "req.md",
            "--canon",
            "docs/auth.md#rate-limit"
        ]
    )
    .status
    .success());

    let out = forge(
        repo,
        &["ratify", "--id", "login", "-m", "受け入れ条件に合意"],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let spec = fs::read_to_string(repo.join("specs/login.toml")).unwrap();
    assert!(spec.contains("status = \"ratified\""), "promoted:\n{spec}");
    assert!(spec.contains("[spec.ratification]"));
    assert!(spec.contains("reason = \"受け入れ条件に合意\""));
    assert!(spec.contains("canon_commit = "));
    assert!(spec.contains("fingerprint = "));
}

#[test]
fn ratify_requires_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, GOOD_DRAFT);
    fs::write(repo.join("req.md"), "x\n").unwrap();
    assert!(forge(
        repo,
        &[
            "draft",
            "--id",
            "login",
            "--req",
            "req.md",
            "--canon",
            "docs/auth.md#rate-limit"
        ]
    )
    .status
    .success());

    let out = forge(repo, &["ratify", "--id", "login", "-m", "   "]);
    assert_eq!(out.status.code(), Some(2), "blank reason rejected");
}

#[test]
fn ratify_missing_spec_is_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, GOOD_DRAFT);
    let out = forge(repo, &["ratify", "--id", "nope", "-m", "x"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn prompt_subcommand_renders_without_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(repo, "unused");
    fs::write(repo.join("req.md"), "ログインを制限したい\n").unwrap();

    let out = forge(
        repo,
        &[
            "prompt",
            "--id",
            "login",
            "--req",
            "req.md",
            "--canon",
            "docs/auth.md#rate-limit",
        ],
    );
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Demo"));
    assert!(stdout.contains("docs/auth.md#rate-limit"));
    assert!(stdout.contains("ログインを制限したい"));
    assert!(stdout.contains("<<<SPEC_DRAFT>>>"));
    assert!(!stdout.contains("{{"));
}

#[test]
fn ack_clears_escalation_sentinel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_config(
        repo,
        "不足。\n\n<<<SPEC_DRAFT>>>\nrigor: fail\nneeds_user: yes\nsummary: 不足",
    );
    fs::write(repo.join("req.md"), "x\n").unwrap();
    assert!(forge(repo, &["draft", "--id", "login", "--req", "req.md"])
        .status
        .success());
    assert!(repo.join(".forge-pending").exists(), "sentinel raised");

    assert!(forge(repo, &["ack"]).status.success());
    assert!(!repo.join(".forge-pending").exists(), "ack cleared it");
}
