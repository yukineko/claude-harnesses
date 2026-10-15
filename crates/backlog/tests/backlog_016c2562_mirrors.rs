//! backlog 016c2562 — mirror sites of the `wontfix` selection rule.
//!
//! `next` / `next --claim` no longer hand out a task tagged `wontfix` (a human
//! "do not implement" ruling). Two other places answer "what work is queued
//! here?" and must agree with that:
//!
//! 1. The SessionStart hook's "## Backlog — pending tasks for this project"
//!    listing tells the agent to implement every task it lists, so a wontfix
//!    task must not appear in it (a normal pending task must still appear —
//!    the non-vacuous half).
//! 2. The 5ba13c3e divergence guard refuses (non-zero exit) when the resolved
//!    store has no queued work while the legacy `~/.backlog` store holds
//!    queued work for the same project. A resolved store holding ONLY a
//!    wontfix task has no queued work either (`next` hands nothing out), so it
//!    must be refused exactly like the empty-store control.
//!
//! Written by an independent test author (not the implementer). Every fixture
//! is a fresh temp git repo with an isolated HOME; the real `~/.backlog` and
//! this repo's `.backlog` are never touched.

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-016c2562m-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // The binary resolves identity through canonical paths; macOS temp is a symlink.
    std::fs::canonicalize(&dir).unwrap()
}

/// A fresh git repo (the store follows the checkout) plus an isolated HOME.
struct Fx {
    home: PathBuf,
    repo: PathBuf,
}

fn fx(tag: &str) -> Fx {
    let root = unique(tag);
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    common::linked_checkout(&repo);
    // `add` lands `pending` only with a REPRODUCED repro test from a committed
    // script (otherwise `add` is refused). Commit one.
    std::fs::create_dir_all(repo.join("tests")).unwrap();
    std::fs::write(
        repo.join("tests/repro_yes.sh"),
        "echo 'bug present'; exit 1\n",
    )
    .unwrap();
    for args in [
        &["add", "tests/repro_yes.sh"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t.t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "repro",
        ],
    ] {
        let ok = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?} failed: fixture is void");
    }
    Fx { home, repo }
}

fn run_stdin(f: &Fx, args: &[&str], payload: &str) -> (i32, String, String) {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_backlog"))
        .args(args)
        .env("HOME", &f.home)
        .env("PATH", common::path_with_condukt_shim())
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&f.repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run(f: &Fx, args: &[&str]) -> (i32, String, String) {
    run_stdin(f, args, "")
}

/// Add a pending task to the resolved (repo) store and return its id.
fn add(f: &Fx, title: &str, extra: &[&str]) -> String {
    let proj = f.repo.to_str().unwrap();
    let mut args = vec![
        "add",
        "--title",
        title,
        "--project",
        proj,
        "--repro-test",
        "bash tests/repro_yes.sh",
    ];
    args.extend_from_slice(extra);
    let (rc, out, err) = run(f, &args);
    assert_eq!(rc, 0, "precondition: add must succeed; out={out} err={err}");
    out.lines()
        .find_map(|l| l.strip_prefix("added: "))
        .unwrap_or_else(|| panic!("no 'added:' line in {out:?}"))
        .trim()
        .to_string()
}

fn add_wontfix(f: &Fx) -> String {
    add(
        f,
        "ruled out work",
        &["--priority", "p1", "--tag", "wontfix"],
    )
}

/// Seed the LEGACY store `$HOME/.backlog/tasks.toml` with one pending,
/// untagged task labelled with this checkout's project identity (a linked
/// worktree's identity is its main tree `<name>.main`, see
/// `common::linked_checkout`).
fn seed_legacy_pending(f: &Fx, id: &str) {
    let name = f.repo.file_name().unwrap().to_string_lossy().into_owned();
    let project = std::fs::canonicalize(f.repo.with_file_name(format!("{name}.main"))).unwrap();
    let dir = f.home.join(".backlog");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tasks.toml"),
        format!(
            "[[task]]\nid = \"{id}\"\ntitle = \"stranded legacy work\"\nproject = \"{}\"\n\
             tags = []\nstatus = \"pending\"\nnotes = \"\"\ncreated_at = 1000\nupdated_at = 1000\n\n",
            project.display()
        ),
    )
    .unwrap();
}

const PENDING_HEADING: &str = "## Backlog \u{2014} pending tasks for this project";

/// Run the SessionStart hook and return its injected `additionalContext`.
fn session_start_context(f: &Fx) -> String {
    let payload = format!(
        r#"{{"session_id":"s","cwd":"{}","hook_event_name":"SessionStart"}}"#,
        f.repo.display()
    );
    let (rc, out, err) = run_stdin(f, &["session-start"], &payload);
    assert_eq!(rc, 0, "the hook must exit 0; out={out:?} err={err:?}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| {
        panic!("session-start printed no JSON context ({e}); out={out:?} err={err:?}")
    });
    v.get("additionalContext")
        .and_then(|x| x.as_str())
        .unwrap_or_else(|| panic!("no string additionalContext in {out:?}"))
        .to_string()
}

/// The "pending tasks for this project" section: from its heading up to the
/// next `---` rule or `## ` heading (or the end).
fn pending_section(ctx: &str) -> Option<&str> {
    let start = ctx.find(PENDING_HEADING)?;
    let body = &ctx[start + PENDING_HEADING.len()..];
    let end = [body.find("\n---"), body.find("\n## ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(body.len());
    Some(&body[..end])
}

/// (1) The SessionStart pending-task listing omits the wontfix task while
/// still listing the normal pending task from the same store.
#[test]
fn session_start_pending_listing_omits_wontfix_but_lists_normal_task() {
    let f = fx("hook-mixed");
    let wf = add_wontfix(&f);
    let normal = add(&f, "ordinary work", &["--priority", "p3"]);

    let ctx = session_start_context(&f);
    let section = pending_section(&ctx).unwrap_or_else(|| {
        panic!("no '{PENDING_HEADING}' section although {normal} is pending; ctx={ctx:?}")
    });
    assert!(
        section.contains(&normal),
        "the normal pending task {normal} must still be listed; section={section:?}"
    );
    assert!(
        !section.contains(&wf),
        "the SessionStart pending-task listing presents wontfix-tagged task {wf} \
         as work to implement; section={section:?}"
    );
}

/// (2-control) The divergence guard is reachable in this fixture: an empty
/// resolved store + legacy work for the same project is refused (rc != 0)
/// by both `next` and `next --claim`.
#[test]
fn control_empty_resolved_store_with_legacy_work_is_refused() {
    let f = fx("div-control");
    seed_legacy_pending(&f, "legacy01");
    for cmd in [&["next"][..], &["next", "--claim"][..]] {
        let (rc, out, err) = run(&f, cmd);
        assert_ne!(
            rc, 0,
            "control: `{cmd:?}` must be refused when the resolved store is empty and the \
             legacy store holds queued work; out={out:?} err={err:?}"
        );
    }
}

/// (2) A resolved store holding ONLY a wontfix task has no queued work, so
/// with legacy work for the same project `next` / `next --claim` must be
/// refused exactly like the empty-store control — not answer
/// `no pending tasks` with rc 0.
#[test]
fn wontfix_only_resolved_store_with_legacy_work_is_refused_like_empty() {
    let f = fx("div-wontfix");
    let wf = add_wontfix(&f);
    seed_legacy_pending(&f, "legacy02");
    for cmd in [&["next"][..], &["next", "--claim"][..]] {
        let (rc, out, err) = run(&f, cmd);
        assert!(
            !out.contains(&wf),
            "precondition (016c2562 selection fix): `{cmd:?}` must not hand out {wf}; out={out:?}"
        );
        assert_ne!(
            rc, 0,
            "`{cmd:?}` answered rc 0 although the resolved store holds only wontfix task {wf} \
             (no queued work) and the legacy store holds queued work for this project; \
             the divergence guard counted the wontfix row as queued. out={out:?} err={err:?}"
        );
    }
}
