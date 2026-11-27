//! Backlog c512596e follow-up: a variable whose value is rebound by a builtin
//! or `=~` must never resolve to the earlier literal (never Allow), at the top
//! level and inside a function body with `local`.
use blastguard::detect;
use blastguard::model::Decision;
use blastguard::scope::SafeRoots;
use serde_json::json;

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
fn rebound_variables_are_never_allow() {
    let l = "f(){ local P=/home/yuki/proj/ok.log; ";
    let t = "P=/home/yuki/proj/ok.log; ";
    let w = "echo hi > \"$P\"";
    let cases = [
        format!("{l}printf -vP %s /etc/hosts; {w}; }}; f"),
        format!("{l}read -aP <<< /etc/hosts; {w}; }}; f"),
        format!("{l}print -v P /etc/hosts; {w}; }}; f"),
        format!("{l}zformat -f P /etc/hosts; {w}; }}; f"),
        format!("{t}printf -vP %s /etc/hosts; {w}"),
        format!("{t}read -aP <<< /etc/hosts; {w}"),
        "f(){ local MATCH=/home/yuki/proj/ok.log; [[ /etc/hosts =~ .* ]]; echo hi > \"$MATCH\"; }; f"
            .to_string(),
        "MATCH=/home/yuki/proj/ok.log; [[ /etc/hosts =~ .* ]]; echo hi > \"$MATCH\"".to_string(),
    ];
    for c in &cases {
        assert!(
            !matches!(scoped(c), Decision::Allow),
            "rebound variable resolved to Allow: {c}"
        );
    }
}

#[test]
fn unrebound_controls_stay_allow() {
    for c in [
        "f(){ local P=/home/yuki/proj/ok.log; echo hi > \"$P\"; }; f",
        "P=/home/yuki/proj/ok.log; [[ x =~ y ]]; echo hi > \"$P\"",
    ] {
        assert!(
            matches!(scoped(c), Decision::Allow),
            "control not Allow: {c}"
        );
    }
}
