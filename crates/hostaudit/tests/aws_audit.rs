//! `aws` read-only audit A1–A7 (CONTRACT.md §6, §8).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;
use common::*;
use serde_json::{json, Value};

const READ_ONLY: [&str; 13] = [
    "sts get-caller-identity",
    "ec2 describe-regions",
    "ec2 describe-security-groups",
    "ec2 describe-security-group-rules",
    "ec2 describe-instances",
    "ec2 describe-network-interfaces",
    "s3api list-buckets",
    "s3api get-public-access-block",
    "s3api get-bucket-policy-status",
    "rds describe-db-instances",
    "iam get-account-summary",
    "cloudtrail describe-trails",
    "cloudtrail get-trail-status",
];
const A_RULES: [&str; 7] = ["A1", "A2", "A3", "A4", "A5", "A6", "A7"];

fn run_aws(env: &Env) -> (Out, Vec<Value>) {
    let o = env.run(&["aws", "--json"]);
    let fs = o.findings();
    (o, fs)
}

fn violations(fs: &[Value], rule: &str) -> Vec<Value> {
    by_rule(fs, rule)
        .into_iter()
        .filter(|f| f["verdict"] == "violation")
        .collect()
}

#[test]
fn clean_account_is_clean_for_every_rule() {
    let mut env = Env::new();
    env.aws_clean_account();
    let (o, fs) = run_aws(&env);
    assert_eq!(o.code, 0, "{}", o.dbg());
    for r in A_RULES {
        assert!(
            has(&fs, r, "clean"),
            "{r} must have a clean finding: {fs:?}"
        );
        assert!(violations(&fs, r).is_empty(), "{r}: {fs:?}");
    }
}

#[test]
fn sts_failure_makes_every_aws_rule_undetermined_with_expired_login() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_resp(
        "sts.get-caller-identity",
        fail(
            255,
            "Error when retrieving token from sso: Token has expired and refresh failed\n",
        ),
    );
    let (o, fs) = run_aws(&env);
    assert_eq!(o.code, 1, "{}", o.dbg());
    for r in A_RULES {
        let rs = by_rule(&fs, r);
        assert!(!rs.is_empty(), "{r} missing: {fs:?}");
        for f in rs {
            assert_eq!(f["verdict"], "undetermined", "{r}: {f}");
            assert!(
                f["reason"]
                    .as_str()
                    .unwrap_or("")
                    .contains("ログインが切れている"),
                "{r}: reason must say the login expired: {f}"
            );
        }
    }
    let log = env.aws_log();
    assert_eq!(log.len(), 1, "no aws call may follow a failed sts: {log:?}");
    assert_eq!(&log[0][..2], ["sts", "get-caller-identity"]);
}

#[test]
fn aws_audit_issues_only_read_only_calls() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_sg(&[sgr("sgr-0ssh", "tcp", 22, 22, Some("0.0.0.0/0"), None)]);
    env.aws_instances("optional");
    let (o, _) = run_aws(&env);
    assert_eq!(o.code, 1, "{}", o.dbg());
    let log = env.aws_log();
    assert!(log.len() >= 8, "expected the full read sweep: {log:?}");
    for argv in &log {
        let call = format!("{} {}", argv[0], argv[1]);
        assert!(
            READ_ONLY.contains(&call.as_str()),
            "non-read-only aws call: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--output" && w[1] == "json"),
            "--output json missing: {argv:?}"
        );
    }
    assert_eq!(
        &log[0][..2],
        ["sts", "get-caller-identity"],
        "sts must be first"
    );
}

#[test]
fn a1_world_open_ssh_rdp_db_ports_are_violations() {
    for (rule, what) in [
        (
            sgr("sgr-0v4ssh", "tcp", 22, 22, Some("0.0.0.0/0"), None),
            "v4 22",
        ),
        (
            sgr("sgr-0v6rdp", "tcp", 3389, 3389, None, Some("::/0")),
            "v6 3389",
        ),
        (
            sgr("sgr-0range", "tcp", 3300, 3310, Some("0.0.0.0/0"), None),
            "range over 3306",
        ),
        (
            sgr("sgr-0redis", "tcp", 6379, 6379, Some("0.0.0.0/0"), None),
            "6379",
        ),
    ] {
        let mut env = Env::new();
        env.aws_clean_account();
        let id = rule["SecurityGroupRuleId"].as_str().unwrap().to_string();
        env.aws_sg(&[rule]);
        let (o, fs) = run_aws(&env);
        assert_eq!(o.code, 1, "{what}: {}", o.dbg());
        let v = violations(&fs, "A1");
        assert_eq!(v.len(), 1, "{what}: {fs:?}");
        assert_eq!(v[0]["sgr_id"], id.as_str(), "{what}: {}", v[0]);
        assert_eq!(
            v[0]["id"],
            format!("A1:ap-northeast-1:sg-0aaa:{id}"),
            "{}",
            v[0]
        );
    }
}

#[test]
fn a1_restricted_or_non_sensitive_rules_are_clean() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_sg(&[
        sgr("sgr-0https", "tcp", 443, 443, Some("0.0.0.0/0"), None),
        sgr("sgr-0office", "tcp", 22, 22, Some("203.0.113.0/24"), None),
        sgr("sgr-0db", "tcp", 5432, 5432, Some("10.0.0.0/8"), None),
    ]);
    let (o, fs) = run_aws(&env);
    assert_eq!(o.code, 0, "{}", o.dbg());
    assert!(violations(&fs, "A1").is_empty(), "{fs:?}");
    assert!(has(&fs, "A1", "clean"));
}

#[test]
fn a2_all_protocols_from_anywhere_is_violation() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_sg(&[sgr("sgr-0all", "-1", -1, -1, Some("0.0.0.0/0"), None)]);
    let (o, fs) = run_aws(&env);
    assert_eq!(o.code, 1, "{}", o.dbg());
    let v = violations(&fs, "A2");
    assert_eq!(v.len(), 1, "{fs:?}");
    assert_eq!(v[0]["sgr_id"], "sgr-0all");
    assert_eq!(v[0]["id"], "A2:ap-northeast-1:sg-0aaa:sgr-0all");
}

#[test]
fn a1_a2_findings_carry_sg_location_and_cli() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_sg(&[
        sgr("sgr-0v4ssh", "tcp", 22, 22, Some("0.0.0.0/0"), None),
        sgr("sgr-0all", "-1", -1, -1, Some("0.0.0.0/0"), None),
    ]);
    let (_, fs) = run_aws(&env);
    for (rule, sgr_id) in [("A1", "sgr-0v4ssh"), ("A2", "sgr-0all")] {
        let v = violations(&fs, rule);
        assert_eq!(v.len(), 1, "{rule}: {fs:?}");
        let f = &v[0];
        assert_eq!(f["region"], "ap-northeast-1", "{f}");
        assert_eq!(f["sg_id"], "sg-0aaa", "{f}");
        assert_eq!(f["sg_name"], "web-sg", "{f}");
        assert_eq!(f["sgr_id"], sgr_id, "{f}");
        assert_eq!(f["cidr"], "0.0.0.0/0", "{f}");
        let att = f["attached"]
            .as_array()
            .unwrap_or_else(|| panic!("attached: {f}"));
        assert!(
            att.iter().any(|a| a["instance_id"] == "i-0123web"
                && a["name"] == "web"
                && a["eni"] == "eni-0web"),
            "{f}"
        );
        let cp = f["console_path"].as_str().unwrap_or("");
        assert!(
            cp.contains("sg-0aaa") && cp.contains("インバウンドルールを編集"),
            "{f}"
        );
        let cli: Vec<&str> = f["cli"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c.as_str())
            .collect();
        let want = format!(
            "aws ec2 revoke-security-group-ingress --region ap-northeast-1 --group-id sg-0aaa --security-group-rule-ids {sgr_id}"
        );
        assert!(
            cli.iter().any(|c| c.contains(&want)),
            "{rule}: want {want} in {cli:?}"
        );
        assert!(f["fix"].as_str().is_some_and(|s| !s.is_empty()), "{f}");
    }
    // Printed, never executed.
    assert!(env
        .aws_log()
        .iter()
        .all(|a| a[1] != "revoke-security-group-ingress"));
}

#[test]
fn a3_public_access_block_not_all_true_is_violation() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_json(
        "s3api.get-public-access-block",
        json!({"PublicAccessBlockConfiguration": {"BlockPublicAcls": true, "IgnorePublicAcls": true,
               "BlockPublicPolicy": true, "RestrictPublicBuckets": false}}),
    );
    let (_, fs) = run_aws(&env);
    let v = violations(&fs, "A3");
    assert_eq!(v.len(), 1, "{fs:?}");
    assert_eq!(v[0]["target"], "bucket-one");
}

#[test]
fn a3_missing_public_access_block_is_violation() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_resp(
        "s3api.get-public-access-block",
        fail(254, "An error occurred (NoSuchPublicAccessBlockConfiguration) when calling the GetPublicAccessBlock operation: The public access block configuration was not found\n"),
    );
    let (_, fs) = run_aws(&env);
    assert_eq!(violations(&fs, "A3").len(), 1, "{fs:?}");
}

#[test]
fn a3_public_policy_status_is_violation() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_json(
        "s3api.get-bucket-policy-status",
        json!({"PolicyStatus": {"IsPublic": true}}),
    );
    let (_, fs) = run_aws(&env);
    assert_eq!(violations(&fs, "A3").len(), 1, "{fs:?}");
}

#[test]
fn a3_no_bucket_policy_is_not_public() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_resp(
        "s3api.get-bucket-policy-status",
        fail(254, "An error occurred (NoSuchBucketPolicy) when calling the GetBucketPolicyStatus operation: The bucket policy does not exist\n"),
    );
    let (o, fs) = run_aws(&env);
    assert_eq!(o.code, 0, "{}", o.dbg());
    assert!(has(&fs, "A3", "clean"), "{fs:?}");
}

#[test]
fn a4_publicly_accessible_rds_is_violation() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_json(
        "rds.describe-db-instances",
        json!({"DBInstances": [{"DBInstanceIdentifier": "db-one", "PubliclyAccessible": true}]}),
    );
    let (_, fs) = run_aws(&env);
    let v = violations(&fs, "A4");
    assert_eq!(v.len(), 1, "{fs:?}");
    assert_eq!(v[0]["target"], "db-one");
}

#[test]
fn a4_api_failure_is_undetermined() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_resp(
        "rds.describe-db-instances",
        fail(
            254,
            "An error occurred (AccessDenied) when calling the DescribeDBInstances operation\n",
        ),
    );
    let (o, fs) = run_aws(&env);
    assert_eq!(o.code, 1, "{}", o.dbg());
    assert!(has(&fs, "A4", "undetermined"), "{fs:?}");
    assert!(!has(&fs, "A4", "clean"), "{fs:?}");
}

#[test]
fn a5_root_without_mfa_or_with_keys_is_violation() {
    for (mfa, keys) in [(0, 0), (1, 1)] {
        let mut env = Env::new();
        env.aws_clean_account();
        env.aws_json(
            "iam.get-account-summary",
            json!({"SummaryMap": {"AccountMFAEnabled": mfa, "AccountAccessKeysPresent": keys}}),
        );
        let (_, fs) = run_aws(&env);
        assert_eq!(
            violations(&fs, "A5").len(),
            1,
            "mfa={mfa} keys={keys}: {fs:?}"
        );
    }
}

#[test]
fn a6_missing_or_non_logging_multi_region_trail_is_violation() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_json("cloudtrail.describe-trails", json!({"trailList": []}));
    let (_, fs) = run_aws(&env);
    assert_eq!(violations(&fs, "A6").len(), 1, "no trails: {fs:?}");

    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_json("cloudtrail.get-trail-status", json!({"IsLogging": false}));
    let (_, fs) = run_aws(&env);
    assert_eq!(violations(&fs, "A6").len(), 1, "not logging: {fs:?}");

    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_json(
        "cloudtrail.describe-trails",
        json!({"trailList": [{"Name": "single", "TrailARN": "arn:aws:cloudtrail:ap-northeast-1:123456789012:trail/single",
               "IsMultiRegionTrail": false, "HomeRegion": "ap-northeast-1"}]}),
    );
    let (_, fs) = run_aws(&env);
    assert_eq!(violations(&fs, "A6").len(), 1, "single-region only: {fs:?}");
}

#[test]
fn a7_imdsv1_allowed_is_violation_with_manual_command() {
    let mut env = Env::new();
    env.aws_clean_account();
    env.aws_instances("optional");
    let (_, fs) = run_aws(&env);
    let v = violations(&fs, "A7");
    assert_eq!(v.len(), 1, "{fs:?}");
    assert_eq!(v[0]["target"], "i-0123web");
    assert!(
        v[0]["fix"].as_str().unwrap_or("").contains(
            "aws ec2 modify-instance-metadata-options --instance-id i-0123web --http-tokens required"
        ),
        "{}",
        v[0]
    );
    assert!(
        env.aws_log()
            .iter()
            .all(|a| a[1] != "modify-instance-metadata-options"),
        "A7 fix must never be executed"
    );
}
