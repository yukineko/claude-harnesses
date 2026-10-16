#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: `budgetguard status --json` loads the ledger with the lenient
//! `Ledger::load`, so a corrupt ledger reads as day_usd 0 and pressure false
//! (headroom). `check` already reports it as undetermined (exit 2).

use std::process::{Command, Stdio};

#[test]
#[ignore = "RED repro: status --json reads a corrupt ledger as zero spend / no pressure"]
fn status_json_corrupt_ledger_is_not_zero_spend() {
    let base = std::env::temp_dir().join(format!("bg-status-corrupt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let cwd = base.join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        cwd.join("budgetguard.toml"),
        "[daily]\nwarn_usd = 5.0\nblock_usd = 10.0\n",
    )
    .unwrap();
    let state = home.join(".budgetguard/state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("ledger.json"), "{ corrupt").unwrap();

    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_budgetguard"))
            .args(args)
            .env("HOME", &home)
            .env_remove("BUDGETGUARD_DISABLE")
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .output()
            .expect("binary runs")
    };
    // Control: check already treats this ledger as undetermined.
    let ctl = run(&["check"]);
    assert_eq!(ctl.status.code(), Some(2), "control: check fails closed");

    let out = run(&["status", "--json"]);
    let _ = std::fs::remove_dir_all(&base);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let code = out.status.code();
    let reads_as_headroom =
        stdout.contains("\"pressure\":false") && stdout.contains("\"day_usd\":0");
    assert!(
        code != Some(0) && !reads_as_headroom,
        "corrupt ledger reported as zero spend / no pressure. exit={code:?} stdout={stdout:?}"
    );
}
