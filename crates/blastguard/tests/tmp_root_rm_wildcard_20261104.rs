#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED regression for the user ruling 2026-11-02 (verbatim):
//! 「そもそも/tmp以下はrm -frを削除していい。worktreeなどと同じあつかいにして」.
//!
//! A recursive `rm` of a LITERAL operand strictly inside `/tmp` was already
//! Allow. The gap was a WILDCARD operand below `/tmp`
//! (`rm -rf /tmp/<dir>/name-*`, `rm -rf /tmp/<head>-*`), which came back
//! Deny/Ask until blastguard 0.2.262 (backlog 486fb7de, #1531).
//!
//! Fixture: deliberately NOT `support/neutral_base.rs` (on Linux it panics with
//! "no neutral fixture parent … /tmp is equal to or under the fixed temp root",
//! backlog 3b3c22a5). HOME and the project dir live under
//! `CARGO_TARGET_TMPDIR` (asserted NOT under `/tmp`), and the victims live in a
//! unique `/tmp/<uniq>` directory. Every case runs the real hook binary through
//! the same `Fx::run`, so a fixture problem shows up in the literal-operand
//! control too, not only in the wildcard case.
//!
//! The decision is read explicitly: no stdout = allow, otherwise
//! `hookSpecificOutput.permissionDecision`. Anything unparseable is reported
//! as a distinct value so it never reads as allow.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

struct Fx {
    base: PathBuf,
    home: PathBuf,
    proj: PathBuf,
    uniq: String,
    tmp: PathBuf,
}

impl Fx {
    fn new(name: &str) -> Fx {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let uniq = format!("bgtmpwild-{}-{n}-{name}", std::process::id());
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("tmp_root_rm_wildcard")
            .join(&uniq);
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::fs::create_dir_all(base.join("src/proj")).unwrap();
        let home = base.join("home").canonicalize().unwrap();
        let proj = base.join("src/proj").canonicalize().unwrap();
        assert!(
            !home.starts_with("/tmp") && !proj.starts_with("/tmp"),
            "fixture HOME/proj must not be under /tmp: {home:?} {proj:?}"
        );
        let tmp = PathBuf::from(format!("/tmp/{uniq}"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub/a")).unwrap();
        std::fs::create_dir_all(tmp.join("sub/b")).unwrap();
        std::fs::create_dir_all(tmp.join("sub/name-1")).unwrap();
        std::fs::create_dir_all(tmp.join("sub/name-2")).unwrap();
        // The host's canonical spelling of /tmp must be one of the fixed temp
        // roots blastguard recognises (`/tmp` on Linux, `/private/tmp` on
        // macOS where /tmp is a symlink to it). Any other host cannot run this
        // test meaningfully and says so instead of passing.
        let tmp_real = PathBuf::from("/tmp")
            .canonicalize()
            .unwrap_or_else(|e| panic!("HOST CANNOT RUN THIS TEST: cannot canonicalize /tmp: {e}"));
        assert!(
            tmp_real == std::path::Path::new("/tmp")
                || tmp_real == std::path::Path::new("/private/tmp"),
            "HOST CANNOT RUN THIS TEST: /tmp resolves to {tmp_real:?}, which is \
             not a fixed temp root blastguard recognises (/tmp, /private/tmp)"
        );
        // Real path of /tmp/<uniq> must be strictly under the real /tmp root
        // (no symlink below /tmp redirecting the victims elsewhere).
        let uniq_real = tmp.canonicalize().unwrap();
        assert!(
            uniq_real == tmp_real.join(&uniq),
            "/tmp/<uniq> resolves to {uniq_real:?}, not directly under the real \
             /tmp root {tmp_real:?}"
        );
        Fx {
            base,
            home,
            proj,
            uniq,
            tmp,
        }
    }

    fn t(&self) -> String {
        self.tmp.display().to_string()
    }

    /// Returns "allow" | "deny" | "ask" | "UNPARSEABLE:<stdout>".
    fn decide(&self, command: &str) -> String {
        let payload = serde_json::json!({
            "session_id": format!("tmp-root-rm-wild-{}", self.uniq),
            "cwd": self.proj.display().to_string(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
        .to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &self.proj)
            .env("BLASTGUARD_APPROVALS_DIR", self.base.join("store"))
            .env("HOME", &self.home)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env_remove("BLASTGUARD_ASK")
            .env_remove("TMPDIR")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .current_dir(&self.proj);
        let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
        assert_eq!(
            out.status.code(),
            Some(0),
            "hook must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let d = if s.is_empty() {
            "allow".to_string()
        } else {
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(v) => match v["hookSpecificOutput"]["permissionDecision"].as_str() {
                    Some(d) => d.to_string(),
                    None => format!("UNPARSEABLE:{s}"),
                },
                Err(_) => format!("UNPARSEABLE:{s}"),
            }
        };
        println!("[{command}] => {d} | raw: {s}");
        d
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[track_caller]
fn assert_decision(case: &str, got: &str, want: &str) {
    assert_eq!(got, want, "{case}: expected {want}, got {got}");
}

/// CONTROL (passes today): a literal operand strictly inside /tmp is Allow
/// in this exact fixture. If this fails, the fixture — not the rule — is broken.
#[test]
fn control_literal_operand_inside_tmp_is_allow() {
    let f = Fx::new("lit");
    let d = f.decide(&format!("rm -rf {}/sub/a", f.t()));
    assert_decision("rm -rf /tmp/<uniq>/sub/a", &d, "allow");
}

/// RED: wildcard in the last component, literal parent strictly inside /tmp.
#[test]
fn wildcard_last_component_inside_tmp_dir_is_allow() {
    let f = Fx::new("insub");
    let lit = f.decide(&format!("rm -rf {}/sub/b", f.t()));
    assert_decision("same-fixture literal control", &lit, "allow");
    let d = f.decide(&format!("rm -rf {}/sub/name-*", f.t()));
    assert_decision("rm -rf /tmp/<uniq>/sub/name-*", &d, "allow");
}

/// RED: wildcard directly under /tmp with a literal name head.
#[test]
fn wildcard_with_literal_head_directly_under_tmp_is_allow() {
    let f = Fx::new("head");
    let lit = f.decide(&format!("rm -rf {}/sub/b", f.t()));
    assert_decision("same-fixture literal control", &lit, "allow");
    let d = f.decide(&format!("rm -rf /tmp/{}-*", &f.uniq[..f.uniq.len() - 5]));
    assert_decision("rm -rf /tmp/<head>-*", &d, "allow");
}

/// CONTROL (must stay non-Allow): a bare wildcard over all of /tmp.
#[test]
fn control_bare_tmp_star_is_not_allow() {
    let f = Fx::new("bare");
    let d = f.decide("rm -rf /tmp/*");
    assert_ne!(d, "allow", "rm -rf /tmp/* must NOT be Allow");
}

/// CONTROL (must stay non-Allow): wildcard in a non-last component.
#[test]
fn control_glob_in_non_last_component_is_not_allow() {
    let f = Fx::new("mid");
    let d = f.decide("rm -rf /tmp/*/x");
    assert_ne!(d, "allow", "rm -rf /tmp/*/x must NOT be Allow");
}

// ---------------------------------------------------------------------------
// User ruling 2026-11-05 (#1531): directly under the /tmp root, a wildcard
// operand is Allow only when the literal head before the first `*` is at
// least 4 characters. Deeper under /tmp (`/tmp/<dir>/name-*`) any head length
// stays Allow. The same rule applies to the `/private/tmp` spelling.
//
// Heads are prefixes of HEAD4. They are chosen NOT to prefix any protected
// path component: a head like `b` or `s` is already non-Allow for an
// unrelated reason (`b*` may expand to `bin`, `s*` to `settings.local.json`,
// both protected), which would make a short-head case pass for the wrong
// reason. Each short head is therefore paired with a same-fixture control:
// the SAME head one level below `/tmp/<uniq>` must be Allow, so a non-Allow
// directly under the root can only come from the head-length rule. blastguard
// never expands a glob, so the decision does not depend on what exists in
// /tmp; nothing is created there for these heads.
// ---------------------------------------------------------------------------

/// A 4-char literal head; its 1..=3 prefixes are the short heads.
const HEAD4: &str = "zqxv";

/// True on hosts (macOS) where /tmp is a symlink to /private/tmp.
fn host_tmp_is_private_tmp() -> bool {
    PathBuf::from("/tmp").canonicalize().unwrap() == std::path::Path::new("/private/tmp")
}

/// The tmp-root spellings to exercise on this host.
fn tmp_roots() -> Vec<&'static str> {
    let mut roots = vec!["/tmp"];
    if host_tmp_is_private_tmp() {
        roots.push("/private/tmp");
    }
    roots
}

fn assert_short_heads_not_allow(f: &Fx, root: &str) {
    // Same-fixture control: the 4-char head is Allow directly under the root.
    let ctl = format!("rm -rf {root}/{HEAD4}*");
    let d = f.decide(&ctl);
    assert_decision(&format!("control {ctl}"), &d, "allow");
    let mut allowed = Vec::new();
    for n in 1..=3 {
        let head = &HEAD4[..n];
        // Same-fixture control: this head one level deeper is Allow, so the
        // head itself is not refused for a protected-name reason.
        let deeper = format!("rm -rf {root}/{}/{head}*", f.uniq);
        let d = f.decide(&deeper);
        assert_decision(&format!("control {deeper}"), &d, "allow");
        let cmd = format!("rm -rf {root}/{head}*");
        if f.decide(&cmd) == "allow" {
            allowed.push(cmd);
        }
    }
    assert!(
        allowed.is_empty(),
        "a head of 1-3 chars directly under the tmp root must NOT be Allow \
         (ruling 2026-11-05), but these were: {allowed:?}"
    );
}

/// RED (ruling 2026-11-05): `/tmp/z*`, `/tmp/zq*`, `/tmp/zqx*` are not Allow.
#[test]
fn short_head_1_to_3_directly_under_tmp_is_not_allow() {
    let f = Fx::new("short");
    assert_short_heads_not_allow(&f, "/tmp");
}

/// RED (ruling 2026-11-05): the same rule spelled `/private/tmp`. Only
/// meaningful where /tmp is /private/tmp; elsewhere it prints that it did not
/// run and returns.
#[test]
fn short_head_1_to_3_directly_under_private_tmp_is_not_allow() {
    let f = Fx::new("pshort");
    if !host_tmp_is_private_tmp() {
        println!("NOT RUN ON THIS HOST: /tmp is not /private/tmp");
        return;
    }
    assert_short_heads_not_allow(&f, "/private/tmp");
}

/// RED (ruling 2026-11-05, the ruling's own examples verbatim): `/tmp/c*`,
/// `/tmp/cl*`, `/tmp/cla*` are not Allow while `/tmp/clau*` is, under every
/// tmp-root spelling this host has.
#[test]
fn ruling_examples_c_cl_cla_directly_under_tmp_are_not_allow() {
    let f = Fx::new("clau");
    let mut allowed = Vec::new();
    for root in tmp_roots() {
        let ctl = format!("rm -rf {root}/clau*");
        let d = f.decide(&ctl);
        assert_decision(&format!("control {ctl}"), &d, "allow");
        for head in ["c", "cl", "cla"] {
            let cmd = format!("rm -rf {root}/{head}*");
            if f.decide(&cmd) == "allow" {
                allowed.push(cmd);
            }
        }
    }
    assert!(
        allowed.is_empty(),
        "the ruling's short-head examples must NOT be Allow, but these were: {allowed:?}"
    );
}

/// CONTROL: heads of exactly 4 and of more than 4 characters directly under
/// the tmp root (every spelling this host has) are Allow, including the
/// `<head>-*` shape with a 4+ char head.
#[test]
fn control_head_of_4_or_more_directly_under_tmp_is_allow() {
    let f = Fx::new("long");
    for root in tmp_roots() {
        for pattern in [
            format!("{HEAD4}*"),
            format!("{HEAD4}k*"),
            format!("{HEAD4}-*"),
            format!("{}*", f.uniq),
        ] {
            let cmd = format!("rm -rf {root}/{pattern}");
            let d = f.decide(&cmd);
            assert_decision(&cmd, &d, "allow");
        }
    }
}

/// CONTROL: one and two levels below the tmp root, a 1-char head is Allow
/// (`/tmp/<dir>/n*`, `/tmp/<dir>/sub/n*`), for every spelling this host has.
#[test]
fn control_short_head_one_level_deeper_is_allow() {
    let f = Fx::new("deep");
    for root in tmp_roots() {
        for rel in ["n*", "sub/n*", "z*"] {
            let cmd = format!("rm -rf {root}/{}/{rel}", f.uniq);
            let d = f.decide(&cmd);
            assert_decision(&cmd, &d, "allow");
        }
    }
}
