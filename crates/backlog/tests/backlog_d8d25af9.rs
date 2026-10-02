#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog d8d25af9: `backlog cancel <ID> --reason <R>` moves a task to the
//! terminal `cancelled` status. Black-box tests through the binary and the
//! store files.
//!
//! Written by an independent author, not the implementer.

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    home: PathBuf,
    repo: PathBuf,
}

struct Out {
    rc: i32,
    out: String,
    err: String,
}

impl Fx {
    fn new(tag: &str) -> Fx {
        let root = std::env::temp_dir().join(format!(
            "backlog-d8d25af9-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (home, repo) = (root.join("home"), root.join("repo"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let repo = std::fs::canonicalize(&repo).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
        Fx { home, repo }
    }

    fn run(&self, args: &[&str]) -> Out {
        let o = Command::new(env!("CARGO_BIN_EXE_backlog"))
            .args(args)
            .env("HOME", &self.home)
            .current_dir(&self.repo)
            .stdin(Stdio::null())
            .output()
            .expect("binary runs");
        Out {
            rc: o.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&o.stdout).into_owned(),
            err: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }

    fn add(&self, title: &str) -> String {
        let o = self.run(&[
            "add",
            "--title",
            title,
            "--project",
            self.repo.to_str().unwrap(),
        ]);
        assert_eq!(o.rc, 0, "precondition: add; out={} err={}", o.out, o.err);
        o.out
            .lines()
            .find_map(|l| l.strip_prefix("added: "))
            .unwrap()
            .trim()
            .to_string()
    }

    /// The row with `id` in the given store file (None if file/row absent).
    fn row(&self, file: &str, id: &str) -> Option<toml::Table> {
        let body = std::fs::read_to_string(self.repo.join(".backlog").join(file)).ok()?;
        let doc: toml::Table = toml::from_str(&body).unwrap();
        for v in doc.values() {
            if let Some(arr) = v.as_array() {
                for t in arr {
                    if let Some(t) = t.as_table() {
                        if t.get("id").and_then(|i| i.as_str()) == Some(id) {
                            return Some(t.clone());
                        }
                    }
                }
            }
        }
        None
    }

    /// Row from either file; panics if absent in both.
    fn any_row(&self, id: &str) -> toml::Table {
        self.row("tasks.toml", id)
            .or_else(|| self.row("tasks.done.toml", id))
            .unwrap_or_else(|| panic!("task {id} found in neither tasks.toml nor tasks.done.toml"))
    }

    fn status(&self, id: &str) -> String {
        self.any_row(id)
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string()
    }
}

fn assert_subcommand_exists(o: &Out) {
    assert!(
        !o.err.contains("unrecognized subcommand"),
        "`cancel` subcommand does not exist (non-zero exit is vacuous): {:?}",
        o.err
    );
}

fn notes(row: &toml::Table) -> String {
    row.get("notes")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string()
}

#[test]
fn cancel_pending_task() {
    let fx = Fx::new("pending");
    let id = fx.add("abandon me");
    let o = fx.run(&["cancel", &id, "--reason", "no longer needed zq1"]);
    assert_eq!(o.rc, 0, "cancel failed: out={} err={}", o.out, o.err);
    assert!(
        o.out
            .lines()
            .any(|l| l.trim() == format!("cancelled: {id}")),
        "stdout lacks `cancelled: {id}`: {:?}",
        o.out
    );
    let done = fx
        .row("tasks.done.toml", &id)
        .expect("cancelled row must live in tasks.done.toml");
    assert_eq!(
        done.get("status").and_then(|s| s.as_str()),
        Some("cancelled")
    );
    assert!(
        notes(&done).contains("no longer needed zq1"),
        "notes={done:?}"
    );
    assert!(
        !done.contains_key("defer_until"),
        "defer_until set: {done:?}"
    );
    assert!(
        fx.row("tasks.toml", &id).is_none(),
        "cancelled row must be absent from tasks.toml"
    );
}

#[test]
fn cancelled_task_is_never_handed_out_and_filters_work() {
    let fx = Fx::new("visibility");
    let id = fx.add("abandon me too");
    let c = fx.run(&["cancel", &id, "--reason", "r"]);
    assert_eq!(c.rc, 0, "precondition: cancel; out={} err={}", c.out, c.err);

    let n = fx.run(&["next"]);
    assert!(
        !n.out.contains(&id),
        "next handed out cancelled task: {:?}",
        n.out
    );

    let p = fx.run(&["list", "--status", "pending"]);
    assert!(
        !p.out.contains(&id),
        "list --status pending shows cancelled task: {:?}",
        p.out
    );

    let l = fx.run(&["list", "--status", "cancelled"]);
    assert!(
        l.out.contains(&id),
        "list --status cancelled hides it: out={:?} err={:?}",
        l.out,
        l.err
    );
    assert!(
        !l.err.to_lowercase().contains("unknown status"),
        "unknown status warning: {:?}",
        l.err
    );
}

#[test]
fn cancel_failed_deferred_task_clears_defer_until() {
    let fx = Fx::new("failed");
    let id = fx.add("flaky");
    let f = fx.run(&["fail", &id, "--reason", "x"]);
    assert_eq!(f.rc, 0, "precondition: fail; out={} err={}", f.out, f.err);
    let c = fx.run(&["cancel", &id, "--reason", "giving up"]);
    assert_eq!(c.rc, 0, "cancel failed: out={} err={}", c.out, c.err);
    let row = fx.any_row(&id);
    assert_eq!(
        row.get("status").and_then(|s| s.as_str()),
        Some("cancelled"),
        "{row:?}"
    );
    assert!(
        !row.contains_key("defer_until"),
        "defer_until remains: {row:?}"
    );
}

#[test]
fn cancel_requires_reason() {
    let fx = Fx::new("noreason");
    let id = fx.add("keep me");
    let o = fx.run(&["cancel", &id]);
    assert_subcommand_exists(&o);
    assert!(
        o.err.to_lowercase().contains("reason"),
        "error should name --reason: {:?}",
        o.err
    );
    assert_ne!(o.rc, 0, "cancel without --reason must fail: out={}", o.out);
    assert_eq!(fx.status(&id), "pending");
}

#[test]
fn cancel_done_task_is_refused() {
    let fx = Fx::new("done");
    let id = fx.add("finished");
    let d = fx.run(&["done", &id]);
    assert_eq!(d.rc, 0, "precondition: done; out={} err={}", d.out, d.err);
    let o = fx.run(&["cancel", &id, "--reason", "r"]);
    assert_subcommand_exists(&o);
    assert_ne!(o.rc, 0, "cancelling a done task must fail: out={}", o.out);
    assert_eq!(fx.status(&id), "done");
}

#[test]
fn cancel_is_idempotent_and_does_not_repeat_reason() {
    let fx = Fx::new("idem");
    let id = fx.add("twice");
    let a = fx.run(&["cancel", &id, "--reason", "uniq-reason-k7x"]);
    assert_eq!(a.rc, 0, "first cancel: out={} err={}", a.out, a.err);
    let b = fx.run(&["cancel", &id, "--reason", "uniq-reason-k7x"]);
    assert_eq!(b.rc, 0, "second cancel: out={} err={}", b.out, b.err);
    let row = fx.any_row(&id);
    assert_eq!(
        row.get("status").and_then(|s| s.as_str()),
        Some("cancelled"),
        "{row:?}"
    );
    assert_eq!(
        notes(&row).matches("uniq-reason-k7x").count(),
        1,
        "reason repeated: {row:?}"
    );
}

#[test]
fn cancel_unknown_id_fails() {
    let fx = Fx::new("unknown");
    let _ = fx.add("exists");
    let o = fx.run(&["cancel", "deadbeef", "--reason", "r"]);
    assert_subcommand_exists(&o);
    assert_ne!(o.rc, 0, "unknown id must fail: out={} err={}", o.out, o.err);
}

#[test]
fn fail_on_cancelled_task_is_refused() {
    let fx = Fx::new("failcancelled");
    let id = fx.add("terminal");
    let c = fx.run(&["cancel", &id, "--reason", "r"]);
    assert_eq!(c.rc, 0, "precondition: cancel; out={} err={}", c.out, c.err);
    let f = fx.run(&["fail", &id, "--reason", "x"]);
    assert_ne!(f.rc, 0, "fail on cancelled must fail: out={}", f.out);
    assert_eq!(fx.status(&id), "cancelled");
}

#[test]
fn edit_status_cancelled_accepted_claimed_rejected() {
    let fx = Fx::new("edit");
    let id = fx.add("edit target");
    let bad = fx.run(&["edit", &id, "--status", "claimed"]);
    assert_ne!(
        bad.rc, 0,
        "edit --status claimed must be rejected: out={}",
        bad.out
    );
    assert_eq!(
        fx.status(&id),
        "pending",
        "claimed rejection must leave status unchanged"
    );

    let ok = fx.run(&["edit", &id, "--status", "cancelled"]);
    assert_eq!(
        ok.rc, 0,
        "edit --status cancelled: out={} err={}",
        ok.out, ok.err
    );
    assert_eq!(fx.status(&id), "cancelled");
}
