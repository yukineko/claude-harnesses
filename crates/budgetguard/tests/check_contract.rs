// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract for `budgetguard check`: the deterministic budget verdict /flow
//! consults before every pick. Exactly one JSON line on stdout with a
//! `verdict` key; exit 0 = within/disabled, 1 = over, 2 = undetermined.
//!
//! clap already exits 2 for an unknown subcommand, so EVERY test asserts the
//! stdout `verdict` value as well — an exit-code-only assertion on the
//! undetermined cases would pass against a binary that has no `check` at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    home: PathBuf,
    cwd: PathBuf,
}

fn fixture(tag: &str) -> Fx {
    let base = std::env::temp_dir().join(format!(
        "budgetguard-check-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = base.join("home");
    let cwd = base.join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    Fx { home, cwd }
}

impl Fx {
    fn config(&self, text: &str) {
        std::fs::write(self.cwd.join("budgetguard.toml"), text).unwrap();
    }
    fn ledger_path(&self) -> PathBuf {
        self.home
            .join(".budgetguard")
            .join("state")
            .join("ledger.json")
    }
    fn write_ledger_raw(&self, text: &str) {
        let p = self.ledger_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    /// A ledger whose TODAY (local date) entry sums to `usd`.
    fn spend_today(&self, usd: f64) {
        let key = local_today();
        self.write_ledger_raw(&format!(
            "{{\"days\":{{\"{key}\":{{\"sessions\":{{\"s1\":{usd}}}}}}}}}"
        ));
    }
}

/// Local calendar date, the key the ledger uses (not UTC).
fn local_today() -> String {
    let out = Command::new("date").arg("+%Y-%m-%d").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn run_check(fx: &Fx, disable_env: bool) -> (i32, String, serde_json::Value) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_budgetguard"));
    cmd.arg("check")
        .env("HOME", &fx.home)
        .env_remove("BUDGETGUARD_DISABLE")
        .current_dir(&fx.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if disable_env {
        cmd.env("BUDGETGUARD_DISABLE", "1");
    }
    let out = cmd.output().expect("binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = stdout.trim_end_matches('\n');
    assert!(
        !line.is_empty() && !line.contains('\n'),
        "check must print exactly ONE line of JSON on stdout, got: {stdout:?}"
    );
    let json: serde_json::Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {stdout:?}"));
    (out.status.code().unwrap_or(-1), stdout, json)
}

fn assert_no_thresholds(json: &serde_json::Value) {
    for k in ["pressure", "day_usd", "daily_warn_usd", "daily_block_usd"] {
        assert!(
            json.get(k).is_none(),
            "undetermined must not carry `{k}` (it would read as a known answer): {json}"
        );
    }
}

#[test]
fn check_within_budget_exits_0_with_verdict_within() {
    let fx = fixture("within");
    fx.config("[daily]\nwarn_usd = 5.0\nblock_usd = 10.0\n");
    fx.spend_today(1.0);
    let (code, out, j) = run_check(&fx, false);
    assert_eq!(j["verdict"], "within", "stdout: {out}");
    assert_eq!(code, 0, "stdout: {out}");
    assert_eq!(j["day_usd"], 1.0);
    assert_eq!(j["daily_warn_usd"], 5.0);
    assert_eq!(j["daily_block_usd"], 10.0);
    assert_eq!(j["pressure"], false);
}

#[test]
fn check_between_warn_and_block_is_within_with_pressure_true() {
    let fx = fixture("pressure");
    fx.config("[daily]\nwarn_usd = 5.0\nblock_usd = 10.0\n");
    fx.spend_today(7.0);
    let (code, out, j) = run_check(&fx, false);
    assert_eq!(j["verdict"], "within", "stdout: {out}");
    assert_eq!(code, 0, "stdout: {out}");
    assert_eq!(j["pressure"], true);
    assert_eq!(j["day_usd"], 7.0);
}

#[test]
fn check_at_or_over_block_threshold_exits_1_with_verdict_over() {
    let fx = fixture("over");
    fx.config("[daily]\nwarn_usd = 5.0\nblock_usd = 10.0\n");
    fx.spend_today(10.0); // boundary: >= block is over
    let (code, out, j) = run_check(&fx, false);
    assert_eq!(j["verdict"], "over", "stdout: {out}");
    assert_eq!(code, 1, "stdout: {out}");
    assert_eq!(j["day_usd"], 10.0);
    assert_eq!(j["daily_block_usd"], 10.0);
    assert_eq!(j["pressure"], true);
}

#[test]
fn check_corrupt_config_is_undetermined_exit_2() {
    let fx = fixture("badcfg");
    fx.config("[daily\nblock_usd = = 1.0\n");
    fx.spend_today(999.0);
    let (code, out, j) = run_check(&fx, false);
    assert_eq!(j["verdict"], "undetermined", "stdout: {out}");
    assert_eq!(code, 2, "stdout: {out}");
    assert!(
        j["error"].as_str().is_some_and(|s| !s.is_empty()),
        "undetermined must carry a non-empty `error` string: {out}"
    );
    assert_no_thresholds(&j);
}

#[test]
fn check_corrupt_ledger_is_undetermined_exit_2() {
    // The important fail-closed case: a corrupt ledger must NOT read as 0 spend.
    let fx = fixture("badledger");
    fx.config("[daily]\nwarn_usd = 5.0\nblock_usd = 10.0\n");
    fx.write_ledger_raw("{ this is not a ledger");
    let (code, out, j) = run_check(&fx, false);
    assert_eq!(j["verdict"], "undetermined", "stdout: {out}");
    assert_eq!(code, 2, "stdout: {out}");
    assert!(
        j["error"].as_str().is_some_and(|s| !s.is_empty()),
        "undetermined must carry a non-empty `error` string: {out}"
    );
    assert_no_thresholds(&j);
}

#[test]
fn check_unreadable_ledger_is_undetermined_exit_2() {
    // ledger.json exists but is a directory: read fails with a non-NotFound error.
    let fx = fixture("dirledger");
    fx.config("[daily]\nwarn_usd = 5.0\nblock_usd = 10.0\n");
    std::fs::create_dir_all(fx.ledger_path()).unwrap();
    let (code, out, j) = run_check(&fx, false);
    assert_eq!(j["verdict"], "undetermined", "stdout: {out}");
    assert_eq!(code, 2, "stdout: {out}");
    assert_no_thresholds(&j);
}

#[test]
fn check_missing_ledger_is_within_with_zero_spend() {
    let fx = fixture("noledger");
    fx.config("[daily]\nwarn_usd = 5.0\nblock_usd = 10.0\n");
    assert!(!Path::new(&fx.ledger_path()).exists());
    let (code, out, j) = run_check(&fx, false);
    assert_eq!(j["verdict"], "within", "stdout: {out}");
    assert_eq!(code, 0, "stdout: {out}");
    assert_eq!(j["day_usd"], 0.0);
    assert_eq!(j["pressure"], false);
}

#[test]
fn check_no_config_at_all_is_within_not_disabled() {
    let fx = fixture("nocfg");
    let (code, out, j) = run_check(&fx, false);
    assert_eq!(j["verdict"], "within", "stdout: {out}");
    assert_eq!(code, 0, "stdout: {out}");
    assert_eq!(j["daily_warn_usd"], 0.0);
    assert_eq!(j["daily_block_usd"], 0.0);
    assert_eq!(j["pressure"], false);
}

#[test]
fn check_enabled_false_is_disabled_exit_0() {
    let fx = fixture("disabled-cfg");
    fx.config("enabled = false\n[daily]\nblock_usd = 1.0\n");
    fx.spend_today(999.0);
    let (code, out, j) = run_check(&fx, false);
    assert_eq!(j["verdict"], "disabled", "stdout: {out}");
    assert_eq!(code, 0, "stdout: {out}");
}

#[test]
fn check_budgetguard_disable_env_is_disabled_exit_0() {
    let fx = fixture("disabled-env");
    fx.config("[daily]\nblock_usd = 1.0\n");
    fx.spend_today(999.0);
    let (code, out, j) = run_check(&fx, true);
    assert_eq!(j["verdict"], "disabled", "stdout: {out}");
    assert_eq!(code, 0, "stdout: {out}");
}
