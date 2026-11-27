#![allow(clippy::expect_used)]
//! RED (main session from verifier candidates, 2026-11-28, tip 2bb59e526 /
//! 0.2.965): spellings of alias / function definitions that the whole-word
//! `exemption_disabled` check misses, so a listed non-binder's bare `P`
//! operand is exempt while the real shell rebinds P. Measured in main
//! (bash 3.2.57 / zsh 5.9, `P=/ok; echo P <<< /b; printf "%s\n" "$P"`):
//!   `:;shopt -s expand_aliases;alias echo=read<NL>...`        -> /b
//!   `\shopt -s expand_aliases; \alias echo=read<NL>...`       -> /b
//!   `true&&shopt -s expand_aliases&&alias echo=read<NL>...`   -> /b
//!   `:;shopt -s expand_aliases;"alias" echo=read<NL>...`      -> /b
//!   zsh `functions+=(echo "read \$1"); ...`                   -> /b
//! Not measured (kept fail-closed: an unknown spelling must not be Allow):
//! `sh''opt` / `al''ias`, `$'shopt'` / `$'alias'`.
use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

const OK: &str = "/home/yuki/proj/ok.log";
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

#[test]
fn exemption_disabled_bypass_spellings_are_never_allow() {
    let tail = format!("P={OK}; echo P <<< /etc/hosts; echo hi > \"$P\"");
    let heads = [
        ":;shopt -s expand_aliases;alias echo=read\n",
        "\\shopt -s expand_aliases; \\alias echo=read\n",
        "true&&shopt -s expand_aliases&&alias echo=read\n",
        ":;shopt -s expand_aliases;\"alias\" echo=read\n",
        "sh''opt -s expand_aliases; al''ias echo=read\n",
        "$'shopt' -s expand_aliases; $'alias' echo=read\n",
        "functions+=(echo 'read $1'); ",
    ];
    let mut bad = Vec::new();
    for h in heads {
        let c = format!("{h}{tail}");
        let d = scoped(&c);
        if matches!(d, Decision::Allow) || !d.clone().hardened().is_deny() {
            bad.push(format!("{d:?}: {c:?}"));
        }
    }
    let c = format!("P={OK}; functions+=(cat 'read $1'); cat P <<< /etc/hosts; echo hi > \"$P\"");
    let d = scoped(&c);
    if matches!(d, Decision::Allow) || !d.clone().hardened().is_deny() {
        bad.push(format!("{d:?}: {c:?}"));
    }
    assert!(
        bad.is_empty(),
        "{} still Allow / not Deny as subagent:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
