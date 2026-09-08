//! End-to-end: two concurrent *processes* driving the same project's queue.
//!
//! The unit tests in `store.rs`/`driver.rs` use threads inside one process.
//! These drive the real built binary in separate OS processes, which is what
//! two `/flow` sessions actually are, and assert on real stdout.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-cd-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // A real repo, because the store resolves per repo and a cwd with no repo
    // root above it now has NO store at all (`config::StoreLocation::NoProject`
    // — the cross-project `~/.backlog` fallback was removed). This fixture used
    // to lean on that fallback: with `HOME` swapped it was harmless, but it
    // also meant the test drove a shape no real driver has. `git init` puts it
    // back on the real path, and the subject of the test (two processes, one
    // queue, disjoint tasks) is untouched either way.
    let st = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git must be available to run this test — a skip here would report green on a case that never ran");
    assert!(st.success(), "git init failed in {}", dir.display());
    dir
}

fn spawn(args: &[&str], home: &Path) -> Child {
    // Pin the child's cwd to the same isolated `home` dir every call site
    // already builds. The task store's LOCATION is resolved from the child
    // process's OWN cwd (not from `--project`, not from `HOME`) — see
    // `crates/backlog/src/config.rs::locate`. Left unset, every
    // spawned process here would silently inherit this test binary's own
    // cwd (the real harness repo checkout it was built and run from) and
    // land `add`/`next --claim` writes in that repo's TRACKED
    // `.backlog/tasks.toml`, corrupting the real queue. `home` itself has no
    // `.git` above it (a fresh dir under the system temp root), so this also
    // makes every driver-registry command here consistent with the isolated
    // `HOME` it already uses — none of those commands are cwd-sensitive
    // (`driver`/`lock` resolve via `base_dir("backlog")`, keyed off `HOME`
    // only), so pinning cwd changes no other test's behavior.
    Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", home)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns")
}

fn run(args: &[&str], home: &Path) -> (i32, String, String) {
    let out = spawn(args, home).wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON ({e}), got: {stdout}"))
}

/// DoD 1, end to end: two concurrent driver *processes* claiming from the same
/// project's queue are handed DIFFERENT tasks and neither is refused. Under the
/// old design the second `/flow` never got this far — it stood down at
/// `backlog lock acquire`.
#[test]
fn two_concurrent_driver_processes_get_disjoint_tasks() {
    let home = temp_home("disjoint");
    // The store is this repo's, so the project IS this repo. A `--project`
    // naming anything else is now refused rather than silently filtered (see
    // `main::assert_store_scopes_to`), which is what the old `"/p"` label was.
    let project = home.to_str().unwrap().to_string();
    for i in 0..4 {
        let title = format!("Task {i}");
        let (code, _, err) = run(
            &[
                "add",
                "--title",
                &title,
                "--project",
                &project,
                "--priority",
                "p1",
            ],
            &home,
        );
        assert_eq!(code, 0, "add must succeed: {err}");
    }

    // Launch both claimers as close together as possible.
    let a = spawn(&["next", "--claim", "--project", &project], &home);
    let b = spawn(&["next", "--claim", "--project", &project], &home);
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();

    let a_out = String::from_utf8_lossy(&a.stdout).into_owned();
    let b_out = String::from_utf8_lossy(&b.stdout).into_owned();
    assert_eq!(a.status.code(), Some(0), "driver A must not be refused");
    assert_eq!(b.status.code(), Some(0), "driver B must not be refused");
    assert!(
        !a_out.contains("no pending tasks"),
        "driver A got no work while 4 tasks were pending: {a_out}"
    );
    assert!(
        !b_out.contains("no pending tasks"),
        "driver B got no work while 4 tasks were pending: {b_out}"
    );

    let a_id = json(&a_out)["id"].as_str().unwrap().to_string();
    let b_id = json(&b_out)["id"].as_str().unwrap().to_string();
    assert_ne!(
        a_id, b_id,
        "two concurrent drivers must be handed different tasks (got {a_id} twice)"
    );
    assert_eq!(json(&a_out)["status"], "claimed");
    assert_eq!(json(&b_out)["status"], "claimed");

    // A driver that picks with `next --claim` still gets the computed
    // `hashkey` it needs for the cross-session claim registry and for
    // `overwatch begin --key` — the same field `list --json` supplies. Without
    // it, switching the pick from `list` to `next --claim` would silently drop
    // those keys.
    for out in [&a_out, &b_out] {
        let hk = json(out)["hashkey"].as_str().unwrap_or("").to_string();
        assert!(!hk.is_empty(), "next --claim must emit a hashkey: {out}");
    }
    assert_ne!(
        json(&a_out)["hashkey"],
        json(&b_out)["hashkey"],
        "different tasks must have different hashkeys"
    );

    // And a third driver still gets one of the two remaining tasks.
    let (code, out, err) = run(&["next", "--claim", "--project", &project], &home);
    assert_eq!(code, 0, "third driver was refused: {err}");
    let c_id = json(&out)["id"].as_str().unwrap().to_string();
    assert!(c_id != a_id && c_id != b_id, "third driver got a duplicate");
}

/// DoD 2, end to end: `backlog lock status` answers correctly at 0, 1 and 2+
/// concurrently-registered drivers, with no exclusive lock involved anywhere.
#[test]
fn lock_status_answers_liveness_for_zero_one_and_many_drivers() {
    let home = temp_home("presence");

    // 0 drivers.
    let (code, out, _) = run(&["lock", "status", "--project", "/p"], &home);
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "none", "no drivers → `none`");

    // 1 driver.
    let (code, _, err) = run(
        &[
            "driver",
            "register",
            "--session-id",
            "sess-a",
            "--project",
            "/p",
        ],
        &home,
    );
    assert_eq!(code, 0, "first registration must succeed: {err}");
    let (_, out, _) = run(&["lock", "status", "--project", "/p"], &home);
    let v = json(&out);
    assert_eq!(v["kind"], "driver-presence");
    assert_eq!(v["driver_count"], 1);
    assert_eq!(v["session_id"], "sess-a");
    assert!(
        v.get("stale").is_none(),
        "a live driver is not stale: {out}"
    );

    // 2 drivers — the state the exclusive lock could not represent. The second
    // registration must NOT be refused.
    let (code, _, err) = run(
        &[
            "driver",
            "register",
            "--session-id",
            "sess-b",
            "--project",
            "/p",
        ],
        &home,
    );
    assert_eq!(
        code, 0,
        "a second concurrent driver must register, not be refused: {err}"
    );
    let (_, out, _) = run(&["lock", "status", "--project", "/p"], &home);
    let v = json(&out);
    assert_eq!(v["driver_count"], 2, "{out}");
    let ids: Vec<&str> = v["drivers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["session_id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"sess-a") && ids.contains(&"sess-b"),
        "{ids:?}"
    );

    // The cross-project scan `daily` uses (no --project) sees them too.
    let (_, out, _) = run(&["lock", "status"], &home);
    assert_eq!(json(&out)["driver_count"], 2, "{out}");

    // `driver status` states liveness explicitly rather than by silence.
    let (_, out, _) = run(&["driver", "status", "--project", "/p"], &home);
    let v = json(&out);
    assert_eq!(v["active"], true);
    assert_eq!(v["undetermined"], false);
    assert_eq!(v["count"], 2);

    // Back to 1, then back to 0.
    run(
        &[
            "driver",
            "unregister",
            "--session-id",
            "sess-a",
            "--project",
            "/p",
        ],
        &home,
    );
    let (_, out, _) = run(&["lock", "status", "--project", "/p"], &home);
    assert_eq!(json(&out)["driver_count"], 1, "{out}");
    run(
        &[
            "driver",
            "unregister",
            "--session-id",
            "sess-b",
            "--project",
            "/p",
        ],
        &home,
    );
    let (_, out, _) = run(&["lock", "status", "--project", "/p"], &home);
    assert_eq!(
        out.trim(),
        "none",
        "after every driver unregisters, liveness is `none` again"
    );
}

/// Registrations are per-project: a driver on another project must not make my
/// project look busy (the property the per-project lock file introduced).
#[test]
fn another_projects_driver_does_not_make_my_project_look_busy() {
    let home = temp_home("scoped");
    run(
        &[
            "driver",
            "register",
            "--session-id",
            "s",
            "--project",
            "/other",
        ],
        &home,
    );
    let (_, out, _) = run(&["lock", "status", "--project", "/mine"], &home);
    assert_eq!(out.trim(), "none", "got: {out}");
    // ...but the project-agnostic scan still finds it.
    let (_, out, _) = run(&["lock", "status"], &home);
    assert_eq!(json(&out)["driver_count"], 1, "{out}");
}

fn driver_record_paths(home: &Path) -> Vec<PathBuf> {
    let root = home.join(".backlog").join("drivers");
    let mut out = Vec::new();
    for proj in std::fs::read_dir(&root)
        .expect("drivers root exists")
        .flatten()
    {
        for f in std::fs::read_dir(proj.path()).unwrap().flatten() {
            if f.path().extension().is_some_and(|e| e == "driver") {
                out.push(f.path());
            }
        }
    }
    out
}

/// DoD 2, end to end: a stale registration is reaped exactly as a stale
/// exclusive lock is — it stops counting as live, renders with `"stale": true`
/// (which every existing consumer reads as "not active"), and its record is
/// deleted by the next registration.
#[test]
fn a_stale_driver_registration_is_reaped_like_a_stale_lock() {
    let home = temp_home("stale");
    run(
        &[
            "driver",
            "register",
            "--session-id",
            "ghost",
            "--project",
            "/p",
        ],
        &home,
    );
    let records = driver_record_paths(&home);
    assert_eq!(records.len(), 1);

    // Back-date the heartbeat past the TTL (1800s, same constant as the lock).
    let mut v = json(&std::fs::read_to_string(&records[0]).unwrap());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    v["heartbeat_at"] = serde_json::json!(now - 1801);
    std::fs::write(&records[0], serde_json::to_string(&v).unwrap()).unwrap();

    let (_, out, _) = run(&["lock", "status", "--project", "/p"], &home);
    let s = json(&out);
    assert_eq!(s["stale"], true, "{out}");
    assert_eq!(s["driver_count"], 0, "{out}");
    let (_, out, _) = run(&["driver", "status", "--project", "/p"], &home);
    assert_eq!(json(&out)["active"], false, "{out}");

    // A fresh registration reaps the stale record.
    run(
        &[
            "driver",
            "register",
            "--session-id",
            "fresh",
            "--project",
            "/p",
        ],
        &home,
    );
    let (_, out, _) = run(&["lock", "status", "--project", "/p"], &home);
    let s = json(&out);
    assert_eq!(s["driver_count"], 1, "{out}");
    assert_eq!(s["session_id"], "fresh");
    assert_eq!(
        driver_record_paths(&home).len(),
        1,
        "the stale record file must have been deleted"
    );
}

/// §3, end to end: an unreadable registry must not print `none`. `none` is what
/// makes autoflow fire its auto-loop and daily run its tasks — the permissive
/// answer to a question we failed to answer.
#[test]
fn an_unreadable_registry_does_not_report_none() {
    let home = temp_home("undetermined");
    run(
        &["driver", "register", "--session-id", "s", "--project", "/p"],
        &home,
    );
    let records = driver_record_paths(&home);
    std::fs::write(&records[0], "{ not json").unwrap();

    let (code, out, _) = run(&["lock", "status", "--project", "/p"], &home);
    assert_eq!(code, 0, "status stays exit 0; the JSON carries the answer");
    assert_ne!(
        out.trim(),
        "none",
        "a corrupt registry must never render as `none`"
    );
    let v = json(&out);
    assert_eq!(v["undetermined"], true, "{out}");
    assert!(
        v.get("stale").is_none(),
        "undetermined must not carry `stale: true`, which reads as inactive: {out}"
    );

    let (_, out, _) = run(&["driver", "status", "--project", "/p"], &home);
    let v = json(&out);
    assert_eq!(v["active"], true, "{out}");
    assert_eq!(v["undetermined"], true, "{out}");
}
