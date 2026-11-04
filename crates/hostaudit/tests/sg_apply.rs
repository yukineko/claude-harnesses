//! `aws apply` — the only mutating operation (CONTRACT.md §7).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;

const DB_FINDING: &str = "A1:ap-northeast-1:sg-0aaa:sgr-0db";
const SSH_FINDING: &str = "A1:ap-northeast-1:sg-0aaa:sgr-0ssh";
const MUTATING: [&str; 2] = [
    "revoke-security-group-ingress",
    "authorize-security-group-ingress",
];

/// Account with sg-0aaa = {sgr-0db 3306 world-open, sgr-0office 22 from office,
/// sgr-0https 443}; after a revoke it no longer lists sgr-0db.
fn db_open_env() -> Env {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_sg(&[
        sgr("sgr-0db", "tcp", 3306, 3306, Some("0.0.0.0/0"), None),
        sgr("sgr-0office", "tcp", 22, 22, Some("203.0.113.0/24"), None),
        sgr("sgr-0https", "tcp", 443, 443, Some("0.0.0.0/0"), None),
    ]);
    env.aws_sg_after(&[
        sgr("sgr-0office", "tcp", 22, 22, Some("203.0.113.0/24"), None),
        sgr("sgr-0https", "tcp", 443, 443, Some("0.0.0.0/0"), None),
    ]);
    env.aws_resp(
        "ec2.revoke-security-group-ingress.dryrun",
        fail(254, "An error occurred (DryRunOperation) when calling the RevokeSecurityGroupIngress operation: Request would have succeeded, but DryRun flag is set.\n"),
    );
    env.aws_resp(
        "ec2.revoke-security-group-ingress",
        ok("{\"Return\": true}"),
    );
    env.aws_resp(
        "ec2.authorize-security-group-ingress.dryrun",
        fail(254, "An error occurred (DryRunOperation) when calling the AuthorizeSecurityGroupIngress operation: Request would have succeeded, but DryRun flag is set.\n"),
    );
    env.aws_resp(
        "ec2.authorize-security-group-ingress",
        ok("{\"Return\": true, \"SecurityGroupRules\": [{\"SecurityGroupRuleId\": \"sgr-0new\"}]}"),
    );
    env
}

/// Only ssh 22 world-open, no other 22 rule → closing it removes all ssh access.
fn ssh_only_env() -> Env {
    let mut env = db_open_env();
    env.aws_sg(&[
        sgr("sgr-0ssh", "tcp", 22, 22, Some("0.0.0.0/0"), None),
        sgr("sgr-0https", "tcp", 443, 443, Some("0.0.0.0/0"), None),
    ]);
    env.aws_sg_after(&[
        sgr("sgr-0https", "tcp", 443, 443, Some("0.0.0.0/0"), None),
        sgr("sgr-0new", "tcp", 22, 22, Some("198.51.100.7/32"), None),
    ]);
    env
}

fn mutating_calls(env: &Env) -> Vec<Vec<String>> {
    env.aws_log()
        .into_iter()
        .filter(|a| a.len() > 1 && MUTATING.contains(&a[1].as_str()))
        .collect()
}

fn is_dry(a: &[String]) -> bool {
    a.iter().any(|x| x == "--dry-run")
}

#[test]
fn apply_without_confirmation_non_interactive_is_refused_before_any_mutation() {
    let env = db_open_env();
    let o = env.run(&["aws", "apply", "--finding", DB_FINDING]);
    assert_eq!(o.code, 3, "{}", o.dbg());
    assert!(
        o.stderr.contains("confirm"),
        "refusal must say confirmation is missing: {}",
        o.dbg()
    );
    assert!(mutating_calls(&env).is_empty(), "{:?}", env.aws_log());
}

#[test]
fn apply_in_autonomous_context_is_refused_even_with_confirm_flag() {
    let env = db_open_env();
    let o = env.run_with(
        &[
            "aws",
            "apply",
            "--finding",
            DB_FINDING,
            "--confirm",
            "sgr-0db",
        ],
        &[("CLAUDECODE", "1")],
    );
    assert_eq!(o.code, 3, "{}", o.dbg());
    assert!(mutating_calls(&env).is_empty(), "{:?}", env.aws_log());
}

#[test]
fn apply_with_mismatched_confirmation_is_refused() {
    let env = db_open_env();
    let o = env.run(&[
        "aws",
        "apply",
        "--finding",
        DB_FINDING,
        "--confirm",
        "sgr-0office",
    ]);
    assert_eq!(o.code, 3, "{}", o.dbg());
    assert!(mutating_calls(&env).is_empty(), "{:?}", env.aws_log());
}

#[test]
fn apply_unknown_or_non_sg_finding_is_refused() {
    for id in ["A1:ap-northeast-1:sg-0aaa:sgr-0nope", "A4:db-one"] {
        let env = db_open_env();
        let o = env.run(&["aws", "apply", "--finding", id, "--confirm", "sgr-0nope"]);
        assert_eq!(o.code, 3, "{id}: {}", o.dbg());
        assert!(mutating_calls(&env).is_empty(), "{id}: {:?}", env.aws_log());
    }
}

#[test]
fn confirmed_apply_dry_runs_then_revokes_only_that_rule_then_reaudits() {
    let env = db_open_env();
    let o = env.run(&[
        "aws",
        "apply",
        "--finding",
        DB_FINDING,
        "--confirm",
        "sgr-0db",
    ]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    assert!(
        o.stdout.contains("sgr-0db"),
        "diff must name the rule: {}",
        o.dbg()
    );

    let log = env.aws_log();
    let pos = |pred: &dyn Fn(&Vec<String>) -> bool| log.iter().position(pred);
    let dry = pos(&|a| a[1] == "revoke-security-group-ingress" && is_dry(a))
        .unwrap_or_else(|| panic!("no dry-run revoke: {log:?}"));
    let real = pos(&|a| a[1] == "revoke-security-group-ingress" && !is_dry(a))
        .unwrap_or_else(|| panic!("no real revoke: {log:?}"));
    assert!(dry < real, "dry-run must precede the revoke: {log:?}");
    let reaudit = log.iter().skip(real + 1).any(|a| {
        a[0] == "ec2"
            && (a[1] == "describe-security-group-rules" || a[1] == "describe-security-groups")
    });
    assert!(reaudit, "no describe after the revoke: {log:?}");

    let muts = mutating_calls(&env);
    let reals: Vec<_> = muts.iter().filter(|a| !is_dry(a)).collect();
    assert_eq!(reals.len(), 1, "exactly one real mutation: {muts:?}");
    let r = reals[0];
    assert_eq!(r[1], "revoke-security-group-ingress");
    let pair = |k: &str, v: &str| r.windows(2).any(|w| w[0] == k && w[1] == v);
    assert!(pair("--group-id", "sg-0aaa"), "{r:?}");
    assert!(pair("--security-group-rule-ids", "sgr-0db"), "{r:?}");
    assert!(pair("--region", "ap-northeast-1"), "{r:?}");
    for a in &muts {
        assert!(
            !a.iter()
                .any(|x| x.contains("sgr-0office") || x.contains("sgr-0https")),
            "apply touched another rule: {a:?}"
        );
    }
    let rows = env.reports();
    assert!(!rows.is_empty(), "apply must append a report row");
}

#[test]
fn dry_run_permission_failure_refuses_real_revoke() {
    let mut env = db_open_env();
    env.aws_resp(
        "ec2.revoke-security-group-ingress.dryrun",
        fail(254, "An error occurred (UnauthorizedOperation) when calling the RevokeSecurityGroupIngress operation: You are not authorized\n"),
    );
    let o = env.run(&[
        "aws",
        "apply",
        "--finding",
        DB_FINDING,
        "--confirm",
        "sgr-0db",
    ]);
    assert_eq!(o.code, 3, "{}", o.dbg());
    let muts = mutating_calls(&env);
    assert!(
        !muts.is_empty(),
        "dry-run must have been attempted: {:?}",
        env.aws_log()
    );
    assert!(
        muts.iter().all(|a| is_dry(a)),
        "real call after failed dry-run: {muts:?}"
    );
}

#[test]
fn reaudit_still_showing_rule_exits_nonzero() {
    let mut env = db_open_env();
    // The revoke "succeeds" but the rule is still listed afterwards.
    env.aws_sg_after(&[
        sgr("sgr-0db", "tcp", 3306, 3306, Some("0.0.0.0/0"), None),
        sgr("sgr-0office", "tcp", 22, 22, Some("203.0.113.0/24"), None),
    ]);
    let o = env.run(&[
        "aws",
        "apply",
        "--finding",
        DB_FINDING,
        "--confirm",
        "sgr-0db",
    ]);
    assert_eq!(o.code, 1, "{}", o.dbg());
    assert!(
        mutating_calls(&env).iter().any(|a| !is_dry(a)),
        "the revoke itself must have run: {:?}",
        env.aws_log()
    );
}

#[test]
fn closing_last_ssh_rule_without_replacement_is_refused_with_warning() {
    let env = ssh_only_env();
    let o = env.run(&[
        "aws",
        "apply",
        "--finding",
        SSH_FINDING,
        "--confirm",
        "sgr-0ssh",
    ]);
    assert_eq!(o.code, 3, "{}", o.dbg());
    assert!(
        o.stderr.contains("--replace-cidr"),
        "warning must require --replace-cidr: {}",
        o.dbg()
    );
    assert!(mutating_calls(&env).is_empty(), "{:?}", env.aws_log());
}

#[test]
fn closing_last_ssh_rule_with_replacement_authorizes_before_revoking() {
    let env = ssh_only_env();
    let o = env.run(&[
        "aws",
        "apply",
        "--finding",
        SSH_FINDING,
        "--confirm",
        "sgr-0ssh",
        "--replace-cidr",
        "198.51.100.7/32",
    ]);
    assert_eq!(o.code, 0, "{}", o.dbg());
    let muts = mutating_calls(&env);
    let reals: Vec<_> = muts.iter().filter(|a| !is_dry(a)).collect();
    assert_eq!(reals.len(), 2, "authorize + revoke: {muts:?}");
    assert_eq!(reals[0][1], "authorize-security-group-ingress", "{muts:?}");
    assert!(
        reals[0].iter().any(|x| x.contains("198.51.100.7/32")),
        "{:?}",
        reals[0]
    );
    assert_eq!(reals[1][1], "revoke-security-group-ingress", "{muts:?}");
    assert!(
        reals[1]
            .windows(2)
            .any(|w| w[0] == "--security-group-rule-ids" && w[1] == "sgr-0ssh"),
        "{:?}",
        reals[1]
    );
}
