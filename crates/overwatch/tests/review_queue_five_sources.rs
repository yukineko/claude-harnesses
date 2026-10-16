#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-10-15 removed the `needs-ruling` backlog status, so
//! `overwatch review-queue` merges EXACTLY five sources: systemic violations,
//! canary rollbacks, AI review findings, condukt escalations, unresolved merge
//! conflicts. This test plants one entry per source (plus a legacy
//! `status = "needs-ruling"` backlog row, which used to feed a sixth source)
//! and asserts, through the real binary, that the set of `kind`s in
//! `review-queue --json` is exactly those five and nothing else.
//! Hermetic: temp HOME + temp git repo as cwd.
//! Written by an independent test writer (CLAUDE.md §2(a)).
#![cfg(unix)]

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

fn ow(home: &Path, work: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_overwatch"))
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(work)
        .output()
        .expect("spawn overwatch");
    assert!(
        out.status.success(),
        "overwatch {args:?} exited non-zero: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("stdout utf8")
}

#[test]
fn review_queue_merges_exactly_five_sources() {
    let base = std::env::temp_dir().join(format!("ow-five-src-{}", std::process::id()));
    let home = base.join("home");
    let work = base.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(work.join(".backlog")).unwrap();
    let st = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&work)
        .status()
        .expect("git init");
    assert!(st.success(), "git init failed");

    // 1. systemic: same signature across three distinct tasks.
    for t in ["task-a", "task-b", "task-c"] {
        ow(
            &home,
            &work,
            &[
                "record-violation",
                "--source",
                "blastguard",
                "--discriminator",
                "rm-rf",
                "--task",
                t,
                "--session",
                t,
            ],
        );
    }
    // 2. canary rollback.
    ow(
        &home,
        &work,
        &[
            "record-rollback",
            "--plugin",
            "overwatch",
            "--from-version",
            "0.1.7",
            "--to-version",
            "0.1.8",
            "--stage",
            "1",
            "--reason",
            "systemic",
        ],
    );
    // 3. AI review finding.
    ow(
        &home,
        &work,
        &[
            "record-finding",
            "--verdict",
            "confirmed",
            "--finding-id",
            "F-001",
            "--source",
            "reviewgate",
            "--severity",
            "high",
            "--summary",
            "unchecked unwrap on user input",
            "--file",
            "src/foo.rs",
        ],
    );

    // 4 + 5 are seeded on disk at the paths the production readers use; HOME is
    // set only while deriving those paths (this file has a single test fn, so
    // there is no concurrent env reader).
    std::env::set_var("HOME", &home);
    let repo_root = harness_core::projkey::repo_root(&work);
    let project_key = harness_core::projkey::project_key(&repo_root);
    let mc_path = overwatch::store::merge_conflicts_path(&work).unwrap();
    std::env::remove_var("HOME");

    // 4. condukt escalation (open).
    let esc_path = home
        .join(".condukt")
        .join("state")
        .join(&project_key)
        .join("escalations.json");
    std::fs::create_dir_all(esc_path.parent().unwrap()).unwrap();
    std::fs::write(
        &esc_path,
        r#"{"escalations":[{"id":"esc-open-1","run":"runA","task":"t1",
            "question":"Which migration strategy?","options":["a","b"],
            "recommended":0,"created_at":500,"resolved":false}]}"#,
    )
    .unwrap();

    // 5. unresolved merge conflict.
    std::fs::create_dir_all(mc_path.parent().unwrap()).unwrap();
    let entry = serde_json::json!({
        "conflict_id": "runA/condukt-t2/1000",
        "origin": "merge-conflict",
        "run_id": "runA",
        "branch": "condukt/t2",
        "default_branch": "main",
        "base_ref": "deadbeef",
        "conflicted_files": ["crates/x/src/main.rs"],
        "diff_ours": "our side",
        "diff_theirs": "their side",
        "ts": 1000
    });
    std::fs::write(&mc_path, format!("{entry}\n")).unwrap();

    // Legacy removed-status row: used to feed a sixth source.
    std::fs::write(
        work.join(".backlog/tasks.toml"),
        "[[task]]\nid = \"nr-1\"\ntitle = \"legacy ruling row\"\nproject = \"/x\"\n\
         project_unresolved = false\ntags = []\ntouched_files = []\n\
         status = \"needs-ruling\"\nruling_kind = \"judgment\"\nrationale = \"r\"\n",
    )
    .unwrap();

    let out = ow(&home, &work, &["review-queue", "--json"]);
    let rows: Vec<Value> = serde_json::from_str(out.trim()).expect("review-queue --json parses");
    let kinds: BTreeSet<String> = rows
        .iter()
        .map(|r| r["kind"].as_str().expect("string kind").to_string())
        .collect();
    let want: BTreeSet<String> = [
        "systemic",
        "rollback",
        "ai-finding",
        "escalation",
        "merge-conflict",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        kinds, want,
        "review-queue must merge exactly the five sources; rows={rows:?}"
    );
    assert_eq!(rows.len(), 5, "one planted entry per source: {rows:?}");
}
