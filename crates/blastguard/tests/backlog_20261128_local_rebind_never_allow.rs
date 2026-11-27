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
        // `{P}>file` assigns an fd number to P (zsh 5.9: `[11]`), so the real
        // write is /etc/11 after the `cd`.
        format!("{l}cd /etc; echo x {{P}}>/dev/null; {w}; }}; f"),
        format!("{l}echo x {{P}}>/dev/null; cd /etc; {w}; }}; f"),
        format!("{l}cd /etc; : {{P}}</dev/null; {w}; }}; f"),
        format!("{l}cd /etc; true {{P}}>&-; {w}; }}; f"),
        format!("{l}cd /etc; cat /dev/null {{P}}<>/dev/null; {w}; }}; f"),
        // bash parse.y accepts `{name}` when `legal_identifier(name) ||
        // valid_array_reference(name)`: `{P[0]}` may assign the fd to P.
        format!("{l}cd /etc; echo x {{P[0]}}>/dev/null; {w}; }}; f"),
        format!("{l}cd /etc; echo x {{P[1]}}<&-; {w}; }}; f"),
        format!("{l}cd /etc; exec {{P[0]}}>/dev/null; {w}; }}; f"),
    ];
    for c in &cases {
        let d = scoped(c);
        assert!(
            !matches!(d, Decision::Allow),
            "rebound variable resolved to Allow: {c}"
        );
        assert!(
            d.clone().hardened().is_deny(),
            "no human to answer must harden to Deny: {c} -> {d:?}"
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
