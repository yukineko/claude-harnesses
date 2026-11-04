//! Baseline rules B1–B9 (canon §3.3, CONTRACT §4.5), B5 with the v1.1
//! per-CVE evaluation (canon v1.1 §3). The `audit` command is in `audit.rs`.
//!
//! Every host gets exactly one finding per rule; a rule whose input could not
//! be observed is `undetermined`, never omitted and never clean.

use crate::audit::{CveCtx, Memo};
use crate::config::Ctx;
use crate::cve;
use crate::feeds;
use crate::finding::Finding;
use crate::inventory::Host;
use crate::osv::{self, Feed};
use crate::ssh::{Obs, Remote};
use std::collections::{BTreeMap, BTreeSet};

const DAY: i64 = 86_400;
const APT_INDEX_MAX: i64 = 7 * DAY;
const REBOOT_MAX: i64 = 7 * DAY;
const TLS_WARN: i64 = 14 * DAY;
const DB_PORTS: [(u16, &str, &str); 6] = [
    (
        3306,
        "MySQL/MariaDB",
        "/etc/mysql/mysql.conf.d/mysqld.cnf (or /etc/mysql/mariadb.conf.d/50-server.cnf): bind-address = 127.0.0.1",
    ),
    (
        5432,
        "PostgreSQL",
        "/etc/postgresql/<version>/main/postgresql.conf: listen_addresses = 'localhost'",
    ),
    (6379, "Redis", "/etc/redis/redis.conf: bind 127.0.0.1 ::1"),
    (27017, "MongoDB", "/etc/mongod.conf: net.bindIp: 127.0.0.1"),
    (
        9200,
        "Elasticsearch",
        "/etc/elasticsearch/elasticsearch.yml: network.host: 127.0.0.1",
    ),
    (11211, "memcached", "/etc/memcached.conf: -l 127.0.0.1"),
];

pub type Answers = BTreeMap<Remote, Obs>;

fn obs(a: &Answers, r: Remote) -> Obs {
    match a.get(&r) {
        Some(o) => o.clone(),
        None => Obs::Undet(format!("{} was not run", r.id())),
    }
}

fn parse_int(o: &Obs, r: Remote) -> Result<Option<i64>, String> {
    match o {
        Obs::Out(s) => s
            .trim()
            .parse::<i64>()
            .map(Some)
            .map_err(|_| format!("{} output {:?} is not unix seconds", r.id(), s.trim())),
        Obs::Absent => Ok(None),
        Obs::Unit(u) => Err(format!("{} unexpected output {u:?}", r.id())),
        Obs::Undet(why) | Obs::Missing(why) => Err(why.clone()),
    }
}

// ───────────────────────────── B1 / B2 ─────────────────────────────────────

fn sshd_key(rule: &str, t: &str, w4: &Obs, key: &str, fix: &str) -> Finding {
    let text = match w4 {
        Obs::Out(s) => s,
        Obs::Undet(why) => return Finding::undetermined(rule, t, vec![], why),
        other => {
            return Finding::undetermined(rule, t, vec![], &format!("W4 unexpected {other:?}"))
        }
    };
    let line = text.lines().map(str::trim).find(|l| {
        l.split_whitespace()
            .next()
            .is_some_and(|k| k.eq_ignore_ascii_case(key))
    });
    match line {
        None => Finding::undetermined(rule, t, vec![], &format!("`sshd -T` has no {key} line")),
        Some(l) => {
            let v = l.split_whitespace().nth(1).unwrap_or("");
            if v.eq_ignore_ascii_case("yes") {
                Finding::violation(rule, t, vec![l.to_string()], fix)
            } else {
                Finding::clean(rule, t, vec![l.to_string()])
            }
        }
    }
}

// ───────────────────────────── B3 ──────────────────────────────────────────

/// A chain whose body contains both `hook input` and `policy drop`.
fn nft_has_input_drop(ruleset: &str) -> bool {
    let mut rest = ruleset;
    while let Some(pos) = rest.find("chain ") {
        let after = &rest[pos..];
        let Some(open) = after.find('{') else {
            break;
        };
        let mut depth = 0i32;
        let mut end = after.len();
        for (i, c) in after[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &after[open..end];
        if body.contains("hook input") && body.contains("policy drop") {
            return true;
        }
        rest = &after[end.min(after.len())..];
        if end == after.len() {
            break;
        }
    }
    false
}

fn b3(t: &str, w5: &Obs, w6: &Obs) -> Finding {
    const FIX: &str = "sudo ufw allow OpenSSH && sudo ufw enable";
    let ufw = match w5 {
        Obs::Out(s) => match s.lines().map(str::trim).find(|l| !l.is_empty()) {
            Some("Status: active") => Ok(true),
            Some("Status: inactive") => Ok(false),
            other => Err(format!("unrecognised `ufw status` output {other:?}")),
        },
        Obs::Undet(why) => Err(why.clone()),
        other => Err(format!("W5 unexpected {other:?}")),
    };
    let nft = match w6 {
        Obs::Out(s) => Ok(nft_has_input_drop(s)),
        Obs::Undet(why) => Err(why.clone()),
        other => Err(format!("W6 unexpected {other:?}")),
    };
    match (ufw, nft) {
        (Err(a), Err(b)) => Finding::undetermined("B3", t, vec![], &format!("{a}; {b}")),
        (Err(a), _) | (_, Err(a)) => Finding::undetermined("B3", t, vec![], &a),
        (Ok(active), Ok(drop)) => {
            let ev = vec![
                format!("ufw: {}", if active { "active" } else { "inactive" }),
                format!(
                    "nft input chain with policy drop: {}",
                    if drop { "yes" } else { "no" }
                ),
            ];
            if active || drop {
                Finding::clean("B3", t, ev)
            } else {
                Finding::violation("B3", t, ev, FIX)
            }
        }
    }
}

// ───────────────────────────── B4 / B9 ─────────────────────────────────────

struct Listen {
    line: String,
    addr: String,
    port: u16,
}

fn parse_listen(text: &str) -> Result<Vec<Listen>, String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let local = line
            .split_whitespace()
            .nth(3)
            .ok_or_else(|| format!("`ss -H -tln` line has no local address column: {line:?}"))?;
        let (addr, port) = local
            .rsplit_once(':')
            .ok_or_else(|| format!("local address {local:?} has no port"))?;
        let port = port
            .parse::<u16>()
            .map_err(|_| format!("local address {local:?} has no numeric port"))?;
        let addr = addr.split('%').next().unwrap_or(addr).to_string();
        out.push(Listen {
            line: line.trim().to_string(),
            addr,
            port,
        });
    }
    Ok(out)
}

fn b4(t: &str, listen: &Result<Vec<Listen>, String>) -> Finding {
    let ls = match listen {
        Ok(l) => l,
        Err(e) => return Finding::undetermined("B4", t, vec![], e),
    };
    let mut ev = Vec::new();
    let mut fixes = Vec::new();
    for l in ls {
        if !matches!(l.addr.as_str(), "0.0.0.0" | "[::]" | "*" | "::") {
            continue;
        }
        if let Some((_, svc, file)) = DB_PORTS.iter().find(|(p, _, _)| *p == l.port) {
            ev.push(l.line.clone());
            let fx = format!(
                "{svc} (port {}) listens on {}: bind it to 127.0.0.1 — {file}, then restart the service",
                l.port, l.addr
            );
            if !fixes.contains(&fx) {
                fixes.push(fx);
            }
        }
    }
    if ev.is_empty() {
        Finding::clean("B4", t, vec![])
    } else {
        Finding::violation("B4", t, ev, &fixes.join("\n"))
    }
}

fn b9(ctx: &Ctx, host: &Host, listen: &Result<Vec<Listen>, String>) -> Finding {
    let t = host.alias.as_str();
    let ls = match listen {
        Ok(l) => l,
        Err(e) => return Finding::undetermined("B9", t, vec![], e),
    };
    let Some(l443) = ls.iter().find(|l| l.port == 443) else {
        return Finding::not_applicable("B9", t, vec![], "nothing listens on 443");
    };
    let name = host.tls_name();
    match crate::tls::probe(ctx, name) {
        Err(e) => Finding::undetermined(
            "B9",
            t,
            vec![l443.line.clone()],
            &format!("TLS probe of {name}:443 failed: {e}"),
        ),
        Ok(not_after) => {
            let left = not_after - ctx.now;
            let ev = vec![format!(
                "{name}:443 notAfter={} ({} days left)",
                chrono::DateTime::from_timestamp(not_after, 0)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_else(|| not_after.to_string()),
                left / DAY
            )];
            if left <= TLS_WARN {
                Finding::violation(
                    "B9",
                    t,
                    ev,
                    "Renew the certificate (certbot: `sudo certbot renew`), then reload the web server",
                )
            } else {
                Finding::clean("B9", t, ev)
            }
        }
    }
}

// ───────────────────────────── B5 (+ K2) ───────────────────────────────────

pub type FeedMemo = BTreeMap<&'static str, Result<Feed, String>>;

/// How a CVE rule (B5 / K2) words its result.
pub struct CveRule<'a> {
    pub rule: &'a str,
    /// What was checked, for the clean evidence.
    pub scope: String,
    /// Fix text for the affected items.
    pub fix: fn(&[cve::Item]) -> String,
}

/// The finding of a CVE rule plus the evaluated items (`Err` = undetermined).
pub struct CveOutcome {
    pub finding: Finding,
    pub items: Result<Vec<cve::Item>, String>,
}

fn undetermined_cve(rule: &str, t: &str, why: &str) -> CveOutcome {
    CveOutcome {
        finding: Finding::undetermined(rule, t, vec![], why),
        items: Err(why.to_string()),
    }
}

/// OSV match of `pkgs` (by source package / source version) plus the
/// per-CVE evaluation of canon v1.1 §3 — shared by B5 (every installed
/// package) and K2 (the running kernel's package only).
pub fn cve_rule(
    ctx: &Ctx,
    t: &str,
    spec: &CveRule<'_>,
    os_release: &str,
    pkgs: &[osv::Pkg],
    memo: &mut Memo,
    cx: &CveCtx,
) -> CveOutcome {
    let rule = spec.rule;
    let eco = match osv::eco_from_os_release(os_release) {
        Ok(e) => e,
        Err(e) => return undetermined_cve(rule, t, &e),
    };
    let Memo {
        osv: osv_memo, kev, ..
    } = memo;
    let feed = osv_memo
        .entry(eco.base())
        .or_insert_with(|| osv::load(ctx, eco.base()));
    let feed = match feed {
        Ok(f) => f,
        Err(e) => return undetermined_cve(rule, t, &format!("advisories unavailable: {e}")),
    };
    let (count, hits) = osv::evaluate(feed, &eco, pkgs);
    let mut out = if count == 0 {
        undetermined_cve(
            rule,
            t,
            &format!(
                "the advisory feed has 0 records for {}; no data is not the same as no vulnerabilities",
                eco.label()
            ),
        )
    } else if hits.is_empty() {
        let mut ev = vec![format!(
            "{} checked against {count} {} advisories",
            spec.scope,
            eco.label()
        )];
        if let Some(n) = &feed.cache_note {
            ev.push(n.clone());
        }
        let (cves, counts) = cve::to_json(&[]);
        CveOutcome {
            finding: Finding::clean(rule, t, ev)
                .with("cves", cves)
                .with("priority_counts", counts),
            items: Ok(Vec::new()),
        }
    } else {
        // KEV is fetched only when there is something to look up.
        let kev = kev.get_or_insert_with(|| feeds::load_kev(ctx));
        let (kev_set, kev_at) = match kev {
            Ok(k) => (Ok(&k.data), Some(k.fetched_at)),
            Err(e) => (Err(e.as_str()), None),
        };
        let items = cve::evaluate(&hits, kev_set, &cx.exposure, &cx.candidates);
        let mut ev: Vec<String> = hits
            .iter()
            .map(|h| {
                format!(
                    "{} {} (source {} {}): {}",
                    h.pkg.name, h.pkg.version, h.pkg.source, h.pkg.source_version, h.advisory
                )
            })
            .collect();
        ev.extend(cve::evidence(&items, kev_set));
        if let Some(why) = &cx.exposure.unknown {
            ev.push(format!("exposure unknown: {why}"));
        }
        if let Err(why) = &cx.candidates {
            ev.push(format!("W14 candidate versions unknown: {why}"));
        }
        let (cves, counts) = cve::to_json(&items);
        CveOutcome {
            finding: Finding::violation(rule, t, ev, &(spec.fix)(&items))
                .with("cves", cves)
                .with("priority_counts", counts)
                .with(
                    "kev_fetched_at",
                    kev_at
                        .map(serde_json::Value::from)
                        .unwrap_or(serde_json::Value::Null),
                ),
            items: Ok(items),
        }
    };
    out.finding.advisory_fetched_at = Some(feed.fetched_at);
    out
}

fn b5_fix(items: &[cve::Item]) -> String {
    cve::fix_text(items, "", "")
}

/// B5 for one host; also returns the affected binary packages (for S7).
fn b5(
    ctx: &Ctx,
    t: &str,
    w1: &Obs,
    w2: &Obs,
    memo: &mut Memo,
    cx: &CveCtx,
) -> (Finding, Result<BTreeSet<String>, String>) {
    let os = match w1 {
        Obs::Out(s) => s,
        Obs::Undet(why) => {
            return (
                Finding::undetermined("B5", t, vec![], why),
                Err(why.clone()),
            )
        }
        other => {
            let why = format!("W1 unexpected {other:?}");
            return (Finding::undetermined("B5", t, vec![], &why), Err(why));
        }
    };
    let pkgs = match w2 {
        Obs::Out(s) => match osv::parse_dpkg(s) {
            Ok(p) => p,
            Err(e) => return (Finding::undetermined("B5", t, vec![], &e), Err(e)),
        },
        Obs::Undet(why) => {
            return (
                Finding::undetermined("B5", t, vec![], why),
                Err(why.clone()),
            )
        }
        other => {
            let why = format!("W2 unexpected {other:?}");
            return (Finding::undetermined("B5", t, vec![], &why), Err(why));
        }
    };
    let spec = CveRule {
        rule: "B5",
        scope: format!("{} installed packages", pkgs.len()),
        fix: b5_fix,
    };
    let o = cve_rule(ctx, t, &spec, os, &pkgs, memo, cx);
    let bins = o
        .items
        .map(|items| items.into_iter().map(|i| i.package).collect());
    (o.finding, bins)
}

// ───────────────────────────── B6 / B7 / B8 ────────────────────────────────

fn b6(t: &str, w9: &Obs, w10: &Obs) -> Finding {
    const FIX: &str =
        "sudo apt install unattended-upgrades && sudo dpkg-reconfigure -plow unattended-upgrades";
    let mut ev = Vec::new();
    let mut violation = false;
    let mut undet = Vec::new();
    match w9 {
        Obs::Out(s) | Obs::Unit(s) => {
            let v = s.lines().next().map(str::trim).unwrap_or("").to_string();
            ev.push(format!("unattended-upgrades: {v}"));
            if v != "enabled" {
                violation = true;
            }
        }
        Obs::Undet(why) | Obs::Missing(why) => undet.push(why.clone()),
        Obs::Absent => undet.push("W9 unexpected absent".to_string()),
    }
    match w10 {
        Obs::Absent => {
            ev.push("/etc/apt/apt.conf.d/20auto-upgrades: absent".to_string());
            violation = true;
        }
        Obs::Out(s) => {
            ev.extend(
                s.lines()
                    .filter(|l| l.contains("APT::Periodic::"))
                    .map(|l| l.trim().to_string()),
            );
            if !s
                .lines()
                .any(|l| l.contains("APT::Periodic::Unattended-Upgrade \"1\""))
            {
                violation = true;
            }
        }
        Obs::Undet(why) | Obs::Missing(why) => undet.push(why.clone()),
        Obs::Unit(u) => undet.push(format!("W10 unexpected {u:?}")),
    }
    if violation {
        Finding::violation("B6", t, ev, FIX)
    } else if !undet.is_empty() {
        Finding::undetermined("B6", t, ev, &undet.join("; "))
    } else {
        Finding::clean("B6", t, ev)
    }
}

fn b7(t: &str, a: &Answers) -> Finding {
    let w7a = parse_int(&obs(a, Remote::AptStamp), Remote::AptStamp);
    let w7b = parse_int(&obs(a, Remote::AptLists), Remote::AptLists);
    let w11 = parse_int(&obs(a, Remote::Date), Remote::Date);
    let errs: Vec<String> = [&w7a, &w7b, &w11]
        .iter()
        .filter_map(|r| r.as_ref().err().cloned())
        .collect();
    if !errs.is_empty() {
        return Finding::undetermined("B7", t, vec![], &errs.join("; "));
    }
    let (Ok(x), Ok(y), Ok(now)) = (w7a, w7b, w11) else {
        return Finding::undetermined("B7", t, vec![], "internal: unreachable");
    };
    let Some(now) = now else {
        return Finding::undetermined("B7", t, vec![], "W11 returned no time");
    };
    let Some(stamp) = x.into_iter().chain(y).max() else {
        return Finding::undetermined(
            "B7",
            t,
            vec![],
            "neither /var/lib/apt/periodic/update-success-stamp nor /var/lib/apt/lists exists",
        );
    };
    let age = now - stamp;
    let ev = vec![format!("apt index updated {} days ago", age / DAY)];
    let mut f = if age > APT_INDEX_MAX {
        Finding::violation(
            "B7",
            t,
            ev,
            "sudo apt update (fixing B6 keeps it current automatically)",
        )
    } else {
        Finding::clean("B7", t, ev)
    };
    f.apt_index_time = Some(stamp);
    f
}

fn b8(t: &str, a: &Answers) -> Finding {
    let w8 = match parse_int(&obs(a, Remote::RebootRequired), Remote::RebootRequired) {
        Ok(None) => {
            return Finding::clean("B8", t, vec!["/var/run/reboot-required: absent".into()])
        }
        Ok(Some(v)) => v,
        Err(e) => return Finding::undetermined("B8", t, vec![], &e),
    };
    let now = match parse_int(&obs(a, Remote::Date), Remote::Date) {
        Ok(Some(n)) => n,
        Ok(None) => return Finding::undetermined("B8", t, vec![], "W11 returned no time"),
        Err(e) => return Finding::undetermined("B8", t, vec![], &e),
    };
    let age = now - w8;
    let ev = vec![format!("reboot required for {} days", age / DAY)];
    if age > REBOOT_MAX {
        Finding::violation(
            "B8",
            t,
            ev,
            "Reboot in a planned maintenance window (`sudo reboot`)",
        )
    } else {
        Finding::clean("B8", t, ev)
    }
}

/// B1–B9 for one host. Also returns the B5-affected binary packages
/// (`Err` = B5 undetermined), which S7 needs.
pub fn evaluate(
    ctx: &Ctx,
    host: &Host,
    a: &Answers,
    memo: &mut Memo,
    cx: &CveCtx,
) -> (Vec<Finding>, Result<BTreeSet<String>, String>) {
    let t = host.alias.as_str();
    let w4 = obs(a, Remote::SshdConfig);
    let listen = match obs(a, Remote::Listen) {
        Obs::Out(s) => parse_listen(&s),
        Obs::Undet(why) => Err(why),
        other => Err(format!("W3 unexpected {other:?}")),
    };
    let (f5, bins) = b5(
        ctx,
        t,
        &obs(a, Remote::OsRelease),
        &obs(a, Remote::DpkgList),
        memo,
        cx,
    );
    let fs = vec![
        sshd_key(
            "B1",
            t,
            &w4,
            "permitrootlogin",
            "Set `PermitRootLogin prohibit-password` (or `no`) in /etc/ssh/sshd_config, then `sudo sshd -t && sudo systemctl reload ssh`",
        ),
        sshd_key(
            "B2",
            t,
            &w4,
            "passwordauthentication",
            "Set `PasswordAuthentication no` in /etc/ssh/sshd_config (confirm key login works first), then `sudo sshd -t && sudo systemctl reload ssh`",
        ),
        b3(t, &obs(a, Remote::Ufw), &obs(a, Remote::Nft)),
        b4(t, &listen),
        f5,
        b6(t, &obs(a, Remote::UnattendedUnit), &obs(a, Remote::AutoUpgrades)),
        b7(t, a),
        b8(t, a),
        b9(ctx, host, &listen),
    ];
    (fs, bins)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nft_drop_detection() {
        let drop = "table inet filter {\n\tchain input {\n\t\ttype filter hook input priority filter; policy drop;\n\t}\n}\n";
        assert!(nft_has_input_drop(drop));
        let accept = "table inet filter {\n\tchain input {\n\t\ttype filter hook input priority filter; policy accept;\n\t}\n\tchain forward {\n\t\ttype filter hook forward priority filter; policy drop;\n\t}\n}\n";
        assert!(!nft_has_input_drop(accept));
        assert!(!nft_has_input_drop(""));
    }
}
