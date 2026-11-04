//! AWS read-only audit A1–A7 (canon §4, CONTRACT §6) through a CLOSED call
//! whitelist (AW1–AW9). There is no API that runs an arbitrary aws command:
//! [`Call`] is the complete list of read calls, and the only two mutating
//! calls live in [`Mutation`], used by `aws apply` alone (canon §5).

use crate::config::Ctx;
use crate::finding::{self, Attached, Finding, SgDetail};
use crate::inventory::{Host, Source};
use harness_core::verdict::Required;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

pub const SENSITIVE_PORTS: [i64; 8] = [22, 3389, 3306, 5432, 6379, 27017, 9200, 11211];
const A_RULES: [&str; 7] = ["A1", "A2", "A3", "A4", "A5", "A6", "A7"];
pub const LOGIN_EXPIRED: &str = "AWS のログインが切れている（または認証できない）";

/// The complete read-only whitelist.
#[derive(Clone, Debug)]
pub enum Call<'a> {
    /// AW1
    CallerIdentity,
    /// AW2
    Regions,
    /// AW3
    SecurityGroups { region: &'a str },
    /// AW3 (sgr ids; 2026-11-04 ruling)
    SecurityGroupRules {
        region: &'a str,
        group: Option<&'a str>,
    },
    /// AW4
    Instances { region: &'a str },
    /// AW5
    NetworkInterfaces { region: &'a str },
    /// AW6
    ListBuckets,
    /// AW6
    PublicAccessBlock { bucket: &'a str },
    /// AW6
    BucketPolicyStatus { bucket: &'a str },
    /// AW7
    DbInstances { region: &'a str },
    /// AW8
    AccountSummary,
    /// AW9
    DescribeTrails { region: &'a str },
    /// AW9
    TrailStatus { name: &'a str, region: &'a str },
}

impl Call<'_> {
    fn argv(&self) -> Vec<String> {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut a = match self {
            Call::CallerIdentity => v(&["sts", "get-caller-identity"]),
            Call::Regions => v(&["ec2", "describe-regions"]),
            Call::SecurityGroups { region } => {
                v(&["ec2", "describe-security-groups", "--region", region])
            }
            Call::SecurityGroupRules { region, group } => {
                let mut a = v(&["ec2", "describe-security-group-rules", "--region", region]);
                if let Some(g) = group {
                    a.push("--filters".into());
                    a.push(format!("Name=group-id,Values={g}"));
                }
                a
            }
            Call::Instances { region } => v(&["ec2", "describe-instances", "--region", region]),
            Call::NetworkInterfaces { region } => {
                v(&["ec2", "describe-network-interfaces", "--region", region])
            }
            Call::ListBuckets => v(&["s3api", "list-buckets"]),
            Call::PublicAccessBlock { bucket } => {
                v(&["s3api", "get-public-access-block", "--bucket", bucket])
            }
            Call::BucketPolicyStatus { bucket } => {
                v(&["s3api", "get-bucket-policy-status", "--bucket", bucket])
            }
            Call::DbInstances { region } => {
                v(&["rds", "describe-db-instances", "--region", region])
            }
            Call::AccountSummary => v(&["iam", "get-account-summary"]),
            Call::DescribeTrails { region } => {
                v(&["cloudtrail", "describe-trails", "--region", region])
            }
            Call::TrailStatus { name, region } => v(&[
                "cloudtrail",
                "get-trail-status",
                "--name",
                name,
                "--region",
                region,
            ]),
        };
        a.push("--output".into());
        a.push("json".into());
        a
    }
}

/// The only two mutating calls hostaudit can issue (canon §5, `aws apply`).
#[derive(Clone, Debug)]
pub enum Mutation<'a> {
    Revoke {
        region: &'a str,
        group: &'a str,
        sgr: &'a str,
        dry_run: bool,
    },
    Authorize {
        region: &'a str,
        group: &'a str,
        protocol: &'a str,
        port: Option<&'a str>,
        cidr: &'a str,
        dry_run: bool,
    },
}

impl Mutation<'_> {
    fn argv(&self) -> Vec<String> {
        let mut a: Vec<String> = match self {
            Mutation::Revoke {
                region, group, sgr, ..
            } => [
                "ec2",
                "revoke-security-group-ingress",
                "--region",
                region,
                "--group-id",
                group,
                "--security-group-rule-ids",
                sgr,
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            Mutation::Authorize {
                region,
                group,
                protocol,
                port,
                cidr,
                ..
            } => {
                let mut a: Vec<String> = [
                    "ec2",
                    "authorize-security-group-ingress",
                    "--region",
                    region,
                    "--group-id",
                    group,
                    "--protocol",
                    protocol,
                ]
                .iter()
                .map(|s| s.to_string())
                .collect();
                if let Some(p) = port {
                    a.push("--port".into());
                    a.push(p.to_string());
                }
                a.push("--cidr".into());
                a.push(cidr.to_string());
                a
            }
        };
        let dry = match self {
            Mutation::Revoke { dry_run, .. } | Mutation::Authorize { dry_run, .. } => *dry_run,
        };
        if dry {
            a.push("--dry-run".into());
        }
        a.push("--output".into());
        a.push("json".into());
        a
    }

    /// Human-readable CLI line (for the diff / report).
    pub fn display(&self) -> String {
        format!("aws {}", self.argv().join(" "))
    }
}

#[derive(Debug, Clone)]
pub struct AwsErr {
    pub code: Option<i32>,
    pub stderr: String,
}

impl std::fmt::Display for AwsErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tail: String = self.stderr.trim().chars().take(400).collect();
        match self.code {
            Some(c) => write!(f, "aws exited {c}: {tail}"),
            None => write!(f, "aws did not run to completion: {tail}"),
        }
    }
}

fn spawn(ctx: &Ctx, argv: &[String]) -> Result<(i32, String, String), AwsErr> {
    let mut c = std::process::Command::new(&ctx.aws_bin);
    c.args(argv)
        .env("AWS_PAGER", "")
        .stdin(std::process::Stdio::null());
    match harness_core::boundary::run_with_timeout(&mut c, Duration::from_secs(180)).require() {
        Required::Determined(o) => {
            let code = o.code();
            let stderr = o.stderr().to_string();
            match o.stdout_allowing(&[code]).require() {
                Required::Determined(out) => Ok((code, out, stderr)),
                Required::Blocked(why) => Err(AwsErr {
                    code: None,
                    stderr: why.to_string(),
                }),
            }
        }
        Required::Blocked(why) => Err(AwsErr {
            code: None,
            stderr: why.to_string(),
        }),
    }
}

/// Run one whitelisted read call. Only exit 0 with JSON stdout is `Ok`.
pub fn call(ctx: &Ctx, c: &Call) -> Result<Value, AwsErr> {
    let (code, out, stderr) = spawn(ctx, &c.argv())?;
    if code != 0 {
        return Err(AwsErr {
            code: Some(code),
            stderr,
        });
    }
    serde_json::from_str::<Value>(&out).map_err(|e| AwsErr {
        code: Some(0),
        stderr: format!("output is not JSON: {e}"),
    })
}

/// Run one mutation; returns (exit code, stderr). Used by `aws apply` only.
pub fn mutate(ctx: &Ctx, m: &Mutation) -> Result<(i32, String), AwsErr> {
    spawn(ctx, &m.argv()).map(|(c, _, e)| (c, e))
}

// ───────────────────────────── json helpers ────────────────────────────────

pub fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn name_tag(inst: &Value) -> Option<String> {
    inst["Tags"]
        .as_array()?
        .iter()
        .find(|t| t["Key"] == "Name")
        .and_then(|t| s(&t["Value"]))
}

pub fn regions(ctx: &Ctx) -> Result<Vec<String>, String> {
    let v = call(ctx, &Call::Regions).map_err(|e| format!("ec2 describe-regions: {e}"))?;
    let arr = v["Regions"]
        .as_array()
        .ok_or("ec2 describe-regions: no Regions array")?;
    let mut out = Vec::new();
    for r in arr {
        out.push(s(&r["RegionName"]).ok_or("ec2 describe-regions: region without RegionName")?);
    }
    if out.is_empty() {
        return Err("ec2 describe-regions returned no regions".to_string());
    }
    Ok(out)
}

fn instances_of(v: &Value) -> Result<Vec<Value>, String> {
    let res = v["Reservations"]
        .as_array()
        .ok_or("describe-instances: no Reservations array")?;
    let mut out = Vec::new();
    for r in res {
        let is = r["Instances"]
            .as_array()
            .ok_or("describe-instances: reservation without Instances")?;
        out.extend(is.iter().cloned());
    }
    Ok(out)
}

/// EC2 instances as inventory hosts (CONTRACT §4.1 `--aws`). Any failure is
/// `Err` (inventory undetermined, exit 4) — never a shortened list.
pub fn ec2_hosts(ctx: &Ctx) -> Result<Vec<Host>, String> {
    call(ctx, &Call::CallerIdentity)
        .map_err(|e| format!("{LOGIN_EXPIRED}: sts get-caller-identity failed: {e}"))?;
    let mut hosts = Vec::new();
    for region in regions(ctx)? {
        let v = call(ctx, &Call::Instances { region: &region })
            .map_err(|e| format!("ec2 describe-instances --region {region}: {e}"))?;
        for inst in instances_of(&v)? {
            let id = s(&inst["InstanceId"]).ok_or("instance without InstanceId")?;
            let public_ip = s(&inst["PublicIpAddress"]);
            let private_ip = s(&inst["PrivateIpAddress"]);
            let mut h = Host::simple(&id, Source::Ec2);
            h.hostname = public_ip.clone().or_else(|| private_ip.clone());
            h.instance_id = Some(id);
            h.region = Some(region.clone());
            h.public_ip = public_ip;
            h.private_ip = private_ip;
            h.name = name_tag(&inst);
            hosts.push(h);
        }
    }
    Ok(hosts)
}

// ───────────────────────────── rules ───────────────────────────────────────

/// One ingress rule as returned by `describe-security-group-rules`.
#[derive(Clone, Debug)]
pub struct SgRule {
    pub sgr_id: String,
    pub group_id: String,
    pub is_egress: bool,
    pub protocol: String,
    pub from: i64,
    pub to: i64,
    pub cidr_v4: Option<String>,
    pub cidr_v6: Option<String>,
}

pub fn parse_rules(v: &Value) -> Result<Vec<SgRule>, String> {
    let arr = v["SecurityGroupRules"]
        .as_array()
        .ok_or("describe-security-group-rules: no SecurityGroupRules array")?;
    let mut out = Vec::new();
    for r in arr {
        out.push(SgRule {
            sgr_id: s(&r["SecurityGroupRuleId"]).ok_or("rule without SecurityGroupRuleId")?,
            group_id: s(&r["GroupId"]).ok_or("rule without GroupId")?,
            is_egress: r["IsEgress"].as_bool().ok_or("rule without IsEgress")?,
            protocol: s(&r["IpProtocol"]).ok_or("rule without IpProtocol")?,
            from: r["FromPort"].as_i64().unwrap_or(-1),
            to: r["ToPort"].as_i64().unwrap_or(-1),
            cidr_v4: s(&r["CidrIpv4"]),
            cidr_v6: s(&r["CidrIpv6"]),
        });
    }
    Ok(out)
}

impl SgRule {
    pub fn world_cidr(&self) -> Option<&str> {
        if self.cidr_v4.as_deref() == Some("0.0.0.0/0") {
            return Some("0.0.0.0/0");
        }
        if self.cidr_v6.as_deref() == Some("::/0") {
            return Some("::/0");
        }
        None
    }

    pub fn all_protocols(&self) -> bool {
        self.protocol == "-1"
    }

    fn port_based(&self) -> bool {
        matches!(self.protocol.as_str(), "tcp" | "udp" | "6" | "17")
    }

    /// Does this rule admit `port` over tcp?
    pub fn covers_tcp(&self, port: i64) -> bool {
        self.all_protocols()
            || (matches!(self.protocol.as_str(), "tcp" | "6")
                && self.from <= port
                && port <= self.to)
    }

    pub fn sensitive_ports(&self) -> Vec<i64> {
        if !self.port_based() {
            return Vec::new();
        }
        SENSITIVE_PORTS
            .iter()
            .copied()
            .filter(|p| self.from <= *p && *p <= self.to)
            .collect()
    }

    /// `A1` / `A2` when this ingress rule is a finding, else `None`.
    pub fn offending_rule(&self) -> Option<&'static str> {
        if self.is_egress {
            return None;
        }
        self.world_cidr()?;
        if self.all_protocols() {
            return Some("A2");
        }
        if !self.sensitive_ports().is_empty() {
            return Some("A1");
        }
        None
    }

    pub fn port_range(&self) -> String {
        if self.all_protocols() {
            "all".to_string()
        } else if self.from == self.to {
            self.from.to_string()
        } else {
            format!("{}-{}", self.from, self.to)
        }
    }

    pub fn protocol_label(&self) -> String {
        match self.protocol.as_str() {
            "-1" => "all".to_string(),
            "6" => "tcp".to_string(),
            "17" => "udp".to_string(),
            p => p.to_string(),
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {} {} from {}",
            self.sgr_id,
            self.protocol_label(),
            self.port_range(),
            self.cidr_v4
                .as_deref()
                .or(self.cidr_v6.as_deref())
                .unwrap_or("<non-CIDR source>")
        )
    }
}

pub fn console_path(sg: &str, sgr: &str) -> String {
    format!("EC2 → セキュリティグループ → {sg} → 「インバウンドルール」→「インバウンドルールを編集」→ {sgr}")
}

pub fn revoke_cli(region: &str, sg: &str, sgr: &str) -> String {
    format!(
        "aws ec2 revoke-security-group-ingress --region {region} --group-id {sg} --security-group-rule-ids {sgr}"
    )
}

/// Accumulates one rule's findings; `observed` = at least one target examined.
#[derive(Default)]
struct Acc {
    findings: Vec<Finding>,
    observed: bool,
}

struct Audit<'a> {
    ctx: &'a Ctx,
    acc: BTreeMap<&'static str, Acc>,
}

impl Audit<'_> {
    fn push(&mut self, rule: &'static str, f: Finding) {
        self.acc.entry(rule).or_default().findings.push(f);
    }
    fn observed(&mut self, rule: &'static str) {
        self.acc.entry(rule).or_default().observed = true;
    }
    fn undet(&mut self, rule: &'static str, target: &str, why: &str) {
        self.push(rule, Finding::undetermined(rule, target, vec![], why));
    }

    fn sg_rules(&mut self, region: &str, instances: &Result<Vec<Value>, String>) {
        let target = format!("aws:{region}");
        let rules = match call(
            self.ctx,
            &Call::SecurityGroupRules {
                region,
                group: None,
            },
        )
        .map_err(|e| e.to_string())
        .and_then(|v| parse_rules(&v))
        {
            Ok(r) => r,
            Err(e) => {
                let why = format!("describe-security-group-rules --region {region}: {e}");
                self.undet("A1", &target, &why);
                self.undet("A2", &target, &why);
                return;
            }
        };
        if !rules.is_empty() {
            self.observed("A1");
            self.observed("A2");
        }
        let names: Result<BTreeMap<String, String>, String> =
            call(self.ctx, &Call::SecurityGroups { region })
                .map_err(|e| format!("describe-security-groups: {e}"))
                .and_then(|v| {
                    let arr = v["SecurityGroups"]
                        .as_array()
                        .ok_or("describe-security-groups: no SecurityGroups array")?;
                    Ok(arr
                        .iter()
                        .filter_map(|g| Some((s(&g["GroupId"])?, s(&g["GroupName"])?)))
                        .collect())
                });
        let offending: Vec<(&'static str, &SgRule)> = rules
            .iter()
            .filter_map(|r| r.offending_rule().map(|k| (k, r)))
            .collect();
        if offending.is_empty() {
            return;
        }
        let enis: Result<Vec<Value>, String> = call(self.ctx, &Call::NetworkInterfaces { region })
            .map_err(|e| format!("describe-network-interfaces: {e}"))
            .and_then(|v| {
                v["NetworkInterfaces"]
                    .as_array()
                    .cloned()
                    .ok_or_else(|| "describe-network-interfaces: no NetworkInterfaces array".into())
            });
        let inst_names: Option<BTreeMap<String, Option<String>>> =
            instances.as_ref().ok().map(|is| {
                is.iter()
                    .filter_map(|i| Some((s(&i["InstanceId"])?, name_tag(i))))
                    .collect()
            });
        for (rule, r) in offending {
            let mut evidence = vec![format!("{} ({}, {region})", r.describe(), r.group_id)];
            let sg_name = match &names {
                Ok(m) => m.get(&r.group_id).cloned(),
                Err(e) => {
                    evidence.push(format!("SG name unknown: {e}"));
                    None
                }
            };
            let attached = match &enis {
                Ok(list) => Some(
                    list.iter()
                        .filter(|n| {
                            n["Groups"].as_array().is_some_and(|gs| {
                                gs.iter()
                                    .any(|g| g["GroupId"].as_str() == Some(&r.group_id))
                            })
                        })
                        .map(|n| {
                            let iid = s(&n["Attachment"]["InstanceId"]);
                            let name = iid.as_ref().and_then(|i| {
                                inst_names
                                    .as_ref()
                                    .and_then(|m| m.get(i).cloned().flatten())
                            });
                            Attached {
                                instance_id: iid,
                                name,
                                eni: s(&n["NetworkInterfaceId"])
                                    .unwrap_or_else(|| "<unknown eni>".into()),
                            }
                        })
                        .collect::<Vec<_>>(),
                ),
                Err(e) => {
                    evidence.push(format!("attachments unknown: {e}"));
                    None
                }
            };
            if let Some(att) = &attached {
                for a in att {
                    evidence.push(format!(
                        "attached: {} {} {}",
                        a.instance_id.as_deref().unwrap_or("<no instance>"),
                        a.name.as_deref().unwrap_or("<no Name tag>"),
                        a.eni
                    ));
                }
            }
            let cidr = r.world_cidr().unwrap_or("").to_string();
            let id = format!("{rule}:{region}:{}:{}", r.group_id, r.sgr_id);
            let revoke = revoke_cli(region, &r.group_id, &r.sgr_id);
            let authorize = if r.all_protocols() {
                format!(
                    "aws ec2 authorize-security-group-ingress --region {region} --group-id {} --protocol tcp --port <port> --cidr <trusted-cidr>  # only if a narrower replacement is needed",
                    r.group_id
                )
            } else {
                format!(
                    "aws ec2 authorize-security-group-ingress --region {region} --group-id {} --protocol {} --port {} --cidr <trusted-cidr>  # only if a narrower replacement is needed",
                    r.group_id,
                    r.protocol_label(),
                    r.port_range()
                )
            };
            let what = if rule == "A2" {
                "all protocols open to the world".to_string()
            } else {
                format!("port(s) {:?} open to the world", r.sensitive_ports())
            };
            let cp = console_path(&r.group_id, &r.sgr_id);
            let fix = format!(
                "{what} in {} ({}) in {region}.\nManual (default): {cp} — delete that row, or narrow its source to a trusted CIDR.\nCLI: {revoke}\nReplacement (if needed): {authorize}\nOpt-in automated fix (asks for confirmation): hostaudit aws apply --finding {id}",
                r.group_id,
                sg_name.as_deref().unwrap_or("<name unknown>")
            );
            let mut f = Finding::violation(rule, &r.group_id, evidence, &fix).with_id(id);
            f.sg = Some(SgDetail {
                region: region.to_string(),
                sg_id: r.group_id.clone(),
                sg_name,
                attached,
                protocol: r.protocol_label(),
                port_range: r.port_range(),
                cidr,
                sgr_id: r.sgr_id.clone(),
                console_path: cp,
                cli: vec![revoke, authorize],
            });
            self.push(rule, f);
        }
    }

    fn a7(&mut self, region: &str, instances: &Result<Vec<Value>, String>) {
        let list = match instances {
            Ok(l) => l,
            Err(e) => {
                self.undet("A7", &format!("aws:{region}"), e);
                return;
            }
        };
        for inst in list {
            let Some(id) = s(&inst["InstanceId"]) else {
                self.undet(
                    "A7",
                    &format!("aws:{region}"),
                    "instance without InstanceId",
                );
                continue;
            };
            if inst["State"]["Name"].as_str() == Some("terminated") {
                continue;
            }
            self.observed("A7");
            match inst["MetadataOptions"]["HttpTokens"].as_str() {
                Some("required") => {}
                Some("optional") => self.push(
                    "A7",
                    Finding::violation(
                        "A7",
                        &id,
                        vec![format!("{id} ({region}) MetadataOptions.HttpTokens = optional (IMDSv1 allowed)")],
                        &format!(
                            "Run by hand (hostaudit never executes this): aws ec2 modify-instance-metadata-options --instance-id {id} --http-tokens required --region {region}"
                        ),
                    ),
                ),
                other => self.undet(
                    "A7",
                    &id,
                    &format!("MetadataOptions.HttpTokens is {other:?}"),
                ),
            }
        }
    }

    fn a4(&mut self, region: &str) {
        let v = match call(self.ctx, &Call::DbInstances { region }) {
            Ok(v) => v,
            Err(e) => {
                self.undet(
                    "A4",
                    &format!("aws:{region}"),
                    &format!("rds describe-db-instances --region {region}: {e}"),
                );
                return;
            }
        };
        let Some(dbs) = v["DBInstances"].as_array() else {
            self.undet("A4", &format!("aws:{region}"), "no DBInstances array");
            return;
        };
        for db in dbs {
            let id = s(&db["DBInstanceIdentifier"]).unwrap_or_else(|| "<unknown db>".into());
            self.observed("A4");
            match db["PubliclyAccessible"].as_bool() {
                Some(false) => {}
                Some(true) => self.push(
                    "A4",
                    Finding::violation(
                        "A4",
                        &id,
                        vec![format!("{id} ({region}) PubliclyAccessible = true")],
                        &format!("コンソール: RDS → データベース → {id} → 「変更」→「接続」→「パブリックアクセス」= なし (region {region})"),
                    ),
                ),
                None => self.undet("A4", &id, "PubliclyAccessible missing"),
            }
        }
    }

    fn a3(&mut self) {
        let v = match call(self.ctx, &Call::ListBuckets) {
            Ok(v) => v,
            Err(e) => return self.undet("A3", "aws", &format!("s3api list-buckets: {e}")),
        };
        let Some(buckets) = v["Buckets"].as_array() else {
            return self.undet("A3", "aws", "s3api list-buckets: no Buckets array");
        };
        const FIX: &str = "コンソール: S3 → バケット → {b} → 「アクセス許可」→「ブロックパブリックアクセス (バケット設定)」→ 4 項目すべてオン、バケットポリシーの公開を解除";
        for b in buckets {
            let Some(name) = s(&b["Name"]) else {
                self.undet("A3", "aws", "bucket without Name");
                continue;
            };
            self.observed("A3");
            let mut ev = Vec::new();
            let mut viol = false;
            let mut undet = Vec::new();
            match call(self.ctx, &Call::PublicAccessBlock { bucket: &name }) {
                Ok(v) => {
                    let c = &v["PublicAccessBlockConfiguration"];
                    for k in [
                        "BlockPublicAcls",
                        "IgnorePublicAcls",
                        "BlockPublicPolicy",
                        "RestrictPublicBuckets",
                    ] {
                        match c[k].as_bool() {
                            Some(true) => {}
                            Some(false) => {
                                viol = true;
                                ev.push(format!("{name}: {k} = false"));
                            }
                            None => undet.push(format!("{k} missing")),
                        }
                    }
                }
                Err(e) if e.stderr.contains("NoSuchPublicAccessBlockConfiguration") => {
                    viol = true;
                    ev.push(format!("{name}: no Public Access Block configured"));
                }
                Err(e) => undet.push(format!("get-public-access-block: {e}")),
            }
            match call(self.ctx, &Call::BucketPolicyStatus { bucket: &name }) {
                Ok(v) => match v["PolicyStatus"]["IsPublic"].as_bool() {
                    Some(false) => {}
                    Some(true) => {
                        viol = true;
                        ev.push(format!("{name}: bucket policy is public"));
                    }
                    None => undet.push("PolicyStatus.IsPublic missing".into()),
                },
                Err(e) if e.stderr.contains("NoSuchBucketPolicy") => {}
                Err(e) => undet.push(format!("get-bucket-policy-status: {e}")),
            }
            if viol {
                self.push(
                    "A3",
                    Finding::violation("A3", &name, ev, &FIX.replace("{b}", &name)),
                );
            } else if !undet.is_empty() {
                self.push(
                    "A3",
                    Finding::undetermined("A3", &name, ev, &undet.join("; ")),
                );
            }
        }
    }

    fn a5(&mut self) {
        let v = match call(self.ctx, &Call::AccountSummary) {
            Ok(v) => v,
            Err(e) => return self.undet("A5", "aws", &format!("iam get-account-summary: {e}")),
        };
        let m = &v["SummaryMap"];
        let (mfa, keys) = (
            m["AccountMFAEnabled"].as_i64(),
            m["AccountAccessKeysPresent"].as_i64(),
        );
        let (Some(mfa), Some(keys)) = (mfa, keys) else {
            return self.undet(
                "A5",
                "aws",
                "SummaryMap lacks AccountMFAEnabled / AccountAccessKeysPresent",
            );
        };
        self.observed("A5");
        let ev = vec![format!(
            "root: AccountMFAEnabled = {mfa}, AccountAccessKeysPresent = {keys}"
        )];
        if mfa == 0 || keys == 1 {
            self.push(
                "A5",
                Finding::violation(
                    "A5",
                    "aws",
                    ev,
                    "コンソール: IAM → ルートユーザーのセキュリティ認証情報 → MFA を割り当て、ルートのアクセスキーを削除",
                ),
            );
        }
    }

    fn a6(&mut self, region: &str) {
        let v = match call(self.ctx, &Call::DescribeTrails { region }) {
            Ok(v) => v,
            Err(e) => return self.undet("A6", "aws", &format!("cloudtrail describe-trails: {e}")),
        };
        let Some(trails) = v["trailList"].as_array() else {
            return self.undet("A6", "aws", "describe-trails: no trailList array");
        };
        self.observed("A6");
        let mut undet = Vec::new();
        let mut ev = Vec::new();
        for t in trails
            .iter()
            .filter(|t| t["IsMultiRegionTrail"].as_bool() == Some(true))
        {
            let Some(name) = s(&t["TrailARN"]).or_else(|| s(&t["Name"])) else {
                undet.push("multi-region trail without TrailARN/Name".to_string());
                continue;
            };
            let home = s(&t["HomeRegion"]).unwrap_or_else(|| region.to_string());
            match call(
                self.ctx,
                &Call::TrailStatus {
                    name: &name,
                    region: &home,
                },
            ) {
                Ok(st) => match st["IsLogging"].as_bool() {
                    Some(true) => return,
                    Some(false) => ev.push(format!("{name}: multi-region, IsLogging = false")),
                    None => undet.push(format!("{name}: get-trail-status has no IsLogging")),
                },
                Err(e) => undet.push(format!("{name}: get-trail-status: {e}")),
            }
        }
        if !undet.is_empty() {
            self.push(
                "A6",
                Finding::undetermined("A6", "aws", ev, &undet.join("; ")),
            );
            return;
        }
        if ev.is_empty() {
            ev.push("no multi-region trail".to_string());
        }
        self.push(
            "A6",
            Finding::violation(
                "A6",
                "aws",
                ev,
                "コンソール: CloudTrail → 証跡 → 「証跡の作成」(すべてのリージョンに適用、ログ記録オン)",
            ),
        );
    }

    fn finalize(mut self) -> Vec<Finding> {
        let mut out = Vec::new();
        for rule in A_RULES {
            let acc = self.acc.remove(rule).unwrap_or_default();
            if !acc.findings.is_empty() {
                out.extend(acc.findings);
            } else if acc.observed {
                out.push(Finding::clean(rule, "aws", vec![]));
            } else {
                out.push(Finding::not_applicable(
                    rule,
                    "aws",
                    vec![],
                    "observed: the account has no resources of this kind",
                ));
            }
        }
        out
    }
}

/// The A1–A7 findings for the logged-in account.
pub fn audit(ctx: &Ctx) -> Vec<Finding> {
    if let Err(e) = call(ctx, &Call::CallerIdentity) {
        let why = format!(
            "{LOGIN_EXPIRED}: `aws login`（または有効な profile）で認証してから再実行してください。sts get-caller-identity: {e}"
        );
        return A_RULES
            .iter()
            .map(|r| Finding::undetermined(r, "aws", vec![], &why))
            .collect();
    }
    let mut a = Audit {
        ctx,
        acc: BTreeMap::new(),
    };
    match regions(ctx) {
        Ok(rs) => {
            for region in &rs {
                let instances = call(ctx, &Call::Instances { region })
                    .map_err(|e| format!("ec2 describe-instances --region {region}: {e}"))
                    .and_then(|v| instances_of(&v));
                a.sg_rules(region, &instances);
                a.a4(region);
                a.a7(region, &instances);
            }
            match rs.first() {
                Some(first) => a.a6(first),
                None => a.undet("A6", "aws", "no region to query CloudTrail in"),
            }
        }
        Err(e) => {
            for r in ["A1", "A2", "A4", "A6", "A7"] {
                a.undet(r, "aws", &e);
            }
        }
    }
    a.a3();
    a.a5();
    a.finalize()
}

/// `hostaudit aws [--json]`.
pub fn run_aws(ctx: &Ctx, json: bool) -> i32 {
    let findings = audit(ctx);
    finding::finish(ctx, &findings, json)
}
