//! Reproduction tests for open backlog items audited in shard s03-backlog.
//!
//! Every test here asserts the property the ticket says is MISSING, so each
//! one FAILS on the code as measured (RED observed before `#[ignore]` was
//! added). They are `#[ignore]`d so the suite stays green; remove the
//! `#[ignore]` when the corresponding defect is fixed. Run them with
//! `cargo test -p backlog --test backlog_s03_audit -- --ignored`.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "backlog-s03-{}-{}-{}",
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
    let ok = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .expect("git runs")
        .success();
    assert!(ok, "git init failed: fixture is void");
    // Close-evidence fixture: `add` lands `pending` only with a REPRODUCED
    // repro test from a committed script (otherwise `unconfirmed`, which
    // `next` never hands out — that would make every claim-based test here
    // vacuous). Commit one so `add` below files pending rows as before.
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

fn run(f: &Fx, args: &[&str], path_prefix: Option<&Path>) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backlog"));
    cmd.args(args)
        .env("HOME", &f.home)
        .current_dir(&f.repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match path_prefix {
        Some(p) => {
            // The caller's shim comes first; no deterministic condukt is
            // added behind it, so the shim fully decides the claim check.
            let old = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{}", p.display(), old));
        }
        None => {
            cmd.env("PATH", common::path_with_condukt_shim());
        }
    }
    let out = cmd.output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Add a task and return its id (precondition: add must succeed).
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
    let (rc, out, err) = run(f, &args, None);
    assert_eq!(rc, 0, "precondition: add must succeed; out={out} err={err}");
    out.lines()
        .find_map(|l| l.strip_prefix("added: "))
        .unwrap_or_else(|| panic!("no 'added:' line in {out:?}"))
        .trim()
        .to_string()
}

/// backlog 420f1eec: `add`'s duplicate guard shells out to `condukt state
/// is-claimed`; ANY failure (here exit 3 = "registry unreadable") is mapped to
/// "not claimed", so an undetermined claim check is indistinguishable from a
/// clean one. Expected: refuse, or at least say the check could not be made.
#[test]
fn add_does_not_treat_an_undetermined_condukt_claim_check_as_not_claimed() {
    let f = fx("420f1eec");
    let shim = f.home.join("shim");
    std::fs::create_dir_all(&shim).unwrap();
    let sh = shim.join("condukt");
    std::fs::write(&sh, "#!/bin/sh\necho 'registry unreadable' >&2\nexit 3\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let proj = f.repo.to_str().unwrap();
    let (rc, out, err) = run(
        &f,
        &["add", "--title", "dup guard probe", "--project", proj],
        Some(&shim),
    );
    let said_undetermined = {
        let all = format!("{out}\n{err}").to_lowercase();
        all.contains("could not determine")
            || all.contains("undetermined")
            || all.contains("claim check")
            || all.contains("unverified")
    };
    assert!(
        rc != 0 || said_undetermined,
        "add exited 0 and printed no hint that the claim check failed (condukt exit 3): \
         'cannot determine' was rendered as 'not claimed'. out={out:?} err={err:?}"
    );
}

/// backlog c57a600c: `edit` cannot change priority/weight, so a mis-filed
/// priority can never be corrected.
#[test]
#[ignore = "backlog c57a600c: open defect, remove ignore when fixed"]
fn edit_accepts_priority() {
    let f = fx("c57a600c");
    let id = add(&f, "prio probe", &["--priority", "p1"]);
    let (rc, out, err) = run(&f, &["edit", &id, "--priority", "p3"], None);
    assert_eq!(
        rc, 0,
        "backlog edit --priority is not accepted. out={out:?} err={err:?}"
    );
}

/// backlog 016c2562: a task tagged `wontfix` (human ruling: do not implement)
/// is still handed out by `next --claim`; tags are ignored by selection.
#[test]
#[ignore = "backlog 016c2562: open defect, remove ignore when fixed"]
fn next_claim_skips_wontfix_tagged_tasks() {
    let f = fx("016c2562");
    let wf = add(
        &f,
        "ruled out work",
        &["--priority", "p1", "--tag", "wontfix"],
    );
    let (rc, out, err) = run(&f, &["next", "--claim"], None);
    assert!(
        !(rc == 0 && out.contains(&wf)),
        "next --claim handed out wontfix-tagged task {wf}. out={out:?} err={err:?}"
    );
}

/// backlog 0dafa254: `claimed` is a real (derived) status but the status
/// vocabulary lacks it, so a correct `--status claimed` filter warns "unknown".
#[test]
fn list_status_claimed_is_a_recognised_status() {
    let f = fx("0dafa254");
    let _ = add(&f, "claim me", &[]);
    let (rc, _out, err) = run(&f, &["next", "--claim"], None);
    assert_eq!(
        rc, 0,
        "precondition: next --claim must succeed; err={err:?}"
    );
    let (_rc, out, err) = run(&f, &["list", "--status", "claimed"], None);
    assert!(
        out.contains("claimed"),
        "precondition: list --status claimed must show the claimed row; out={out:?}"
    );
    assert!(
        !err.to_lowercase().contains("unknown status"),
        "list --status claimed warned 'unknown status' for a status the binary itself writes: {err:?}"
    );
}

/// backlog 13f4e5eb: a TOML parse failure reports only the path, not where.
#[test]
#[ignore = "backlog 13f4e5eb: open defect, remove ignore when fixed"]
fn parse_failure_reports_line_and_reason() {
    let f = fx("13f4e5eb");
    let _ = add(&f, "seed", &[]);
    let store = f.repo.join(".backlog").join("tasks.toml");
    let mut body = std::fs::read_to_string(&store).unwrap();
    body.push_str("\nbroken = [\n");
    std::fs::write(&store, body).unwrap();
    let (rc, out, err) = run(&f, &["list"], None);
    assert_ne!(
        rc, 0,
        "precondition: a corrupt store must not list cleanly; out={out:?}"
    );
    let all = format!("{out}\n{err}").to_lowercase();
    assert!(
        all.contains("line") || all.contains("column"),
        "parse error names no line/column/reason, only: {err:?}"
    );
}

/// backlog 787922b3: `done` for an id that is absent from THIS store says only
/// "task not found", with no hint which store was searched, so a store
/// mismatch cannot be told from a mistyped id.
#[test]
#[ignore = "backlog 787922b3: open defect, remove ignore when fixed"]
fn done_not_found_names_the_store_it_searched() {
    let f = fx("787922b3");
    let _ = add(&f, "seed", &[]);
    let (rc, out, err) = run(&f, &["done", "deadbeef"], None);
    assert_ne!(rc, 0, "precondition: unknown id must fail; out={out:?}");
    let store = f.repo.join(".backlog");
    let all = format!("{out}\n{err}");
    assert!(
        all.contains(store.to_str().unwrap()) || all.to_lowercase().contains("store"),
        "'task not found' does not say which store was searched: {err:?}"
    );
}

/// backlog d8d25af9 / f6c5164e: no terminal state meaning "decided not to do
/// it": `fail` always re-queues via defer_until and `edit --status cancelled`
/// is refused. Either fix named in the ticket satisfies this: `edit` accepts a
/// non-completing terminal status, or `edit`/`fail` grows a defer/no-retry flag.
#[test]
#[ignore = "backlog d8d25af9: open defect, remove ignore when fixed"]
fn a_permanent_wontfix_close_exists() {
    let f = fx("d8d25af9");
    let id = add(&f, "will not do", &[]);
    let (rc, _o, _e) = run(&f, &["edit", &id, "--status", "cancelled"], None);
    let (_rc2, help, _e2) = run(&f, &["edit", "--help"], None);
    let (_rc3, fhelp, _e3) = run(&f, &["fail", "--help"], None);
    let has_flag = |h: &str| {
        let h = h.to_lowercase();
        h.contains("defer") || h.contains("no-retry") || h.contains("wontfix")
    };
    assert!(
        rc == 0 || has_flag(&help) || has_flag(&fhelp),
        "no way to close a task permanently without claiming it done: edit --status cancelled \
         rc={rc}; edit --help / fail --help mention no defer/no-retry/wontfix"
    );
}
