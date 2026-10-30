// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Drift audit C4: skills/condukt/SKILL.md's F→P section contradicts
//! src/oracle.rs. Code behaviour (pinned by the behavioural test below, which
//! runs the real binary with `tdd` absent): a missing tdd yields
//! required:true, fallback:false, and `required` does NOT depend on
//! `reproduction_tests` (oracle.rs: "It is NO LONGER a gate switch").
//! Also: the interpreter agent's output schema cannot carry `kind`, so the
//! gate can never be reached from an interpreter-produced decomposition.

use std::path::Path;
use std::process::{Command, Stdio};

fn skill() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/condukt/SKILL.md"))
        .unwrap()
}

/// The `check-oracle` field bullets of the F→P section (from the bash fence
/// through the `valid_fp_oracle`... `transition` bullets).
fn bullets(s: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut on = false;
    for (i, l) in s.lines().enumerate() {
        if l.starts_with("- `required`:") {
            on = true;
        }
        if on && l.starts_with("**実際の強制は") {
            break;
        }
        if on {
            out.push((i + 1, l.to_string()));
        }
    }
    out
}

#[test]
fn skill_required_bullet_does_not_gate_on_reproduction_tests() {
    let s = skill();
    let b = bullets(&s);
    let req = b
        .iter()
        .find(|(_, l)| l.starts_with("- `required`:"))
        .expect("required bullet");
    assert!(
        !req.1.contains("reproduction_tests"),
        "SKILL.md:{} says `required` depends on reproduction_tests, but src/oracle.rs \
         (\"NO LONGER a gate switch\") makes a fix/feature task required regardless: {}",
        req.0,
        req.1
    );
}

#[test]
fn skill_does_not_say_missing_tdd_degrades_to_fallback_true() {
    let s = skill();
    let joined: Vec<(usize, String)> = bullets(&s);
    // the fallback bullet spans several continuation lines
    let start = joined
        .iter()
        .position(|(_, l)| l.starts_with("- `fallback`:"))
        .expect("fallback bullet");
    let end = joined[start + 1..]
        .iter()
        .position(|(_, l)| l.starts_with("- "))
        .map_or(joined.len(), |p| start + 1 + p);
    let text: String = joined[start..end]
        .iter()
        .map(|(_, l)| l.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let line = joined[start].0;
    let says_missing_tdd = text.contains("PATH に無い") || text.contains("spawn 失敗");
    let says_degrades = text.contains("fallback:true") && text.contains("degrade");
    assert!(
        !(says_missing_tdd && says_degrades),
        "SKILL.md:{line}.. says a missing tdd / spawn failure degrades to fallback:true, but \
         src/oracle.rs gives required:true, fallback:false, which `state set --status verified` \
         REJECTS: {text}"
    );
}

/// Behaviour pin (should PASS today): with no `tdd` anywhere, a fix task gets
/// required:true / fallback:false even without reproduction_tests.
#[test]
fn binary_missing_tdd_blocks_regardless_of_reproduction_tests() {
    let base = std::env::temp_dir().join(format!("condukt-c4-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    let home = base.join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let git = |args: &[&str]| {
        let st = harness_core::git_env::scrub(&mut Command::new("git"))
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(st.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    let empty_path = base.join("emptybin");
    std::fs::create_dir_all(&empty_path).unwrap();
    let run = |args: &[&str]| {
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_condukt")))
            .args(args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("PATH", &empty_path)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let dec = repo.join("dec.json");
    std::fs::write(
        &dec,
        r#"{"goal":"g","tasks":[{"id":"t1","title":"x","kind":"fix"}]}"#,
    )
    .unwrap();
    let o = run(&[
        "state",
        "init",
        "--run",
        "r1",
        "--file",
        dec.to_str().unwrap(),
    ]);
    assert!(
        o.status.success(),
        "init: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = run(&["state", "check-oracle", "--run", "r1", "--task", "t1"]);
    let v: serde_json::Value =
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("json: {e}: {:?}", o));
    assert_eq!(v["required"], true, "{v}");
    assert_eq!(v["fallback"], false, "{v}");
}

#[test]
fn interpreter_output_schema_can_carry_kind() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("agents/condukt-interpreter.md");
    let t = std::fs::read_to_string(p).unwrap();
    // SKILL Phase 1 declares `kind` (fix|feature gates the F→P oracle) and the
    // interpreter is told "スキーマに無いキーは足さない".
    assert!(
        t.contains("\"kind\""),
        "agents/condukt-interpreter.md output schema has no \"kind\" key while its rules \
         forbid adding keys not in the schema, so an interpreter decomposition can never \
         carry kind:fix|feature and the F→P gate is unreachable from it"
    );
}
