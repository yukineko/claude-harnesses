//! `hostaudit audit`: per-host OS dispatch and the full rule set
//! (canon v1 §3, v1.1 §1–§7; CONTRACT §4.3, §12.2).
//!
//! Every selected host gets exactly one finding per rule in [`ALL_RULES`]
//! (K6 is parked and emits nothing). A rule that could not be observed is
//! `undetermined` with a reason — never omitted, never clean.

use crate::baseline;
use crate::baseline::FeedMemo;
use crate::config::Ctx;
use crate::cve::Exposure;
use crate::feeds::{self, Cycle, Fetched};
use crate::finding::{self, Finding};
use crate::inventory::{self, Host};
use crate::linux11;
use crate::ssh::{self, Obs, Remote};
use crate::windows;
use std::collections::{BTreeMap, BTreeSet};

/// The rules every host gets a finding for, in output order.
pub const ALL_RULES: [&str; 27] = [
    "B1", "B2", "B3", "B4", "B5", "B6", "B7", "B8", "B9", "S1", "S2", "S3", "S5", "S6", "S7", "S8",
    "S9", "K1", "K2", "K3", "K4", "K5", "WN1", "WN2", "WN3", "WN4", "WN5",
];

pub const WN_RULES: [&str; 5] = ["WN1", "WN2", "WN3", "WN4", "WN5"];

/// The remote answers of one host, run on first use and memoised (each
/// whitelist command is sent at most once per host).
pub struct Probe<'a> {
    ctx: &'a Ctx,
    host: &'a Host,
    pub answers: BTreeMap<Remote, Obs>,
}

impl<'a> Probe<'a> {
    pub fn new(ctx: &'a Ctx, host: &'a Host) -> Probe<'a> {
        Probe {
            ctx,
            host,
            answers: BTreeMap::new(),
        }
    }

    pub fn get(&mut self, r: Remote) -> Obs {
        if let Some(o) = self.answers.get(&r) {
            return o.clone();
        }
        let o = ssh::run(self.ctx, self.host, &r);
        self.answers.insert(r, o.clone());
        o
    }
}

/// Feed data shared by every host of one run.
#[derive(Default)]
pub struct Memo {
    pub osv: FeedMemo,
    /// CISA KEV, loaded on first use (only when some CVE matched).
    pub kev: Option<Result<Fetched<BTreeSet<String>>, String>>,
    eol: BTreeMap<String, Result<Fetched<Vec<Cycle>>, String>>,
}

impl Memo {
    /// endoflife.date data of `slug`, fetched once per run.
    pub fn eol(&mut self, ctx: &Ctx, slug: &str) -> &Result<Fetched<Vec<Cycle>>, String> {
        self.eol
            .entry(slug.to_string())
            .or_insert_with(|| feeds::load_eol(ctx, slug))
    }
}

/// Exposure and W14 candidates for the CVE evaluation (B5 / K2).
pub struct CveCtx {
    pub exposure: Exposure,
    /// `Err(why)` when W14 was not observed (candidates unknown).
    pub candidates: Result<BTreeMap<String, String>, String>,
}

/// `/etc/os-release` fields.
#[derive(Clone, Debug)]
pub struct OsInfo {
    pub id: String,
    pub version_id: String,
    pub codename: Option<String>,
}

impl OsInfo {
    pub fn parse(text: &str) -> OsInfo {
        let mut id = String::new();
        let mut version_id = String::new();
        let mut codename = None;
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                match k.trim() {
                    "ID" => id = v,
                    "VERSION_ID" => version_id = v,
                    "VERSION_CODENAME" if !v.is_empty() => codename = Some(v),
                    _ => {}
                }
            }
        }
        OsInfo {
            id,
            version_id,
            codename,
        }
    }

    pub fn is_ubuntu(&self) -> bool {
        self.id == "ubuntu"
    }

    /// Ubuntu or Debian: every v1.1 rule is evaluated (canon v1.1 §5).
    pub fn is_debian_family(&self) -> bool {
        matches!(self.id.as_str(), "ubuntu" | "debian")
    }
}

fn all_undetermined(t: &str, reason: &str) -> Vec<Finding> {
    ALL_RULES
        .iter()
        .map(|r| Finding::undetermined(r, t, vec![], reason))
        .collect()
}

fn order(mut fs: Vec<Finding>) -> Vec<Finding> {
    fs.sort_by_key(|f| {
        ALL_RULES
            .iter()
            .position(|r| *r == f.rule)
            .unwrap_or(ALL_RULES.len())
    });
    fs
}

/// All rules for one host.
pub fn audit_host(ctx: &Ctx, host: &Host, memo: &mut Memo) -> Vec<Finding> {
    let t = host.alias.as_str();
    let mut p = Probe::new(ctx, host);
    let w1 = p.get(Remote::OsRelease);
    let os_text = match &w1 {
        Obs::Out(s) => s.clone(),
        other => {
            // canon v1.1 §7: W1 failed → W0 decides whether this is Windows.
            let w1_why = match other {
                Obs::Undet(w) => w.clone(),
                o => format!("W1 unexpected {o:?}"),
            };
            return match p.get(Remote::WinVer) {
                Obs::Out(s) if s.contains("Microsoft Windows") => {
                    let mut fs = windows::evaluate(ctx, t, &mut p, memo);
                    for r in ALL_RULES.iter().filter(|r| !WN_RULES.contains(r)) {
                        fs.push(Finding::not_applicable(
                            r,
                            t,
                            vec![],
                            "Windows host (W0: Microsoft Windows): Linux rules do not apply",
                        ));
                    }
                    order(fs)
                }
                w0 => all_undetermined(
                    t,
                    &format!(
                        "OS を判定できない: W1 failed ({w1_why}) and W0 did not report Microsoft Windows ({})",
                        match w0 {
                            Obs::Out(s) => format!("output {:?}", s.trim()),
                            Obs::Undet(w) | Obs::Missing(w) => w,
                            o => format!("{o:?}"),
                        }
                    ),
                ),
            };
        }
    };
    let os = OsInfo::parse(&os_text);
    for r in Remote::V1_AFTER_OS {
        p.get(r);
    }
    let data = if os.is_debian_family() {
        Some(linux11::collect(&mut p, &os))
    } else {
        None
    };
    let cx = match &data {
        Some(d) => linux11::cve_ctx(d, &p.answers),
        None => CveCtx {
            exposure: Exposure {
                yes: BTreeSet::new(),
                unknown: Some("listening processes are not examined on this OS".into()),
            },
            candidates: Err("W14 is not run on this OS".into()),
        },
    };
    let (mut fs, b5_bins) = baseline::evaluate(ctx, host, &p.answers, memo, &cx);
    match &data {
        Some(d) => fs.extend(linux11::evaluate(
            ctx, t, &os, d, &p.answers, &fs, &b5_bins, &cx, memo,
        )),
        None => {
            fs.push(linux11::k1(ctx, t, &os, None, memo));
            let why = format!(
                "この OS の判定は未実装 (ID={:?}; canon v1.1 §5: Ubuntu / Debian 以外は K1 のみ判定)",
                os.id
            );
            for r in [
                "S1", "S2", "S3", "S5", "S6", "S7", "S8", "S9", "K2", "K3", "K4", "K5",
            ] {
                fs.push(Finding::undetermined(r, t, vec![], &why));
            }
        }
    }
    for r in WN_RULES {
        fs.push(Finding::not_applicable(
            r,
            t,
            vec![],
            "Linux host (W1 readable): Windows rules do not apply",
        ));
    }
    order(fs)
}

/// `hostaudit audit [--json]`.
pub fn run_audit(ctx: &Ctx, json: bool) -> i32 {
    let sel = match inventory::read_selection(ctx) {
        Ok(Some(s)) => s,
        Ok(None) => {
            finding::print_err(
                "hostaudit audit: refused — no hosts selected. Run `hostaudit select <alias>...` or `hostaudit select --all` first.",
            );
            return 3;
        }
        Err(e) => {
            finding::print_err(&format!(
                "hostaudit audit: refused — the selection cannot be read ({e}); no host was contacted"
            ));
            return 3;
        }
    };
    if sel.host.is_empty() {
        finding::print_err(
            "hostaudit audit: refused — the selection is empty; no host was contacted",
        );
        return 3;
    }
    let mut memo = Memo::default();
    let mut all = Vec::new();
    for host in &sel.host {
        all.extend(audit_host(ctx, host, &mut memo));
    }
    finding::finish(ctx, &all, json)
}
