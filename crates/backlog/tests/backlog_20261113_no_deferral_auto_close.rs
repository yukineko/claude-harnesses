#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! No-deferral contract (user rulings 2026-11-13: 「保留はありえない
//! 実証されないならclose,実証されたらopen」 / 「起票しますか?というのは
//! もう聞かないこと。実証されたら起票、そうでないなら破棄。それは自動で
//! 実行していくこと。それを仕組みとしていれて」).
//!
//! One re-run of a row's stored repro at HEAD yields:
//!   REPRODUCED       script exit 1 / cargo `test result: FAILED`
//!   NOT-REPRODUCED   exit 0 with >= 1 test shown to have run
//!   NOT-DEMONSTRATED script exit 2 (environment unavailable), 126/127, or
//!                    exit 0 with zero tests run; closes like not-reproduced
//!   UNDETERMINABLE   infrastructure (timeout, killed by signal, cargo
//!                    compile failure); never auto-closed
//! Everything is observed through the real binary (exit codes, stdout ids,
//! stored status / closure JSON, the `gh` stub's recorded argv, store bytes).

mod common;

use common::Fixture;
use std::path::PathBuf;
use std::process::Command;

const TOKEN: &str = "TOK-last-line-7f3a91";
/// One flip script; its verdict follows the state file named by $1.
const FLIP: &str = "bash tests/repro_flip.sh src/state.txt";
const FLIP2: &str = "bash tests/repro_flip.sh src/state2.txt";

fn setup_flip(f: &Fixture) {
    f.write("src/state.txt", "broken\n");
    f.write("src/state2.txt", "broken\n");
    f.write_script(
        "tests/repro_flip.sh",
        &format!(
            "s=\"$(cat \"$1\")\"\n\
             case \"$s\" in\n\
               fixed) echo 'no bug'; echo '{TOKEN}'; exit 0;;\n\
               env) echo 'no v9fs dir'; exit 2;;\n\
               sleep) exec sleep 30;;\n\
               kill) kill -KILL $$;;\n\
               *) echo 'bug present'; exit 1;;\n\
             esac"
        ),
    );
    f.commit_paths(
        &["src/state.txt", "src/state2.txt", "tests/repro_flip.sh"],
        "flip repro",
    );
}

fn add_cmd(f: &Fixture, title: &str, cmd: &str) -> String {
    let p = f.project();
    let o = f.run(&[
        "add",
        "--title",
        title,
        "--project",
        &p,
        "--repro-test",
        cmd,
    ]);
    assert_eq!(
        o.code,
        0,
        "control: add must file the RED row: {}",
        o.both()
    );
    common::parse_added(&o.stdout)
}

fn set_state(f: &Fixture, file: &str, v: &str) {
    f.write(file, &format!("{v}\n"));
    f.commit_paths(&[file], &format!("{file}={v}"));
}

/// `gh` stub recording every argv; `issue create` answers issue 701, 702, ...
fn gh_stub(f: &Fixture) -> PathBuf {
    let log = f.home.join("gh.log");
    let body = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\n\
         if [ \"$1\" = issue ] && [ \"$2\" = create ]; then\n\
         \u{20} n=$(grep -c 'issue create' {log})\n\
         \u{20} echo \"https://github.com/o/r/issues/$((700 + n))\"\n\
         fi\nexit 0\n",
        log = log.display()
    );
    f.write_exec_shim("gh", &body);
    // github.com origin whose pushes land in a local bare repo: `add` now
    // merges the row onto the primary's branch and pushes it before filing
    // the issue (f6795f52), so the primary needs a born branch and a
    // reachable push target.
    common::fixture_github_origin(&f.repo);
    log
}

fn gh_log(log: &PathBuf) -> String {
    std::fs::read_to_string(log).unwrap_or_default()
}

fn closure(f: &Fixture, id: &str) -> serde_json::Value {
    f.row(id)["closure"].clone()
}

fn assert_discarded(f: &Fixture, id: &str, needles: &[&str]) {
    assert_eq!(f.status(id), "cancelled", "{id} must be auto-cancelled");
    let c = closure(f, id);
    assert_eq!(c["reason"], "discard", "closure of {id}: {c}");
    let why = c["discard_reason"].as_str().unwrap_or("");
    for n in needles {
        assert!(
            why.contains(n),
            "discard_reason of {id} must contain {n:?}: {c}"
        );
    }
}

fn assert_untouched_pending(f: &Fixture, id: &str) {
    assert_eq!(f.status(id), "pending", "{id} must stay pending");
    assert!(
        f.row(id)["closure"].is_null(),
        "{id} must carry no closure: {}",
        f.row(id)
    );
}

/// Replace the status of row `id` in the live store (planting a legacy row).
fn set_store_status(f: &Fixture, id: &str, from: &str, to: &str) {
    let p = f.tasks_path();
    let s = std::fs::read_to_string(&p).unwrap();
    let mut out = Vec::new();
    let mut hit = false;
    for blk in s.split("[[task]]") {
        if blk.contains(&format!("id = \"{id}\"")) {
            hit = true;
            out.push(blk.replacen(
                &format!("status = \"{from}\""),
                &format!("status = \"{to}\""),
                1,
            ));
        } else {
            out.push(blk.to_string());
        }
    }
    assert!(hit, "row {id} not in store");
    std::fs::write(&p, out.join("[[task]]")).unwrap();
}

// ---------------------------------------------------------------- (a)

#[test]
fn a_defer_is_refused_and_writes_nothing() {
    let f = Fixture::new("nd-a");
    let id = f.add("would be deferred");
    let before = f.store_bytes();
    let d = f.run(&["defer", &id, "--ruling", "x"]);
    assert_ne!(d.code, 0, "defer must be refused: {}", d.both());
    assert!(
        !d.stdout.contains("ruled-deferred:"),
        "defer must not report a deferral: {}",
        d.both()
    );
    assert_eq!(f.status(&id), "pending");
    assert_eq!(
        f.store_bytes(),
        before,
        "a refused defer must write nothing"
    );
}

// ---------------------------------------------------------------- (b)

#[test]
fn b_claim_auto_cancels_green_top_candidate_and_closes_issue() {
    let f = Fixture::new("nd-b");
    let log = gh_stub(&f);
    setup_flip(&f);
    let green = add_cmd(&f, "went green", FLIP);
    let red = f.add("still red");
    assert!(
        gh_log(&log).contains("issue create"),
        "setup: add must mirror an issue: {:?}",
        gh_log(&log)
    );
    set_state(&f, "src/state.txt", "fixed");

    let c = f.run(&["next", "--claim"]);
    assert_eq!(c.code, 0, "claim must succeed: {}", c.both());
    assert!(
        c.stdout.contains(&red),
        "still-RED row handed out: {}",
        c.both()
    );
    assert!(
        !c.stdout.contains(&green),
        "green row handed out: {}",
        c.both()
    );
    assert_discarded(&f, &green, &["exit 0", TOKEN]);
    let g = gh_log(&log);
    assert!(
        g.contains("issue close 701"),
        "the auto-cancel must close the mirrored issue 701; gh log: {g:?}"
    );
}

// ---------------------------------------------------------------- (c)

#[test]
fn c_claim_auto_cancels_env_unavailable_exit_2_candidate() {
    let f = Fixture::new("nd-c");
    setup_flip(&f);
    let envrow = add_cmd(&f, "env gone", FLIP);
    let red = f.add("still red");
    set_state(&f, "src/state.txt", "env");

    let c = f.run(&["next", "--claim"]);
    assert_eq!(c.code, 0, "claim must succeed: {}", c.both());
    assert!(
        c.stdout.contains(&red),
        "still-RED row handed out: {}",
        c.both()
    );
    assert!(
        !c.stdout.contains(&envrow),
        "exit-2 row handed out: {}",
        c.both()
    );
    assert_discarded(&f, &envrow, &["exit 2", "no v9fs dir"]);
}

// ---------------------------------------------------------------- (d)

#[test]
fn d_control_still_red_candidate_is_handed_out_not_cancelled() {
    let f = Fixture::new("nd-d");
    setup_flip(&f);
    let id = add_cmd(&f, "still red", FLIP);
    let c = f.run(&["next", "--claim"]);
    assert_eq!(c.code, 0, "{}", c.both());
    assert!(c.stdout.contains(&id), "{}", c.both());
    assert_ne!(f.status(&id), "cancelled");
}

// ---------------------------------------------------------------- (e)

fn undeterminable(tag: &str, state: &str, env: &[(&str, &str)]) {
    let f = Fixture::new(tag);
    let log = gh_stub(&f);
    setup_flip(&f);
    let id = add_cmd(&f, "infra undeterminable", FLIP);
    set_state(&f, "src/state.txt", state);
    let c = f.run_env(&["next", "--claim"], env);
    assert_ne!(
        c.code,
        0,
        "undeterminable claim must be refused: {}",
        c.both()
    );
    assert!(!c.stdout.contains(&id), "handed out: {}", c.both());
    assert_untouched_pending(&f, &id);
    assert!(
        !gh_log(&log).contains("issue close"),
        "no issue may be closed: {:?}",
        gh_log(&log)
    );
}

#[test]
fn e_control_timeout_is_undeterminable_never_closed() {
    undeterminable("nd-e1", "sleep", &[("BACKLOG_TEST_TIMEOUT_SECS", "1")]);
}

#[test]
fn e_control_signal_kill_is_undeterminable_never_closed() {
    undeterminable("nd-e2", "kill", &[]);
}

// ---------------------------------------------------------------- (f)

#[test]
fn f_add_with_exit_2_repro_writes_nothing() {
    let f = Fixture::new("nd-f");
    f.write_script("tests/repro_env.sh", "echo 'no v9fs dir'; exit 2");
    f.commit_paths(&["tests/repro_env.sh"], "env repro");
    let before = f.store_bytes();
    let p = f.project();
    let o = f.run(&[
        "add",
        "--title",
        "env gone at filing",
        "--project",
        &p,
        "--repro-test",
        "bash tests/repro_env.sh",
    ]);
    assert_ne!(o.code, 0, "add must be refused: {}", o.both());
    assert_eq!(f.store_bytes(), before, "nothing may be written");
    assert!(f.list_all().is_empty(), "no row may exist");
}

// ---------------------------------------------------------------- (g)

const CARGO_CMD: &str = "cargo test -p x";

fn cargo_row(f: &Fixture) -> String {
    f.write("marker", "broken");
    f.commit_paths(&["marker"], "marker broken");
    add_cmd(f, "cargo row", CARGO_CMD)
}

#[test]
fn g_cargo_zero_tests_at_head_is_auto_cancelled() {
    let f = Fixture::new("nd-g1");
    let id = cargo_row(&f);
    set_state(&f, "marker", "empty");
    let c = f.run(&["next", "--claim"]);
    assert!(!c.stdout.contains(&id), "handed out: {}", c.both());
    assert_discarded(&f, &id, &["ZERO tests"]);
}

#[test]
fn g_control_cargo_compile_failure_at_head_refuses_claim() {
    let f = Fixture::new("nd-g2");
    let id = cargo_row(&f);
    set_state(&f, "marker", "nobuild");
    let before = f.store_bytes();
    let c = f.run(&["next", "--claim"]);
    assert_ne!(c.code, 0, "{}", c.both());
    assert!(!c.stdout.contains(&id), "{}", c.both());
    assert_untouched_pending(&f, &id);
    assert_eq!(f.store_bytes(), before);
}

// ---------------------------------------------------------------- (h)

#[test]
fn h_recheck_cancels_green_and_keeps_red() {
    let f = Fixture::new("nd-h1");
    setup_flip(&f);
    let green = add_cmd(&f, "went green", FLIP);
    let red = add_cmd(&f, "still red", FLIP2);
    set_state(&f, "src/state.txt", "fixed");
    let o = f.run(&["recheck"]);
    assert_eq!(o.code, 0, "recheck must succeed: {}", o.both());
    assert_discarded(&f, &green, &["exit 0", TOKEN]);
    assert_untouched_pending(&f, &red);
}

#[test]
fn h_recheck_resolves_legacy_ruled_deferred_rows() {
    let f = Fixture::new("nd-h2");
    setup_flip(&f);
    let still = add_cmd(&f, "legacy still reproduces", FLIP);
    let gone = add_cmd(&f, "legacy now passes", FLIP2);
    set_store_status(&f, &still, "pending", "ruled-deferred");
    set_store_status(&f, &gone, "pending", "ruled-deferred");
    assert_eq!(f.status(&still), "ruled-deferred", "setup: planted row");
    assert_eq!(f.status(&gone), "ruled-deferred", "setup: planted row");
    set_state(&f, "src/state2.txt", "fixed");
    let o = f.run(&["recheck"]);
    assert_eq!(o.code, 0, "recheck must succeed: {}", o.both());
    assert_eq!(f.status(&still), "pending", "reproduced legacy row reopens");
    assert_discarded(&f, &gone, &["exit 0", TOKEN]);
}

#[test]
fn h_recheck_leaves_undeterminable_row_untouched_and_fails() {
    let f = Fixture::new("nd-h3");
    setup_flip(&f);
    // Infrastructure-undeterminable: the repro is killed by SIGKILL.
    let id = add_cmd(&f, "killed by signal", FLIP);
    let green = add_cmd(&f, "went green", FLIP2);
    set_state(&f, "src/state.txt", "kill");
    set_state(&f, "src/state2.txt", "fixed");
    let before = f.row(&id);
    let o = f.run(&["recheck"]);
    assert_ne!(o.code, 0, "undeterminable row => non-zero: {}", o.both());
    // Non-vacuity: a real recheck ran, named the row and a word of the cause.
    let err = o.stderr.to_lowercase();
    assert!(
        err.contains(&id)
            && (err.contains("signal") || err.contains("kill") || err.contains("undetermin")),
        "recheck must name the row {id} and the cause on stderr: {}",
        o.both()
    );
    assert_untouched_pending(&f, &id);
    assert_eq!(
        f.row(&id),
        before,
        "the undeterminable row must be unchanged"
    );
    // The other row is still processed.
    assert_discarded(&f, &green, &["exit 0", TOKEN]);
}

#[test]
fn h_recheck_id_touches_only_that_row() {
    let f = Fixture::new("nd-h4");
    setup_flip(&f);
    let a = add_cmd(&f, "a green", FLIP);
    let b = add_cmd(&f, "b green", FLIP2);
    set_state(&f, "src/state.txt", "fixed");
    set_state(&f, "src/state2.txt", "fixed");
    let o = f.run(&["recheck", "--id", &a]);
    assert_eq!(o.code, 0, "{}", o.both());
    assert_discarded(&f, &a, &["exit 0"]);
    assert_untouched_pending(&f, &b);
}

// ---------------------------------------------------------------- (i)

#[test]
fn i_claim_from_primary_tree_with_green_repro_writes_nothing() {
    let f = Fixture::new("nd-i");
    setup_flip(&f);
    let id = add_cmd(&f, "went green", FLIP);
    set_state(&f, "src/state.txt", "fixed");

    // The fixture's linked worktree hangs off `<repo>.main`; bring that
    // PRIMARY tree to the same commit and give it a copy of the store.
    let primary = f.repo.with_file_name(format!(
        "{}.main",
        f.repo.file_name().unwrap().to_string_lossy()
    ));
    let g = harness_core::git_env::scrub(&mut Command::new("git"))
        .args(["reset", "-q", "--hard", "main"])
        .current_dir(&primary)
        .env("HOME", &f.home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(g.status.success(), "{}", String::from_utf8_lossy(&g.stderr));
    std::fs::create_dir_all(primary.join(".backlog")).unwrap();
    std::fs::copy(f.tasks_path(), primary.join(".backlog/tasks.toml")).unwrap();
    let before = std::fs::read(primary.join(".backlog/tasks.toml")).unwrap();
    // Non-vacuity: the primary really holds the row and the flipped state.
    assert!(
        String::from_utf8_lossy(&before).contains(&id),
        "setup: primary store must hold the row"
    );
    assert_eq!(
        std::fs::read_to_string(primary.join("src/state.txt")).unwrap(),
        "fixed\n",
        "setup: primary HEAD must carry the flipped state"
    );

    let path = format!(
        "{}:{}:{}",
        f.shim.display(),
        common::condukt_shim_dir().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_backlog"));
    harness_core::git_env::scrub(&mut cmd);
    let out = cmd
        .args(["next", "--claim"])
        .env("HOME", &f.home)
        .env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("BACKLOG_TEST_TIMEOUT_SECS")
        .current_dir(&primary)
        .output()
        .unwrap();
    let both = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "primary tree must refuse: {both}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains(&id),
        "{both}"
    );
    assert_eq!(
        std::fs::read(primary.join(".backlog/tasks.toml")).unwrap(),
        before,
        "primary tasks.toml must be unchanged: {both}"
    );
}
