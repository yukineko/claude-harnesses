//! Shared black-box harness for hostaudit tests (contract: crates/hostaudit/CONTRACT.md).
//!
//! Every external (ssh, aws, OSV, TLS probe, clock) is a fake under a tempdir.
//! The hostaudit process runs with a cleared environment.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// Serialises script writes and child spawns inside one test binary, so a
/// forked child never inherits a still-open write fd of a fake script
/// (ETXTBSY when hostaudit later execs it).
static FORK_LOCK: Mutex<()> = Mutex::new(());

pub const NOW: i64 = 1_800_000_000; // 2027-01-15T08:00:00Z
pub const DAY: i64 = 86_400;

pub const W1: &str = "LC_ALL=C cat /etc/os-release";
pub const W2: &str =
    r"LC_ALL=C dpkg-query -W -f '${Package}\t${Version}\t${source:Package}\t${source:Version}\n'";
pub const W3: &str = "LC_ALL=C ss -H -tln";
pub const W4: &str = "sudo -n LC_ALL=C sshd -T";
pub const W5: &str = "sudo -n LC_ALL=C ufw status";
pub const W6: &str = "sudo -n LC_ALL=C nft list ruleset";
pub const W7A: &str = "LC_ALL=C stat -c %Y /var/lib/apt/periodic/update-success-stamp";
pub const W7B: &str = "LC_ALL=C stat -c %Y /var/lib/apt/lists";
pub const W8: &str = "LC_ALL=C stat -c %Y /var/run/reboot-required";
pub const W9: &str = "LC_ALL=C systemctl is-enabled unattended-upgrades";
pub const W10: &str = "LC_ALL=C cat /etc/apt/apt.conf.d/20auto-upgrades";
pub const W11: &str = "LC_ALL=C date +%s";

pub const WHITELIST: [&str; 12] = [W1, W2, W3, W4, W5, W6, W7A, W7B, W8, W9, W10, W11];
pub const SUDO_CMDS: [&str; 3] = [W4, W5, W6];

pub const NO_SUCH_FILE: &str = "stat: cannot statx: No such file or directory\n";

#[derive(Clone, Debug)]
pub struct Resp {
    pub out: String,
    pub err: String,
    pub code: i32,
}

pub fn ok(out: &str) -> Resp {
    Resp {
        out: out.to_string(),
        err: String::new(),
        code: 0,
    }
}

pub fn fail(code: i32, err: &str) -> Resp {
    Resp {
        out: String::new(),
        err: err.to_string(),
        code,
    }
}

pub fn absent() -> Resp {
    fail(1, NO_SUCH_FILE)
}

pub fn ssh_down() -> Resp {
    fail(
        255,
        "ssh: connect to host 10.0.0.1 port 22: Connection timed out\n",
    )
}

pub fn sudo_fail() -> Resp {
    fail(1, "sudo: a password is required\n")
}

/// A host whose every whitelist answer is clean (B9 → not_applicable: no 443).
pub fn clean_host() -> BTreeMap<String, Resp> {
    let mut m = BTreeMap::new();
    m.insert(
        W1.into(),
        ok("PRETTY_NAME=\"Ubuntu 22.04.4 LTS\"\nNAME=\"Ubuntu\"\nID=ubuntu\nVERSION_ID=\"22.04\"\n"),
    );
    m.insert(
        W2.into(),
        ok("openssh-server\t1:8.9p1-3ubuntu0.10\topenssh\t1:8.9p1-3ubuntu0.10\n"),
    );
    m.insert(
        W3.into(),
        ok("LISTEN 0      128          0.0.0.0:22        0.0.0.0:*\nLISTEN 0      4096       127.0.0.1:3306      0.0.0.0:*\n"),
    );
    m.insert(
        W4.into(),
        ok("port 22\npermitrootlogin prohibit-password\npasswordauthentication no\n"),
    );
    m.insert(
        W5.into(),
        ok("Status: active\n\nTo                         Action      From\n22/tcp                     ALLOW       Anywhere\n"),
    );
    m.insert(W6.into(), ok("table inet filter {\n}\n"));
    m.insert(W7A.into(), ok(&format!("{}\n", NOW - DAY)));
    m.insert(W7B.into(), ok(&format!("{}\n", NOW - DAY)));
    m.insert(W8.into(), absent());
    m.insert(W9.into(), ok("enabled\n"));
    m.insert(
        W10.into(),
        ok("APT::Periodic::Update-Package-Lists \"1\";\nAPT::Periodic::Unattended-Upgrade \"1\";\n"),
    );
    m.insert(W11.into(), ok(&format!("{NOW}\n")));
    m
}

/// An OSV feed with one non-matching Ubuntu 22.04 record (so the ecosystem has
/// data and openssh 1:8.9p1-3ubuntu0.10 is NOT affected: fixed 0.4 < 0.10).
pub fn clean_osv() -> Value {
    serde_json::json!([{
        "id": "UBUNTU-CVE-2099-0001",
        "affected": [{
            "package": {"ecosystem": "Ubuntu:22.04:LTS", "name": "openssh"},
            "ranges": [{"type": "ECOSYSTEM", "events": [{"introduced": "0"}, {"fixed": "1:8.9p1-3ubuntu0.4"}]}]
        }]
    }])
}

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn json(&self) -> Value {
        serde_json::from_str(self.stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "stdout is not JSON ({e}); code={} stdout={} stderr={}",
                self.code, self.stdout, self.stderr
            )
        })
    }
    pub fn findings(&self) -> Vec<Value> {
        self.json()["findings"]
            .as_array()
            .unwrap_or_else(|| panic!("no findings array: {}", self.stdout))
            .clone()
    }
    pub fn dbg(&self) -> String {
        format!(
            "code={} stdout={} stderr={}",
            self.code, self.stdout, self.stderr
        )
    }
}

pub struct Env {
    pub _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub fakes: PathBuf,
    pub vars: BTreeMap<String, String>,
}

pub fn write_exec(path: &Path, body: &str) {
    let _g = FORK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().unwrap();
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn write(path: &Path, body: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

const FAKE_SSH: &str = r#"#!/bin/sh
R='__DIR__'
{ for a in "$@"; do printf '%s\037' "$a"; done; printf '\n'; } >> "$R/log"
dest=""; cmd=""
for a in "$@"; do dest=$cmd; cmd=$a; done
for f in "$R/hosts/$dest"/*.cmd; do
  [ -e "$f" ] || continue
  if [ "$(cat "$f")" = "$cmd" ]; then
    b=${f%.cmd}
    cat "$b.out"; cat "$b.err" >&2
    exit "$(cat "$b.code")"
  fi
done
echo "fake-ssh: no response for dest=[$dest] cmd=[$cmd]" >&2
exit 255
"#;

const FAKE_AWS: &str = r#"#!/bin/sh
R='__DIR__'
{ for a in "$@"; do printf '%s\037' "$a"; done; printf '\n'; } >> "$R/log"
svc=$1; op=$2; region=""; dry=""; prev=""
for a in "$@"; do
  [ "$prev" = "--region" ] && region=$a
  case "$a" in --region=*) region=${a#--region=};; --dry-run) dry=.dryrun;; esac
  prev=$a
done
base="$svc.$op$dry"
key=""
if [ -n "$region" ] && [ -e "$R/resp/$base@$region.code" ]; then key="$base@$region"
elif [ -e "$R/resp/$base.code" ]; then key="$base"; fi
if [ -z "$key" ]; then echo "fake-aws: no response for $base region=[$region]" >&2; exit 254; fi
if [ -e "$R/revoked" ] && [ -e "$R/resp/$key.after.code" ]; then key="$key.after"; fi
c=$(cat "$R/resp/$key.code")
cat "$R/resp/$key.out"; cat "$R/resp/$key.err" >&2
if [ "$base" = "ec2.revoke-security-group-ingress" ] && [ "$c" = "0" ]; then : > "$R/revoked"; fi
exit "$c"
"#;

const FAKE_TLS: &str = r#"#!/bin/sh
R='__DIR__'
{ for a in "$@"; do printf '%s\037' "$a"; done; printf '\n'; } >> "$R/log"
cat "$R/out"; exit "$(cat "$R/code")"
"#;

impl Env {
    pub fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        let cwd = root.join("proj");
        let fakes = root.join("fakes");
        for d in [&home, &cwd, &fakes] {
            std::fs::create_dir_all(d).unwrap();
        }
        let mut vars = BTreeMap::new();
        vars.insert("HOSTAUDIT_NOW".to_string(), NOW.to_string());
        Env {
            _tmp: tmp,
            root,
            home,
            cwd,
            fakes,
            vars,
        }
    }

    pub fn set(&mut self, k: &str, v: &str) {
        self.vars.insert(k.to_string(), v.to_string());
    }

    pub fn ssh_config(&self, body: &str) {
        write(&self.home.join(".ssh/config"), body);
    }

    pub fn hostaudit_dir(&self) -> PathBuf {
        self.home.join(".hostaudit")
    }

    // ---------- ssh fake ----------
    fn ssh_dir(&self) -> PathBuf {
        self.fakes.join("ssh")
    }

    /// Install the fake ssh (idempotent) and register `dest`'s responses.
    pub fn ssh_host(&mut self, dest: &str, resp: &BTreeMap<String, Resp>) {
        let d = self.ssh_dir();
        let hd = d.join("hosts").join(dest);
        let _ = std::fs::remove_dir_all(&hd);
        std::fs::create_dir_all(&hd).unwrap();
        for (i, (cmd, r)) in resp.iter().enumerate() {
            std::fs::write(hd.join(format!("{i}.cmd")), cmd).unwrap();
            std::fs::write(hd.join(format!("{i}.out")), &r.out).unwrap();
            std::fs::write(hd.join(format!("{i}.err")), &r.err).unwrap();
            std::fs::write(hd.join(format!("{i}.code")), r.code.to_string()).unwrap();
        }
        let bin = d.join("ssh");
        if !bin.exists() {
            write_exec(&bin, &FAKE_SSH.replace("__DIR__", d.to_str().unwrap()));
        }
        self.set("HOSTAUDIT_SSH", bin.to_str().unwrap());
    }

    /// Install the fake ssh with no hosts at all (every call fails 255, logged).
    pub fn ssh_fake_only(&mut self) {
        let d = self.ssh_dir();
        std::fs::create_dir_all(d.join("hosts")).unwrap();
        let bin = d.join("ssh");
        if !bin.exists() {
            write_exec(&bin, &FAKE_SSH.replace("__DIR__", d.to_str().unwrap()));
        }
        self.set("HOSTAUDIT_SSH", bin.to_str().unwrap());
    }

    /// Every logged ssh invocation as its argv (without argv[0]).
    pub fn ssh_log(&self) -> Vec<Vec<String>> {
        read_log(&self.ssh_dir().join("log"))
    }

    // ---------- aws fake ----------
    fn aws_dir(&self) -> PathBuf {
        self.fakes.join("aws")
    }

    /// Register a response for `<service>.<operation>[.dryrun][@region][.after]`.
    pub fn aws_resp(&mut self, key: &str, r: Resp) {
        let d = self.aws_dir();
        let rd = d.join("resp");
        std::fs::create_dir_all(&rd).unwrap();
        std::fs::write(rd.join(format!("{key}.out")), &r.out).unwrap();
        std::fs::write(rd.join(format!("{key}.err")), &r.err).unwrap();
        std::fs::write(rd.join(format!("{key}.code")), r.code.to_string()).unwrap();
        let bin = d.join("aws");
        if !bin.exists() {
            write_exec(&bin, &FAKE_AWS.replace("__DIR__", d.to_str().unwrap()));
        }
        self.set("HOSTAUDIT_AWS", bin.to_str().unwrap());
    }

    pub fn aws_json(&mut self, key: &str, v: Value) {
        self.aws_resp(key, ok(&v.to_string()));
    }

    pub fn aws_log(&self) -> Vec<Vec<String>> {
        read_log(&self.aws_dir().join("log"))
    }

    /// A fully clean single-region account (ap-northeast-1).
    pub fn aws_clean_account(&mut self) {
        self.aws_json(
            "sts.get-caller-identity",
            serde_json::json!({"UserId": "AIDAEXAMPLEUSERID", "Account": "123456789012",
                               "Arn": "arn:aws:iam::123456789012:user/alice"}),
        );
        self.aws_json(
            "ec2.describe-regions",
            serde_json::json!({"Regions": [{"RegionName": "ap-northeast-1"}]}),
        );
        self.aws_sg(&[
            sgr("sgr-0https", "tcp", 443, 443, Some("0.0.0.0/0"), None),
            sgr("sgr-0office", "tcp", 22, 22, Some("203.0.113.0/24"), None),
        ]);
        self.aws_instances("required");
        self.aws_json(
            "ec2.describe-network-interfaces",
            serde_json::json!({"NetworkInterfaces": [{
                "NetworkInterfaceId": "eni-0web",
                "Groups": [{"GroupId": "sg-0aaa", "GroupName": "web-sg"}],
                "Attachment": {"InstanceId": "i-0123web"}
            }]}),
        );
        self.aws_json(
            "s3api.list-buckets",
            serde_json::json!({"Buckets": [{"Name": "bucket-one"}]}),
        );
        self.aws_json(
            "s3api.get-public-access-block",
            serde_json::json!({"PublicAccessBlockConfiguration": {
                "BlockPublicAcls": true, "IgnorePublicAcls": true,
                "BlockPublicPolicy": true, "RestrictPublicBuckets": true}}),
        );
        self.aws_json(
            "s3api.get-bucket-policy-status",
            serde_json::json!({"PolicyStatus": {"IsPublic": false}}),
        );
        self.aws_json(
            "rds.describe-db-instances",
            serde_json::json!({"DBInstances": [{"DBInstanceIdentifier": "db-one", "PubliclyAccessible": false}]}),
        );
        self.aws_json(
            "iam.get-account-summary",
            serde_json::json!({"SummaryMap": {"AccountMFAEnabled": 1, "AccountAccessKeysPresent": 0}}),
        );
        self.aws_json(
            "cloudtrail.describe-trails",
            serde_json::json!({"trailList": [{"Name": "org-trail",
                "TrailARN": "arn:aws:cloudtrail:ap-northeast-1:123456789012:trail/org-trail",
                "IsMultiRegionTrail": true, "HomeRegion": "ap-northeast-1"}]}),
        );
        self.aws_json(
            "cloudtrail.get-trail-status",
            serde_json::json!({"IsLogging": true}),
        );
    }

    /// Security group sg-0aaa (web-sg) with the given ingress rules, served
    /// consistently by describe-security-groups and describe-security-group-rules.
    pub fn aws_sg(&mut self, rules: &[Value]) {
        self.aws_sg_key("", rules);
    }

    /// Same, but registered as the response served after a successful revoke.
    pub fn aws_sg_after(&mut self, rules: &[Value]) {
        self.aws_sg_key(".after", rules);
    }

    fn aws_sg_key(&mut self, suffix: &str, rules: &[Value]) {
        let perms: Vec<Value> = rules
            .iter()
            .map(|r| {
                let mut p = serde_json::json!({
                    "IpProtocol": r["IpProtocol"], "IpRanges": [], "Ipv6Ranges": []
                });
                if r["IpProtocol"] != "-1" {
                    p["FromPort"] = r["FromPort"].clone();
                    p["ToPort"] = r["ToPort"].clone();
                }
                if let Some(c) = r.get("CidrIpv4") {
                    p["IpRanges"] = serde_json::json!([{"CidrIp": c}]);
                }
                if let Some(c) = r.get("CidrIpv6") {
                    p["Ipv6Ranges"] = serde_json::json!([{"CidrIpv6": c}]);
                }
                p
            })
            .collect();
        self.aws_json(
            &format!("ec2.describe-security-groups{suffix}"),
            serde_json::json!({"SecurityGroups": [{
                "GroupId": "sg-0aaa", "GroupName": "web-sg", "VpcId": "vpc-1",
                "IpPermissions": perms, "IpPermissionsEgress": []}]}),
        );
        self.aws_json(
            &format!("ec2.describe-security-group-rules{suffix}"),
            serde_json::json!({"SecurityGroupRules": rules}),
        );
    }

    pub fn aws_instances(&mut self, http_tokens: &str) {
        self.aws_json(
            "ec2.describe-instances",
            serde_json::json!({"Reservations": [{"Instances": [{
                "InstanceId": "i-0123web",
                "PublicIpAddress": "198.51.100.10",
                "PrivateIpAddress": "10.1.0.5",
                "Tags": [{"Key": "Name", "Value": "web"}],
                "SecurityGroups": [{"GroupId": "sg-0aaa", "GroupName": "web-sg"}],
                "NetworkInterfaces": [{"NetworkInterfaceId": "eni-0web", "Groups": [{"GroupId": "sg-0aaa"}]}],
                "MetadataOptions": {"HttpTokens": http_tokens, "HttpEndpoint": "enabled"}
            }]}]}),
        );
    }

    // ---------- OSV ----------
    pub fn osv_feed(&mut self, v: &Value) {
        let p = self.fakes.join("osv.json");
        std::fs::write(&p, v.to_string()).unwrap();
        self.set("HOSTAUDIT_OSV_URL", &format!("file://{}", p.display()));
    }

    pub fn osv_broken(&mut self) {
        let p = self.fakes.join("no-such-osv-feed.json");
        self.set("HOSTAUDIT_OSV_URL", &format!("file://{}", p.display()));
    }

    // ---------- TLS ----------
    pub fn tls_probe(&mut self, out: &str, code: i32) {
        let d = self.fakes.join("tls");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("out"), out).unwrap();
        std::fs::write(d.join("code"), code.to_string()).unwrap();
        let bin = d.join("probe");
        if !bin.exists() {
            write_exec(&bin, &FAKE_TLS.replace("__DIR__", d.to_str().unwrap()));
        }
        self.set("HOSTAUDIT_TLS_PROBE", bin.to_str().unwrap());
    }

    pub fn tls_log(&self) -> Vec<Vec<String>> {
        read_log(&self.fakes.join("tls/log"))
    }

    // ---------- run ----------
    pub fn run(&self, args: &[&str]) -> Out {
        self.run_with(args, &[])
    }

    pub fn run_with(&self, args: &[&str], extra: &[(&str, &str)]) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hostaudit"));
        cmd.args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &self.vars {
            cmd.env(k, v);
        }
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let child = {
            let _g = FORK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            cmd.spawn().expect("hostaudit spawns")
        };
        let out = child.wait_with_output().unwrap();
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    pub fn reports(&self) -> Vec<Value> {
        let p = self.hostaudit_dir().join("reports.jsonl");
        let s = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("reports.jsonl not readable at {}: {e}", p.display()));
        s.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad report row {l}: {e}")))
            .collect()
    }

    pub fn reports_raw(&self) -> String {
        std::fs::read_to_string(self.hostaudit_dir().join("reports.jsonl")).unwrap_or_default()
    }

    /// ssh config with one host `web1` (HostName 10.0.0.1), selected; ssh fake
    /// answers with `resp`; OSV feed clean. Ready for `audit --json`.
    pub fn one_selected_host(&mut self, resp: &BTreeMap<String, Resp>) {
        self.ssh_config("Host web1\n  HostName 10.0.0.1\n  User ubuntu\n");
        self.ssh_host("web1", resp);
        self.osv_feed(&clean_osv());
        let o = self.run(&["select", "web1"]);
        assert_eq!(o.code, 0, "select web1 must succeed: {}", o.dbg());
    }

    /// `one_selected_host` + `audit --json`.
    pub fn audit_one(&mut self, resp: &BTreeMap<String, Resp>) -> Out {
        self.one_selected_host(resp);
        self.run(&["audit", "--json"])
    }
}

pub fn sgr(id: &str, proto: &str, from: i64, to: i64, v4: Option<&str>, v6: Option<&str>) -> Value {
    let mut r = serde_json::json!({
        "SecurityGroupRuleId": id, "GroupId": "sg-0aaa", "IsEgress": false,
        "IpProtocol": proto, "FromPort": from, "ToPort": to
    });
    if let Some(c) = v4 {
        r["CidrIpv4"] = Value::String(c.to_string());
    }
    if let Some(c) = v6 {
        r["CidrIpv6"] = Value::String(c.to_string());
    }
    r
}

pub fn read_log(p: &Path) -> Vec<Vec<String>> {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    s.lines()
        .map(|l| {
            l.split('\u{1f}')
                .filter(|a| !a.is_empty())
                .map(str::to_string)
                .collect()
        })
        .collect()
}

/// The unique finding for (target, rule); panics with context otherwise.
pub fn finding(fs: &[Value], target: &str, rule: &str) -> Value {
    let m: Vec<&Value> = fs
        .iter()
        .filter(|f| f["target"] == target && f["rule"] == rule)
        .collect();
    assert_eq!(
        m.len(),
        1,
        "expected exactly one finding target={target} rule={rule}, got {m:?} in {fs:?}"
    );
    m[0].clone()
}

pub fn verdict(fs: &[Value], target: &str, rule: &str) -> String {
    finding(fs, target, rule)["verdict"]
        .as_str()
        .unwrap_or("<missing>")
        .to_string()
}

/// All findings for a rule (any target).
pub fn by_rule(fs: &[Value], rule: &str) -> Vec<Value> {
    fs.iter().filter(|f| f["rule"] == rule).cloned().collect()
}

pub fn has(fs: &[Value], rule: &str, verdict: &str) -> bool {
    fs.iter()
        .any(|f| f["rule"] == rule && f["verdict"] == verdict)
}

pub fn evidence_text(f: &Value) -> String {
    f["evidence"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
