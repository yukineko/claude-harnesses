//! Per-CVE evaluation and the deterministic P0–P3 priority for B5 / K2
//! (canon v1.1 §3, CONTRACT §12.5).

use crate::osv::Hit;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// yes / no / unknown — `unknown` is never collapsed into `no`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tri {
    Yes,
    No,
    Unknown,
}

impl Tri {
    pub fn label(self) -> &'static str {
        match self {
            Tri::Yes => "yes",
            Tri::No => "no",
            Tri::Unknown => "unknown",
        }
    }
}

/// KEV lookup: `Err` = the catalogue could not be obtained (`kev: unknown`).
pub type KevSet<'a> = Result<&'a BTreeSet<String>, &'a str>;

/// Which source packages have an executable listening on 0.0.0.0 / [::] / *
/// (canon v1.1 §3.1 外部公開).
#[derive(Clone, Debug, Default)]
pub struct Exposure {
    pub yes: BTreeSet<String>,
    /// `Some(why)` = some external listener's owner could not be established,
    /// or W12 failed: every source not in `yes` is `unknown`, not `no`.
    pub unknown: Option<String>,
}

impl Exposure {
    pub fn of(&self, source: &str) -> Tri {
        if self.yes.contains(source) {
            Tri::Yes
        } else if self.unknown.is_some() {
            Tri::Unknown
        } else {
            Tri::No
        }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub cve: Option<String>,
    pub package: String,
    pub severity: &'static str,
    pub kev: Tri,
    pub fixed: Option<String>,
    /// W14 candidate version of the package: `Ok(None)` = apt offers none,
    /// `Err(why)` = W14 could not be observed (reported as `unknown`).
    pub candidate: Result<Option<String>, String>,
    pub exposed: Tri,
    pub priority: u8,
}

/// `CVE-<year>-<n>` found inside `s` (`UBUNTU-CVE-2024-1234` → `CVE-2024-1234`).
fn cve_in(s: &str) -> Option<String> {
    let pos = s.find("CVE-")?;
    let rest = &s[pos..];
    let mut parts = rest.splitn(3, '-');
    let (_, y, n) = (parts.next()?, parts.next()?, parts.next()?);
    let n: String = n.chars().take_while(|c| c.is_ascii_digit()).collect();
    (y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
        .then(|| format!("CVE-{y}-{n}"))
}

/// Every CVE id of a record: from `id`, then `aliases` (in order, deduped).
fn cve_ids(rec: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: &str| {
        if let Some(c) = cve_in(s) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    };
    if let Some(id) = rec["id"].as_str() {
        push(id);
    }
    if let Some(a) = rec["aliases"].as_array() {
        for x in a.iter().filter_map(Value::as_str) {
            push(x);
        }
    }
    out
}

/// CVSS v3.x base score from a vector (`CVSS:3.1/AV:N/...`); `None` if the
/// vector is incomplete or malformed.
pub fn cvss3_base(vector: &str) -> Option<f64> {
    let mut m: BTreeMap<&str, &str> = BTreeMap::new();
    for part in vector.split('/') {
        if let Some((k, v)) = part.split_once(':') {
            m.insert(k, v);
        }
    }
    if !m.get("CVSS").is_some_and(|v| v.starts_with('3')) {
        return None;
    }
    let scope_changed = match *m.get("S")? {
        "U" => false,
        "C" => true,
        _ => return None,
    };
    let av = match *m.get("AV")? {
        "N" => 0.85,
        "A" => 0.62,
        "L" => 0.55,
        "P" => 0.2,
        _ => return None,
    };
    let ac = match *m.get("AC")? {
        "L" => 0.77,
        "H" => 0.44,
        _ => return None,
    };
    let pr = match (*m.get("PR")?, scope_changed) {
        ("N", _) => 0.85,
        ("L", false) => 0.62,
        ("L", true) => 0.68,
        ("H", false) => 0.27,
        ("H", true) => 0.5,
        _ => return None,
    };
    let ui = match *m.get("UI")? {
        "N" => 0.85,
        "R" => 0.62,
        _ => return None,
    };
    let cia = |k: &str| -> Option<f64> {
        match *m.get(k)? {
            "H" => Some(0.56),
            "L" => Some(0.22),
            "N" => Some(0.0),
            _ => None,
        }
    };
    let (c, i, a) = (cia("C")?, cia("I")?, cia("A")?);
    let iss = 1.0 - (1.0 - c) * (1.0 - i) * (1.0 - a);
    let impact = if scope_changed {
        7.52 * (iss - 0.029) - 3.25 * (iss - 0.02f64).powi(15)
    } else {
        6.42 * iss
    };
    let expl = 8.22 * av * ac * pr * ui;
    if impact <= 0.0 {
        return Some(0.0);
    }
    let raw = if scope_changed {
        (1.08 * (impact + expl)).min(10.0)
    } else {
        (impact + expl).min(10.0)
    };
    Some(roundup(raw))
}

/// CVSS v3.1 Roundup.
fn roundup(x: f64) -> f64 {
    let i = (x * 100_000.0).round() as i64;
    if i % 10_000 == 0 {
        i as f64 / 100_000.0
    } else {
        ((i / 10_000) + 1) as f64 / 10.0
    }
}

/// Canon v1.1 §3.1: Ubuntu score if present, else CVSS_V3 base score, else unknown.
pub fn severity(rec: &Value) -> &'static str {
    let Some(sev) = rec["severity"].as_array() else {
        return "unknown";
    };
    for s in sev {
        if s["type"].as_str() == Some("Ubuntu") {
            if let Some(score) = s["score"].as_str() {
                match score.trim().to_ascii_lowercase().as_str() {
                    "negligible" => return "negligible",
                    "low" => return "low",
                    "medium" => return "medium",
                    "high" => return "high",
                    "critical" => return "critical",
                    _ => {}
                }
            }
        }
    }
    for s in sev {
        if s["type"].as_str() == Some("CVSS_V3") {
            if let Some(score) = s["score"].as_str().and_then(cvss3_base) {
                return if score >= 9.0 {
                    "critical"
                } else if score >= 7.0 {
                    "high"
                } else if score >= 4.0 {
                    "medium"
                } else if score > 0.0 {
                    "low"
                } else {
                    "unknown"
                };
            }
        }
    }
    "unknown"
}

/// Canon v1.1 §3.2 verbatim. `kev` unknown never yields P0.
pub fn priority(severity: &str, kev: Tri, exposed: Tri) -> u8 {
    if kev == Tri::Yes {
        return 0;
    }
    match severity {
        "critical" | "high" => {
            if exposed == Tri::No {
                2
            } else {
                1
            }
        }
        "low" | "negligible" => 3,
        // medium and unknown
        _ => 2,
    }
}

/// Evaluate the hits. `candidates` = W14 `Inst` candidate per binary package
/// (`Err` = W14 unobservable; every item's candidate is then unknown).
pub fn evaluate(
    hits: &[Hit],
    kev: KevSet<'_>,
    exposure: &Exposure,
    candidates: &Result<BTreeMap<String, Result<String, String>>, String>,
) -> Vec<Item> {
    let mut items: Vec<Item> = hits
        .iter()
        .map(|h| {
            let ids = cve_ids(&h.record);
            let kev_t = match kev {
                Ok(set) => {
                    if ids.iter().any(|c| set.contains(c)) {
                        Tri::Yes
                    } else {
                        Tri::No
                    }
                }
                Err(_) => Tri::Unknown,
            };
            let sev = severity(&h.record);
            let exposed = exposure.of(&h.pkg.source);
            Item {
                id: h.advisory.clone(),
                cve: ids.first().cloned(),
                package: h.pkg.name.clone(),
                severity: sev,
                kev: kev_t,
                fixed: h.fixed.clone(),
                candidate: match candidates {
                    Err(e) => Err(e.clone()),
                    Ok(c) => c.get(&h.pkg.name).cloned().transpose(),
                },
                exposed,
                priority: priority(sev, kev_t, exposed),
            }
        })
        .collect();
    items.sort_by_key(|i| i.priority);
    items
}

pub fn to_json(items: &[Item]) -> (Value, Value) {
    let cves: Vec<Value> = items
        .iter()
        .map(|i| {
            let mut v = json!({
                "id": i.id,
                "cve": i.cve,
                "package": i.package,
                "severity": i.severity,
                "kev": i.kev.label(),
                "fixed": i.fixed.clone().unwrap_or_else(|| "unknown".to_string()),
                "exposed": i.exposed.label(),
                "priority": format!("P{}", i.priority),
            });
            match &i.candidate {
                Ok(Some(c)) => v["candidate"] = json!(c),
                Ok(None) => {}
                Err(_) => v["candidate"] = json!("unknown"),
            }
            v
        })
        .collect();
    let mut counts = serde_json::Map::new();
    for p in 0..4u8 {
        let n = items.iter().filter(|i| i.priority == p).count();
        counts.insert(format!("P{p}"), json!(n));
    }
    (Value::Array(cves), Value::Object(counts))
}

/// Evidence lines: priority counts, one line per CVE, and `KEV 不明` when the
/// catalogue was not available (canon v1.1 §3.2: stated, never read as "no").
pub fn evidence(items: &[Item], kev: KevSet<'_>) -> Vec<String> {
    let mut ev = vec![format!(
        "priority: P0 {} / P1 {} / P2 {} / P3 {}",
        items.iter().filter(|i| i.priority == 0).count(),
        items.iter().filter(|i| i.priority == 1).count(),
        items.iter().filter(|i| i.priority == 2).count(),
        items.iter().filter(|i| i.priority == 3).count(),
    )];
    if let Err(why) = kev {
        ev.push(format!(
            "KEV 不明 (CISA KEV unavailable: {why}) — P0 could not be judged; priorities below are from the rest of the table"
        ));
    }
    for i in items {
        ev.push(format!(
            "P{} {} {} {}: severity {}, kev {}, exposed {}, fixed {}",
            i.priority,
            i.cve.as_deref().unwrap_or("-"),
            i.id,
            i.package,
            i.severity,
            i.kev.label(),
            i.exposed.label(),
            i.fixed.as_deref().unwrap_or("unknown")
        ));
    }
    ev
}

/// Fix text: one `apt install --only-upgrade` for every fixable package, and
/// a `回避策` list for CVEs without a fixed version (canon v1.1 §3.2).
pub fn fix_text(items: &[Item], prefix: &str, suffix: &str) -> String {
    let mut fixable: Vec<&str> = items
        .iter()
        .filter(|i| i.fixed.is_some())
        .map(|i| i.package.as_str())
        .collect();
    fixable.sort();
    fixable.dedup();
    let mut lines = Vec::new();
    if !prefix.is_empty() {
        lines.push(prefix.to_string());
    }
    if !fixable.is_empty() {
        lines.push(format!(
            "sudo apt update && sudo apt install --only-upgrade {}{suffix}",
            fixable.join(" ")
        ));
    }
    let nofix: Vec<String> = items
        .iter()
        .filter(|i| i.fixed.is_none())
        .map(|i| format!("{} ({})", i.cve.as_deref().unwrap_or(&i.id), i.package))
        .collect();
    if !nofix.is_empty() {
        lines.push(format!(
            "回避策が必要 (修正版なし): {} — 対象サービスの停止・bind を 127.0.0.1 に変更・FW で接続元を制限",
            nofix.join(", ")
        ));
    }
    let mut ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    ids.sort();
    ids.dedup();
    lines.push(format!("# advisories: {}", ids.join(" ")));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cvss_examples() {
        assert_eq!(
            cvss3_base("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"),
            Some(9.8)
        );
        assert_eq!(
            cvss3_base("CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:U/C:H/I:H/A:H"),
            Some(8.8)
        );
        assert_eq!(
            cvss3_base("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:L/I:N/A:N"),
            Some(5.3)
        );
        assert_eq!(
            cvss3_base("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:H/A:H"),
            Some(10.0)
        );
        assert_eq!(cvss3_base("garbage"), None);
    }

    #[test]
    fn priorities() {
        assert_eq!(priority("low", Tri::Yes, Tri::No), 0);
        assert_eq!(priority("high", Tri::No, Tri::Unknown), 1);
        assert_eq!(priority("high", Tri::Unknown, Tri::No), 2);
        assert_eq!(priority("unknown", Tri::No, Tri::No), 2);
        assert_eq!(priority("negligible", Tri::Unknown, Tri::Yes), 3);
    }

    #[test]
    fn cve_extraction() {
        assert_eq!(
            cve_in("UBUNTU-CVE-2024-1234").as_deref(),
            Some("CVE-2024-1234")
        );
        assert_eq!(cve_in("USN-6001-1"), None);
    }
}
