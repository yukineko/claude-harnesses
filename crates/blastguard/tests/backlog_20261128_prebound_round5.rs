#![allow(clippy::expect_used)]
//! RED (verifier, backlog f3301770, round on adf18bd7b / 0.2.978).
//! `measured_*`: real-shell rebinds (bash 3.2.57 / zsh 5.9, literal -c,
//! </dev/null, 2026-11-28; P=/ok -> /b or 1 means rebound). With `cd /etc`
//! first, a rebind to `1` writes /etc/1.
//!   bash: X="P=1"; P=/ok; a[X]=1                       -> 1
//!   bash: X="P=1"; P=/ok; a=([X]=1)                    -> 1
//!   bash: X="P=1"; a=(q); P=/ok; unset "a[X]"          -> 1
//!   bash: X="P=1"; P=/ok; read "a[X]" <<< v            -> 1
//!   bash: X="P=1"; P=/ok; declare "a[X]=1"             -> 1
//!   zsh:  X="P=1"; P=/ok; a[X]=1                       -> 1
//!   zsh:  X="P=1"; P=/ok; read "a[X]" <<< v            -> 1
//!   zsh:  X="P=1"; P=/ok; printf "%d" X                -> 1
//!   zsh:  X="P=1"; P=/ok; set -- a b c; shift X        -> 1
//!   zsh:  X="P=1"; P=/ok; repeat X do :; done          -> 1
//!   zsh:  float Y / typeset -F Y / typeset -E Y; P=/ok; Y="P=1" -> 1
//!   bash/zsh: trap -- "P=/b" DEBUG; P=/ok; true        -> /b / /b
//!   bash/zsh: trap "for P in /b; do :; done" DEBUG; P=/ok; true -> /b / /b
//!   bash: trap ": \$[P=1]" DEBUG; P=/ok; true          -> 1
//! Controls measured NOT rebinding (must stay as they are, not asserted here):
//!   bash/zsh `[ X -eq 1 ]`, `test X -eq 1` -> /ok.
//! `weaker_than_base_*`: Deny on base ace252997, Ask in main on adf18bd7b.
//!   Not run in a real shell (relay bgr-9efcf12a5713 blocked the harmless analog).
//! `measured_fp_*`: Allow on base ace252997, Deny on adf18bd7b, measured
//!   not rebinding: for ((i=0;i<3;i++)) -> /ok; declare -i n=0; n+=1 -> /ok;
//!   cleanup(){ echo c; }; trap cleanup EXIT INT TERM -> /ok (bash and zsh).

use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

const OK: &str = "/home/yuki/proj/ok.log";
const W: &str = "echo hi > \"$P\"";

fn identity(p: &str) -> Option<String> {
    Some(p.to_string())
}

fn scoped(cmd: &str) -> Decision {
    let roots = SafeRoots::new(
        Some("/home/yuki/proj"),
        Some("/home/yuki/proj"),
        Some("/home/yuki"),
        None,
        Some(identity),
    );
    detect::detect_scoped("Bash", Some(&json!({ "command": cmd })), &roots)
}

fn assert_never_allow(cases: &[String]) {
    let mut bad = Vec::new();
    for c in cases {
        let d = scoped(c);
        let h = d.clone().hardened();
        if matches!(d, Decision::Allow) || !h.is_deny() {
            bad.push(format!("main={d:?} sub={h:?} :: {c:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "rebind resolved as the confined literal:\n{}",
        bad.join("\n")
    );
}

#[test]
#[ignore = "RED(48bf8ea5): array subscript arithmetic rebinds still resolve \"$P\" to the stale confined value"]
fn measured_array_subscript_rebinds_are_never_allow() {
    assert_never_allow(&[
        format!("cd /etc; X='P=1'; P={OK}; a[X]=1; {W}"),
        format!("cd /etc; X='P=1'; P={OK}; a=([X]=1); {W}"),
        format!("cd /etc; X='P=1'; a=(q); P={OK}; unset 'a[X]'; {W}"),
        format!("cd /etc; X='P=1'; P={OK}; read 'a[X]' <<< v; {W}"),
        format!("cd /etc; X='P=1'; P={OK}; declare 'a[X]=1'; {W}"),
    ]);
}

#[test]
#[ignore = "RED(48bf8ea5): zsh numeric-argument / float rebinds still resolve \"$P\" to the stale confined value"]
fn measured_zsh_numeric_argument_and_float_rebinds_are_never_allow() {
    assert_never_allow(&[
        format!("cd /etc; X='P=1'; P={OK}; printf '%d' X; {W}"),
        format!("cd /etc; f(){{ local P={OK}; X='P=1'; printf '%d' X; {W}; }}; f"),
        format!("cd /etc; X='P=1'; P={OK}; shift X; {W}"),
        format!("cd /etc; X='P=1'; P={OK}; repeat X do :; done; {W}"),
        format!("cd /etc; float Y; P={OK}; Y='P=1'; {W}"),
        format!("cd /etc; typeset -F Y; P={OK}; Y='P=1'; {W}"),
        format!("cd /etc; typeset -E Y; P={OK}; Y='P=1'; {W}"),
    ]);
}

#[test]
#[ignore = "RED(48bf8ea5): DEBUG trap bodies that rebind still resolve \"$P\" to the stale confined value"]
fn measured_trap_body_rebinds_are_never_allow() {
    assert_never_allow(&[
        format!("cd /etc; trap -- 'P=/etc/hosts' DEBUG; P={OK}; true; {W}"),
        format!("cd /etc; trap 'for P in /etc/hosts; do :; done' DEBUG; P={OK}; true; {W}"),
        format!("cd /etc; trap ': $[P=1]' DEBUG; P={OK}; true; {W}"),
    ]);
}

#[test]
fn weaker_than_base_destructive_heads_stay_deny_in_main() {
    let mut bad = Vec::new();
    for c in [
        "X=/bin/r; ${X}m -rf /etc",
        "X=/bin/r; \"$X\"m -rf /etc",
        "E=eval; $E 'rm -rf /etc'",
        "E=eval; $E rm -rf /etc",
    ] {
        let d = scoped(c);
        if !d.is_deny() {
            bad.push(format!("main={d:?} :: {c:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "Deny on base, weaker now:\n{}",
        bad.join("\n")
    );
}

#[test]
fn measured_fp_common_arithmetic_and_cleanup_traps_stay_allow() {
    let mut bad = Vec::new();
    for c in [
        format!("P={OK}; for ((i=0;i<3;i++)); do echo $i; done; {W}"),
        format!("P={OK}; declare -i n=0; n+=1; {W}"),
        format!("declare -i n=0; n+=1; P={OK}; {W}"),
        format!("P={OK}; cleanup(){{ rm -f /home/yuki/proj/tmp; }}; trap cleanup EXIT; {W}"),
        format!(
            "cleanup(){{ rm -f /home/yuki/proj/tmp; }}; trap cleanup EXIT INT TERM; P={OK}; {W}"
        ),
    ] {
        let d = scoped(&c);
        if !matches!(d, Decision::Allow) {
            bad.push(format!("main={d:?} :: {c:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "Allow on base, not Allow now:\n{}",
        bad.join("\n")
    );
}
