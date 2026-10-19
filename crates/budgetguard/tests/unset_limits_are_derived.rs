// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-10-19: 「設定上限をきめて、決められないならその値にする、
//! 具体的にどうするかは経験値があればそれにもとづいて決める」.
//!
//! "No limit" (no config file, or every `[session]`/`[daily]` USD limit 0) is
//! no longer a silent pass. The limits are derived from the ledger history
//! (Tukey fence, at least 7 past days) or, failing that, the built-in
//! defaults; a ledger that cannot be read is undetermined. `check` and
//! `status --json` report `limits_source`: config | derived | default.
//! Explicit opt-outs (`enabled = false`, `BUDGETGUARD_DISABLE=1`) stay no-ops.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    home: PathBuf,
    cwd: PathBuf,
}

fn fixture(tag: &str) -> Fx {
    let base = std::env::temp_dir().join(format!(
        "budgetguard-unset-{}-{}-{}",
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

const ALL_ZERO: &str =
    "[session]\nwarn_usd = 0.0\nblock_usd = 0.0\n\n[daily]\nwarn_usd = 0.0\nblock_usd = 0.0\n";

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Fx {
    fn config(&self, text: &str) {
        std::fs::write(self.cwd.join("budgetguard.toml"), text).unwrap();
    }

    fn ledger_path(&self) -> PathBuf {
        self.home.join(".budgetguard/state/ledger.json")
    }

    fn write_ledger_raw(&self, text: &str) {
        let p = self.ledger_path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// Seven past days (2020-01-01..07) costing $1..$7, one session each.
    /// Sorted day totals 1..7: Q1 = 2.5, Q3 = 5.5, fence = 10 = daily block;
    /// all days are inliers, p90 = 6.4 = daily warn; session fence = 10.
    fn seven_day_history(&self) {
        let days: Vec<String> = (1..=7)
            .map(|d| format!("\"2020-01-0{d}\":{{\"sessions\":{{\"h{d}\":{d}.0}}}}"))
            .collect();
        self.write_ledger_raw(&format!("{{\"days\":{{{}}}}}", days.join(",")));
    }

    /// A transcript pricing to $30 (opus, 1M in + 1M out).
    fn transcript(&self) -> PathBuf {
        let p = self.cwd.join("t.jsonl");
        std::fs::write(
            &p,
            "{\"type\":\"assistant\",\"timestamp\":\"2026-06-22T10:00:01Z\",\"message\":{\"model\":\"claude-opus-4-8\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}],\"usage\":{\"input_tokens\":1000000,\"output_tokens\":1000000,\"cache_read_input_tokens\":0}}}\n",
        )
        .unwrap();
        p
    }

    fn run(&self, args: &[&str], stdin: &str, disable_env: bool) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_budgetguard"));
        cmd.args(args)
            .env("HOME", &self.home)
            .env_remove("BUDGETGUARD_DISABLE")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if disable_env {
            cmd.env("BUDGETGUARD_DISABLE", "1");
        }
        let mut child = cmd.spawn().expect("binary spawns");
        if let Some(mut s) = child.stdin.take() {
            let _ = s.write_all(stdin.as_bytes());
        }
        let out = child.wait_with_output().expect("binary runs");
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn gate(&self, disable_env: bool) -> Out {
        let payload = serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "unset-s1",
            "transcript_path": self.transcript(),
            "cwd": self.cwd,
        })
        .to_string();
        self.run(&["gate"], &payload, disable_env)
    }
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {s:?}"))
}

fn close(v: &serde_json::Value, want: f64) -> bool {
    v.as_f64().is_some_and(|x| (x - want).abs() < 1e-6)
}

// ---- default limits (no history) --------------------------------------------

#[test]
fn absent_config_no_history_check_reports_default_limits() {
    let fx = fixture("abs-def-check");
    let o = fx.run(&["check"], "", false);
    let v = json(&o.stdout);
    assert_eq!(v["limits_source"], "default", "{v}");
    assert!(close(&v["daily_block_usd"], 1871.0), "{v}");
    assert!(close(&v["daily_warn_usd"], 1336.0), "{v}");
    assert_eq!(v["verdict"], "within", "{v}");
    assert_eq!(o.code, 0);
}

#[test]
fn absent_config_no_history_status_reports_default_limits() {
    let fx = fixture("abs-def-status");
    let o = fx.run(&["status", "--json"], "", false);
    let v = json(&o.stdout);
    assert_eq!(v["limits_source"], "default", "{v}");
    assert!(close(&v["daily_block_usd"], 1871.0), "{v}");
    assert_eq!(o.code, 0);
}

// ---- derived limits (>= 7 past days) -------------------------------------------

#[test]
fn absent_config_with_history_check_reports_derived_limits() {
    let fx = fixture("abs-der-check");
    fx.seven_day_history();
    let o = fx.run(&["check"], "", false);
    let v = json(&o.stdout);
    assert_eq!(v["limits_source"], "derived", "{v}");
    assert!(close(&v["daily_block_usd"], 10.0), "{v}");
    assert!(close(&v["daily_warn_usd"], 6.4), "{v}");
}

/// The derived daily block ($10) is below today's $30 session, so the stop is
/// blocked — on unmodified main the absent config meant "no limit" and this
/// stop was allowed.
#[test]
fn absent_config_with_history_gate_blocks_over_the_derived_daily_limit() {
    let fx = fixture("abs-der-gate");
    fx.seven_day_history();
    let o = fx.gate(false);
    let v = json(&o.stdout);
    assert_eq!(
        v["decision"], "block",
        "stdout {:?} stderr {:?}",
        o.stdout, o.stderr
    );
    assert!(
        v["reason"].as_str().unwrap().contains("日次予算超過"),
        "{v}"
    );
}

#[test]
fn all_zero_config_with_history_gate_blocks_over_the_derived_daily_limit() {
    let fx = fixture("zero-der-gate");
    fx.config(ALL_ZERO);
    fx.seven_day_history();
    let o = fx.gate(false);
    assert_eq!(
        json(&o.stdout)["decision"],
        "block",
        "stdout {:?} stderr {:?}",
        o.stdout,
        o.stderr
    );
    let c = json(&fx.run(&["check"], "", false).stdout);
    assert_eq!(c["limits_source"], "derived", "{c}");
}

/// User ruling 2026-10-19 「警告はださなくていい、結果がみたいだけ」: the session
/// axis gets no derived warn or block. History: 7 past days of ten $10
/// sessions (day total $100, so the daily fence is $100; session fence $10).
/// Today's one $30 session is far over the session fence and under every
/// daily limit: the stop is allowed with NO warning, and check/status report
/// the session fence as information.
#[test]
fn absent_config_derives_no_session_warning_but_reports_the_fence() {
    let fx = fixture("abs-no-session-warn");
    let days: Vec<String> = (1..=7)
        .map(|d| {
            let sessions: Vec<String> = (0..10).map(|i| format!("\"h{d}-{i}\":10.0")).collect();
            format!(
                "\"2020-01-0{d}\":{{\"sessions\":{{{}}}}}",
                sessions.join(",")
            )
        })
        .collect();
    fx.write_ledger_raw(&format!("{{\"days\":{{{}}}}}", days.join(",")));
    let g = fx.gate(false);
    assert_eq!(g.code, 0);
    assert!(
        g.stdout.trim().is_empty(),
        "no session warning may be emitted: {:?} / {:?}",
        g.stdout,
        g.stderr
    );
    let c = json(&fx.run(&["check"], "", false).stdout);
    assert_eq!(c["limits_source"], "derived", "{c}");
    assert!(close(&c["session_fence_usd"], 10.0), "{c}");
    let s = json(&fx.run(&["status", "--json"], "", false).stdout);
    assert!(close(&s["session_fence_usd"], 10.0), "{s}");
    assert!(close(&s["session_warn_usd"], 0.0), "{s}");
    assert!(close(&s["session_block_usd"], 0.0), "{s}");
}

// ---- corrupt ledger: undetermined, never the defaults ----------------------------

#[test]
fn absent_config_corrupt_ledger_is_undetermined_everywhere() {
    let fx = fixture("abs-corrupt");
    fx.write_ledger_raw("{ not json");
    let g = fx.gate(false);
    assert_eq!(
        json(&g.stdout)["decision"],
        "block",
        "gate: stdout {:?} stderr {:?}",
        g.stdout,
        g.stderr
    );
    let c = fx.run(&["check"], "", false);
    assert_eq!(c.code, 2, "check: {:?}", c.stdout);
    assert_eq!(json(&c.stdout)["verdict"], "undetermined");
    let s = fx.run(&["status", "--json"], "", false);
    assert_eq!(s.code, 2, "status: {:?}", s.stdout);
    assert_eq!(json(&s.stdout)["undetermined"], true);
    assert_eq!(
        std::fs::read_to_string(fx.ledger_path()).unwrap(),
        "{ not json",
        "the corrupt ledger must be preserved"
    );
}

// ---- controls -------------------------------------------------------------------

#[test]
fn control_enabled_false_is_a_no_op() {
    let fx = fixture("ctl-disabled");
    fx.config("enabled = false\n");
    fx.seven_day_history();
    let g = fx.gate(false);
    assert_eq!(g.code, 0);
    assert!(
        g.stdout.trim().is_empty(),
        "enabled=false gate: {:?}",
        g.stdout
    );
    let c = fx.run(&["check"], "", false);
    assert_eq!(json(&c.stdout)["verdict"], "disabled");
}

#[test]
fn control_disable_env_is_a_no_op() {
    let fx = fixture("ctl-env");
    fx.seven_day_history();
    let g = fx.gate(true);
    assert_eq!(g.code, 0);
    assert!(
        g.stdout.trim().is_empty(),
        "env-disabled gate: {:?}",
        g.stdout
    );
}

#[test]
fn control_one_configured_axis_reports_config_and_keeps_others_off() {
    let fx = fixture("ctl-one");
    fx.config("[daily]\nblock_usd = 1000.0\n");
    fx.seven_day_history();
    let g = fx.gate(false);
    assert_eq!(g.code, 0);
    assert!(
        g.stdout.trim().is_empty(),
        "under the one configured limit the stop is allowed: {:?} / {:?}",
        g.stdout,
        g.stderr
    );
    let v = json(&fx.run(&["check"], "", false).stdout);
    assert_eq!(v["limits_source"], "config", "{v}");
    assert!(close(&v["daily_block_usd"], 1000.0), "{v}");
    assert!(close(&v["daily_warn_usd"], 0.0), "{v}");
}
