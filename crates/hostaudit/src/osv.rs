//! OSV advisories for B5 (canon §3.3 B5, CONTRACT §5): feed fetch with a
//! per-ecosystem cache, dpkg version ordering, source-package matching.
//!
//! A failed fetch is NEVER an empty advisory list: it falls back to a fresh
//! enough cache, otherwise it is `Err` (→ B5 undetermined).

use crate::config::Ctx;
use harness_core::verdict::Required;
use serde_json::Value;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Implementation-defined real feed: the OSV GCS data dump for the ecosystem
/// (a zip of one JSON file per advisory). Not exercised by the test suite.
pub const DEFAULT_URL: &str =
    "https://osv-vulnerabilities.storage.googleapis.com/{ecosystem}/all.zip";

// ───────────────────────────── dpkg ordering ───────────────────────────────

fn order(c: Option<char>) -> i32 {
    match c {
        None => 0,
        Some('~') => -1,
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_ascii_alphabetic() => c as i32,
        Some(c) => c as i32 + 256,
    }
}

/// dpkg's `verrevcmp` on one part (upstream or revision).
fn verrevcmp(a: &str, b: &str) -> Ordering {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() || j < b.len() {
        let mut first_diff = 0i32;
        while (i < a.len() && !a[i].is_ascii_digit()) || (j < b.len() && !b[j].is_ascii_digit()) {
            let ac = order(a.get(i).copied().filter(|c| !c.is_ascii_digit()));
            let bc = order(b.get(j).copied().filter(|c| !c.is_ascii_digit()));
            if ac != bc {
                return ac.cmp(&bc);
            }
            if i < a.len() && !a[i].is_ascii_digit() {
                i += 1;
            }
            if j < b.len() && !b[j].is_ascii_digit() {
                j += 1;
            }
        }
        while i < a.len() && a[i] == '0' {
            i += 1;
        }
        while j < b.len() && b[j] == '0' {
            j += 1;
        }
        while i < a.len() && a[i].is_ascii_digit() && j < b.len() && b[j].is_ascii_digit() {
            if first_diff == 0 {
                first_diff = a[i] as i32 - b[j] as i32;
            }
            i += 1;
            j += 1;
        }
        if i < a.len() && a[i].is_ascii_digit() {
            return Ordering::Greater;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            return Ordering::Less;
        }
        if first_diff != 0 {
            return first_diff.cmp(&0);
        }
    }
    Ordering::Equal
}

struct DebVer<'a> {
    epoch: u64,
    upstream: &'a str,
    revision: &'a str,
}

fn parse_deb(v: &str) -> DebVer<'_> {
    let v = v.trim();
    let (epoch, rest) = match v.split_once(':') {
        Some((e, r)) if !e.is_empty() && e.chars().all(|c| c.is_ascii_digit()) => {
            (e.parse::<u64>().unwrap_or(u64::MAX), r)
        }
        _ => (0, v),
    };
    let (upstream, revision) = match rest.rsplit_once('-') {
        Some((u, r)) => (u, r),
        None => (rest, ""),
    };
    DebVer {
        epoch,
        upstream,
        revision,
    }
}

/// Compare two Debian versions with dpkg semantics.
pub fn dpkg_cmp(a: &str, b: &str) -> Ordering {
    let (x, y) = (parse_deb(a), parse_deb(b));
    x.epoch
        .cmp(&y.epoch)
        .then_with(|| verrevcmp(x.upstream, y.upstream))
        .then_with(|| verrevcmp(x.revision, y.revision))
}

// ───────────────────────────── ecosystems ──────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Eco {
    Ubuntu(String),
    Debian(String),
}

impl Eco {
    /// The base ecosystem name: the cache key and the `{ecosystem}` URL value.
    pub fn base(&self) -> &'static str {
        match self {
            Eco::Ubuntu(_) => "Ubuntu",
            Eco::Debian(_) => "Debian",
        }
    }

    pub fn label(&self) -> String {
        match self {
            Eco::Ubuntu(v) => format!("Ubuntu:{v}"),
            Eco::Debian(v) => format!("Debian:{v}"),
        }
    }

    /// CONTRACT §5: `Ubuntu:<v>` or `Ubuntu:<v>:…` (not `Ubuntu:Pro:…`);
    /// `Debian:<v>`.
    pub fn matches(&self, record_eco: &str) -> bool {
        match self {
            Eco::Ubuntu(v) => {
                let want = format!("Ubuntu:{v}");
                record_eco == want || record_eco.starts_with(&format!("{want}:"))
            }
            Eco::Debian(v) => record_eco == format!("Debian:{v}"),
        }
    }
}

/// From W1 (`/etc/os-release`). `Err` = unsupported or unparseable.
pub fn eco_from_os_release(text: &str) -> Result<Eco, String> {
    let mut id = None;
    let mut ver = None;
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            match k.trim() {
                "ID" => id = Some(v),
                "VERSION_ID" => ver = Some(v),
                _ => {}
            }
        }
    }
    let id = id.ok_or("os-release has no ID")?;
    match id.as_str() {
        "ubuntu" | "debian" => {}
        other => {
            return Err(format!(
                "ID={other} is not Ubuntu/Debian; no OSV ecosystem is defined for it (ID_LIKE is not consulted)"
            ))
        }
    }
    let ver = ver
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("os-release ID={id} has no VERSION_ID"))?;
    Ok(if id == "ubuntu" {
        Eco::Ubuntu(ver)
    } else {
        Eco::Debian(ver)
    })
}

// ───────────────────────────── feed / cache ────────────────────────────────

pub struct Feed {
    pub records: Vec<Value>,
    pub fetched_at: i64,
    /// Set when the data came from the cache because the fetch failed.
    pub cache_note: Option<String>,
}

fn cache_path(ctx: &Ctx, base: &str) -> PathBuf {
    ctx.state_dir()
        .join("osv-cache")
        .join(format!("{base}.json"))
}

fn parse_records(body: &str) -> Result<Vec<Value>, String> {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Array(a)) => Ok(a),
        Ok(_) => Err("advisory feed is not a JSON array".to_string()),
        Err(e) => Err(format!("advisory feed is not JSON: {e}")),
    }
}

fn run_ok(c: &mut std::process::Command, secs: u64) -> Result<String, String> {
    match harness_core::boundary::run_with_timeout(c, Duration::from_secs(secs)).require() {
        Required::Determined(o) => match o.stdout_on_success().require() {
            Required::Determined(s) => Ok(s),
            Required::Blocked(why) => Err(why.to_string()),
        },
        Required::Blocked(why) => Err(why.to_string()),
    }
}

/// https fetch via `curl -fsS` (exit status decides); `.zip` URLs are the OSV
/// dump format and are streamed through `unzip -p`.
fn fetch_http(ctx: &Ctx, url: &str) -> Result<Vec<Value>, String> {
    let dir = ctx.state_dir().join("osv-cache");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".download-{}", std::process::id()));
    let mut curl = std::process::Command::new("curl");
    curl.args(["-fsSL", "--max-time", "600", "-o"])
        .arg(&tmp)
        .arg(url)
        .stdin(std::process::Stdio::null());
    let res = run_ok(&mut curl, 660).and_then(|_| {
        if url.ends_with(".zip") {
            let mut unzip = std::process::Command::new("unzip");
            unzip.arg("-p").arg(&tmp).stdin(std::process::Stdio::null());
            let body = run_ok(&mut unzip, 600)?;
            let mut out = Vec::new();
            for v in serde_json::Deserializer::from_str(&body).into_iter::<Value>() {
                out.push(v.map_err(|e| format!("advisory dump entry is not JSON: {e}"))?);
            }
            Ok(out)
        } else {
            match harness_core::boundary::read_to_string(&tmp).require() {
                Required::Determined(Some(t)) => parse_records(&t),
                Required::Determined(None) => Err("curl produced no file".to_string()),
                Required::Blocked(why) => Err(why.to_string()),
            }
        }
    });
    let _ = std::fs::remove_file(&tmp);
    res
}

fn fetch(ctx: &Ctx, url: &str) -> Result<Vec<Value>, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return match harness_core::boundary::read_to_string(Path::new(path)).require() {
            Required::Determined(Some(t)) => parse_records(&t),
            Required::Determined(None) => Err(format!("advisory feed {path} does not exist")),
            Required::Blocked(why) => Err(why.to_string()),
        };
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        return fetch_http(ctx, url);
    }
    Err(format!("unsupported advisory feed URL {url:?}"))
}

/// Keep only what matching needs, so the cache stays small.
fn slim(records: Vec<Value>, base: &str) -> Vec<Value> {
    records
        .into_iter()
        .filter(|r| {
            r["affected"].as_array().is_some_and(|a| {
                a.iter().any(|x| {
                    x["package"]["ecosystem"]
                        .as_str()
                        .is_some_and(|e| e == base || e.starts_with(&format!("{base}:")))
                })
            })
        })
        .map(|r| {
            serde_json::json!({
                "id": r["id"],
                "aliases": r["aliases"],
                "severity": r["severity"],
                "affected": r["affected"],
            })
        })
        .collect()
}

fn write_cache(ctx: &Ctx, base: &str, records: &[Value]) -> Result<(), String> {
    let p = cache_path(ctx, base);
    let dir = p.parent().ok_or("cache path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let body = serde_json::json!({"ecosystem": base, "fetched_at": ctx.now, "records": records});
    let tmp = dir.join(format!(".{base}.json.{}", std::process::id()));
    std::fs::write(&tmp, body.to_string())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("cannot replace {}: {e}", p.display()))
}

fn read_cache(ctx: &Ctx, base: &str) -> Result<Feed, String> {
    let p = cache_path(ctx, base);
    let text = match harness_core::boundary::read_to_string(&p).require() {
        Required::Determined(Some(t)) => t,
        Required::Determined(None) => return Err("no cached advisories".to_string()),
        Required::Blocked(why) => return Err(format!("cache unreadable: {why}")),
    };
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("cache corrupt: {e}"))?;
    if v["ecosystem"].as_str() != Some(base) {
        return Err("cache is for another ecosystem".to_string());
    }
    let fetched_at = v["fetched_at"].as_i64().ok_or("cache has no fetched_at")?;
    let records = v["records"]
        .as_array()
        .ok_or("cache has no records array")?
        .clone();
    let age = ctx.now - fetched_at;
    if age < 0 || age > ctx.osv_max_age_secs {
        return Err(format!(
            "cached advisories are {}h old (limit {}h)",
            age / 3600,
            ctx.osv_max_age_secs / 3600
        ));
    }
    Ok(Feed {
        records,
        fetched_at,
        cache_note: Some(format!(
            "advisory fetch failed; using cache fetched at {fetched_at}"
        )),
    })
}

/// Load the advisory data for `base` ("Ubuntu" / "Debian").
pub fn load(ctx: &Ctx, base: &str) -> Result<Feed, String> {
    let url = ctx
        .osv_url
        .clone()
        .unwrap_or_else(|| DEFAULT_URL.to_string())
        .replace("{ecosystem}", base);
    match fetch(ctx, &url) {
        Ok(records) => {
            let records = slim(records, base);
            if let Err(e) = write_cache(ctx, base, &records) {
                crate::finding::print_err(&format!("hostaudit: OSV cache not written: {e}"));
            }
            Ok(Feed {
                records,
                fetched_at: ctx.now,
                cache_note: None,
            })
        }
        Err(fetch_err) => read_cache(ctx, base)
            .map_err(|cache_err| format!("advisory fetch failed ({fetch_err}); {cache_err}")),
    }
}

// ───────────────────────────── matching ────────────────────────────────────

/// One installed package (W2 line).
#[derive(Clone, Debug)]
pub struct Pkg {
    pub name: String,
    pub version: String,
    pub source: String,
    pub source_version: String,
}

/// `${db:Status-Abbrev}` → installed? (CONTRACT §12.11 item 1): two ASCII
/// letters `<want><state>` plus an optional trailing space; installed iff the
/// state is `i`. Anything else is `None` (unreadable install state).
fn installed(status: &str) -> Option<bool> {
    let b = status.as_bytes();
    let ok = matches!(b.len(), 2 | 3)
        && b[0].is_ascii_alphabetic()
        && b[1].is_ascii_alphabetic()
        && b.get(2).is_none_or(|c| *c == b' ');
    ok.then(|| b[1] == b'i')
}

/// Parse W2 (`status\tpackage\tversion\tsource\tsource version`). Only
/// installed rows are returned (`rc` and every other state is skipped); a
/// malformed line, an unreadable status, or an installed row without a
/// version is `Err` (never "not installed").
pub fn parse_dpkg(text: &str) -> Result<Vec<Pkg>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let all: Vec<&str> = line.split('\t').collect();
        if all.len() != 5 {
            return Err(format!(
                "dpkg-query line {} has {} fields, expected 5",
                i + 1,
                all.len()
            ));
        }
        match installed(all[0]) {
            None => {
                return Err(format!(
                    "dpkg-query line {} has an unreadable install status {:?}",
                    i + 1,
                    all[0]
                ))
            }
            Some(false) => continue,
            Some(true) => {}
        }
        let f = &all[1..];
        if f[1].is_empty() {
            return Err(format!(
                "dpkg-query line {}: installed package {} has no version",
                i + 1,
                f[0]
            ));
        }
        let source = if f[2].is_empty() { f[0] } else { f[2] };
        let source_version = if f[3].is_empty() { f[1] } else { f[3] };
        out.push(Pkg {
            name: f[0].to_string(),
            version: f[1].to_string(),
            source: source.to_string(),
            source_version: source_version.to_string(),
        });
    }
    Ok(out)
}

/// The lowest ECOSYSTEM `fixed` event above `v` (the version that fixes it).
fn fixed_above(aff: &Value, v: &str) -> Option<String> {
    aff["ranges"]
        .as_array()?
        .iter()
        .filter(|r| r["type"].as_str() == Some("ECOSYSTEM"))
        .filter_map(|r| r["events"].as_array())
        .flatten()
        .filter_map(|e| e["fixed"].as_str())
        .filter(|f| dpkg_cmp(v, f) == Ordering::Less)
        .min_by(|a, b| dpkg_cmp(a, b))
        .map(str::to_string)
}

/// `None` = not affected; `Some(fixed)` = affected, `fixed` = the fixing
/// version if the record names one.
fn affected_by(aff: &Value, v: &str) -> Option<Option<String>> {
    if affected_in_ranges(aff, v)
        || aff["versions"]
            .as_array()
            .is_some_and(|vs| vs.iter().any(|x| x.as_str() == Some(v)))
    {
        return Some(fixed_above(aff, v));
    }
    None
}

fn affected_in_ranges(aff: &Value, v: &str) -> bool {
    let Some(ranges) = aff["ranges"].as_array() else {
        return false;
    };
    for r in ranges {
        if r["type"].as_str() != Some("ECOSYSTEM") {
            continue;
        }
        let Some(events) = r["events"].as_array() else {
            continue;
        };
        // Walk events in order, building [introduced, fixed) / [introduced,
        // last_affected] intervals; an interval left open is still affected.
        let mut start: Option<&str> = None;
        let ge_start = |s: &str| s == "0" || dpkg_cmp(v, s) != Ordering::Less;
        for e in events {
            if let Some(i) = e["introduced"].as_str() {
                start = Some(i);
            } else if let Some(fx) = e["fixed"].as_str() {
                if let Some(s) = start.take() {
                    if ge_start(s) && dpkg_cmp(v, fx) == Ordering::Less {
                        return true;
                    }
                }
            } else if let Some(la) = e["last_affected"].as_str() {
                if let Some(s) = start.take() {
                    if ge_start(s) && dpkg_cmp(v, la) != Ordering::Greater {
                        return true;
                    }
                }
            }
        }
        if let Some(s) = start {
            if ge_start(s) {
                return true;
            }
        }
    }
    false
}

pub struct Hit {
    pub pkg: Pkg,
    pub advisory: String,
    /// The OSV record (id / aliases / severity are read from it, canon v1.1 §3.1).
    pub record: Value,
    /// The fixing version, `None` when the record has none (canon v1.1 §3.2: 回避策).
    pub fixed: Option<String>,
}

/// `(records for this ecosystem, hits)`.
pub fn evaluate(feed: &Feed, eco: &Eco, pkgs: &[Pkg]) -> (usize, Vec<Hit>) {
    let mut count = 0;
    let mut hits = Vec::new();
    for rec in &feed.records {
        let id = rec["id"].as_str().unwrap_or("<no id>").to_string();
        let Some(affected) = rec["affected"].as_array() else {
            continue;
        };
        let mine: Vec<&Value> = affected
            .iter()
            .filter(|a| {
                a["package"]["ecosystem"]
                    .as_str()
                    .is_some_and(|e| eco.matches(e))
            })
            .collect();
        if mine.is_empty() {
            continue;
        }
        count += 1;
        for a in mine {
            let Some(name) = a["package"]["name"].as_str() else {
                continue;
            };
            for p in pkgs.iter().filter(|p| p.source == name) {
                let Some(fixed) = affected_by(a, &p.source_version) else {
                    continue;
                };
                if !hits
                    .iter()
                    .any(|h: &Hit| h.pkg.name == p.name && h.advisory == id)
                {
                    hits.push(Hit {
                        pkg: p.clone(),
                        advisory: id.clone(),
                        record: rec.clone(),
                        fixed,
                    });
                }
            }
        }
    }
    (count, hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpkg_ordering() {
        use Ordering::*;
        assert_eq!(dpkg_cmp("1:2.0-1", "1.9-1"), Greater);
        assert_eq!(dpkg_cmp("2.0~rc1-1", "2.0-1"), Less);
        assert_eq!(
            dpkg_cmp("1:8.9p1-3ubuntu0.10", "1:8.9p1-3ubuntu0.9"),
            Greater
        );
        assert_eq!(dpkg_cmp("1.0", "1.0"), Equal);
        assert_eq!(dpkg_cmp("1.0~", "1.0"), Less);
        assert_eq!(dpkg_cmp("1.0a", "1.0"), Greater);
        assert_eq!(dpkg_cmp("1.0-1", "1.0"), Greater);
        assert_eq!(dpkg_cmp("3.0.11-1~deb12u1", "3.0.11-1~deb12u2"), Less);
    }

    #[test]
    fn ecosystems() {
        let u = Eco::Ubuntu("22.04".into());
        assert!(u.matches("Ubuntu:22.04:LTS"));
        assert!(u.matches("Ubuntu:22.04"));
        assert!(!u.matches("Ubuntu:Pro:22.04:LTS"));
        assert!(!u.matches("Ubuntu:22.040"));
    }
}
