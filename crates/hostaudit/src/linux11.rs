//! The v1.1 Linux rules S1–S3, S5–S9 and K1–K5 (canon v1.1 §2–§5, CONTRACT
//! §12.2, §12.5, §12.6, §12.9) for Ubuntu / Debian hosts, plus K1 for other
//! Linux distributions.
//!
//! Every rule yields exactly one finding. Anything that could not be
//! observed — ssh failure, timeout (`時間切れ`), tool missing, unparseable
//! output, an unsafe executable path that is never sent to ssh — makes the
//! dependent rule or item `undetermined`, never clean. For an unsafe W12b
//! path the dependent items are its CVE exposure (always `unknown`) and,
//! when its basename (` (deleted)` stripped) is a §4.1 product, its S8 item
//! (kept as `undetermined`, never dropped); a basename that is no §4.1
//! product has no S8 item at all (canon v1.1 §2 W22).

use crate::audit::{CveCtx, Memo, OsInfo, Probe};
use crate::baseline::{self, Answers, CveRule};
use crate::config::Ctx;
use crate::cve::{self, Exposure};
use crate::feeds::{self, Period};
use crate::finding::{Finding, Status};
use crate::osv::{self, Pkg};
use crate::products;
use crate::ssh::{Obs, Pid, Remote, SafePath, SourceName};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const S8_WARN_DAYS: i64 = 90;

// ───────────────────────────── observation ─────────────────────────────────

/// Who owns a listening executable (W13).
#[derive(Clone, Debug)]
pub enum Owner {
    /// binary package names from `dpkg -S`
    Dpkg(Vec<String>),
    NotDpkg,
}

/// What W12b / W13 established about one listening process.
#[derive(Clone, Debug)]
pub enum Exe {
    /// W12b failed.
    Unread(String),
    /// The path fails the W13 conditions; it was never sent to ssh.
    Unsafe(String),
    Path(SafePath, Result<Owner, String>),
}

#[derive(Clone, Debug)]
pub struct Listener {
    pub line: String,
    pub external: bool,
    /// `Err(raw)` = a `pid=` value that is not digits only (never sent).
    pub pids: Vec<Result<Pid, String>>,
}

/// Everything the v1.1 rules need beyond the raw answers.
pub struct Data {
    pub listeners: Result<Vec<Listener>, String>,
    pub exes: BTreeMap<Pid, Exe>,
    pub source_files: Vec<SourceName>,
}

fn obs(a: &Answers, r: &Remote) -> Obs {
    match a.get(r) {
        Some(o) => o.clone(),
        None => Obs::Undet(format!("{} was not run", r.id())),
    }
}

fn is_external(addr: &str) -> bool {
    matches!(addr, "0.0.0.0" | "[::]" | "*" | "::")
}

/// Parse W12 (`ss -H -tlnp`).
fn parse_listeners(text: &str) -> Result<Vec<Listener>, String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let local = line
            .split_whitespace()
            .nth(3)
            .ok_or_else(|| format!("`ss -H -tlnp` line has no local address column: {line:?}"))?;
        let (addr, _port) = local
            .rsplit_once(':')
            .ok_or_else(|| format!("local address {local:?} has no port"))?;
        let addr = addr.split('%').next().unwrap_or(addr);
        let mut pids = Vec::new();
        let mut rest = line;
        while let Some(pos) = rest.find("pid=") {
            let after = &rest[pos + 4..];
            let end = after.find([',', ')']).unwrap_or(after.len());
            let raw = &after[..end];
            pids.push(Pid::parse(raw).ok_or_else(|| raw.to_string()));
            rest = &after[end..];
        }
        out.push(Listener {
            line: line.trim().to_string(),
            external: is_external(addr),
            pids,
        });
    }
    Ok(out)
}

/// The binary packages named by `dpkg -S <path>` for exactly that path.
fn parse_owner(text: &str, path: &str) -> Result<Owner, String> {
    let suffix = format!(": {path}");
    for line in text.lines() {
        if let Some(pkgs) = line.trim_end().strip_suffix(&suffix) {
            let names: Vec<String> = pkgs
                .split(", ")
                .map(|p| p.split(':').next().unwrap_or(p).trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            if !names.is_empty() && !pkgs.starts_with("diversion") {
                return Ok(Owner::Dpkg(names));
            }
        }
    }
    Err(format!(
        "W13 output does not name an owner of {path}: {:?}",
        text.trim()
    ))
}

/// The §4.1 product of an (unsafe, never sent) W12b path by its basename,
/// with a trailing ` (deleted)` stripped (CONTRACT §12.10 item 3).
fn product_of_basename(raw: &str) -> Option<(&'static str, &'static str)> {
    let raw = raw.strip_suffix(" (deleted)").unwrap_or(raw);
    products::by_basename(raw.rsplit('/').next().unwrap_or(raw))
}

/// One W13 query: `Ok(None)` = `no path found`.
fn dpkg_query(p: &mut Probe<'_>, path: &SafePath) -> Result<Option<Owner>, String> {
    match p.get(Remote::DpkgOwner(path.clone())) {
        Obs::Absent => Ok(None),
        Obs::Out(t) => parse_owner(&t, path.as_str()).map(Some),
        Obs::Undet(w) | Obs::Missing(w) => Err(w),
        other => Err(format!("W13 unexpected {other:?}")),
    }
}

/// W13 with the usrmerge alias (CONTRACT §12.10 item 12): `no path found`
/// for `/usr/{bin,sbin,lib}/x` also asks about `/{bin,sbin,lib}/x`. Not
/// dpkg-managed only when both say `no path found`; a failed alias query is
/// ownership unknown.
fn owner_of(p: &mut Probe<'_>, path: &SafePath) -> Result<Owner, String> {
    if let Some(o) = dpkg_query(p, path)? {
        return Ok(o);
    }
    let Some(alias) = path.usrmerge_alias() else {
        return Ok(Owner::NotDpkg);
    };
    match dpkg_query(p, &alias) {
        Ok(Some(o)) => Ok(o),
        Ok(None) => Ok(Owner::NotDpkg),
        Err(w) => Err(format!(
            "{} is not known to dpkg and its usrmerge alias {} could not be queried: {w}",
            path.as_str(),
            alias.as_str()
        )),
    }
}

/// Run every v1.1 command for a Debian-family host (memoised in `p`).
pub fn collect(p: &mut Probe<'_>, os: &OsInfo) -> Data {
    let listeners = match p.get(Remote::ListenProcs) {
        Obs::Out(s) => parse_listeners(&s),
        Obs::Undet(w) | Obs::Missing(w) => Err(w),
        other => Err(format!("W12 unexpected {other:?}")),
    };
    let mut exes = BTreeMap::new();
    if let Ok(ls) = &listeners {
        let pids: BTreeSet<Pid> = ls
            .iter()
            .flat_map(|l| l.pids.iter().filter_map(|p| p.as_ref().ok().copied()))
            .collect();
        for pid in pids {
            let exe = match p.get(Remote::Exe(pid)) {
                Obs::Out(s) => {
                    let raw = s.trim().to_string();
                    match SafePath::parse(&raw) {
                        None => Exe::Unsafe(raw),
                        Some(path) => {
                            let owner = owner_of(p, &path);
                            Exe::Path(path, owner)
                        }
                    }
                }
                Obs::Undet(w) | Obs::Missing(w) => Exe::Unread(w),
                other => Exe::Unread(format!("W12b unexpected {other:?}")),
            };
            exes.insert(pid, exe);
        }
    }
    // W22: only non-dpkg executables whose basename is a monitored product.
    for exe in exes.values() {
        if let Exe::Path(path, Ok(Owner::NotDpkg)) = exe {
            if let Some((_, args)) = products::by_basename(path.basename()) {
                p.get(Remote::ExeVersion(path.clone(), args));
            }
        }
    }
    p.get(Remote::AptSim);
    if os.is_ubuntu() {
        p.get(Remote::ProSecurity);
    }
    p.get(Remote::NeedRestart);
    p.get(if os.is_ubuntu() {
        Remote::DistroInfoUbuntu
    } else {
        Remote::DistroInfoDebian
    });
    p.get(Remote::SourcesList);
    let mut source_files = Vec::new();
    if let Obs::Out(ls) = p.get(Remote::SourcesDir) {
        for name in ls.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(n) = SourceName::parse(name) {
                p.get(Remote::SourcesFile(n.clone()));
                source_files.push(n);
            }
        }
    }
    p.get(Remote::DpkgAudit);
    p.get(Remote::Holds);
    for i in 0..products::W21.len() {
        p.get(Remote::ProductVersion(i));
    }
    p.get(Remote::DockerImages);
    p.get(Remote::Uname);
    if os.is_ubuntu() {
        p.get(Remote::ProStatus);
    }
    Data {
        listeners,
        exes,
        source_files,
    }
}

fn dpkg_pkgs(a: &Answers) -> Result<Vec<Pkg>, String> {
    match obs(a, &Remote::DpkgList) {
        Obs::Out(s) => osv::parse_dpkg(&s),
        Obs::Undet(w) | Obs::Missing(w) => Err(w),
        other => Err(format!("W2 unexpected {other:?}")),
    }
}

/// One W14 `Inst` line.
struct Inst {
    name: String,
    /// `(candidate, from a -security origin)`; `Err` when the `( … )` origin
    /// group is missing, unterminated or empty (CONTRACT §12.10 item 9).
    origin: Result<(String, bool), String>,
    line: String,
}

impl Inst {
    fn security(&self) -> bool {
        self.origin.as_ref().is_ok_and(|(_, s)| *s)
    }
}

/// The `Inst` lines whose origin group could not be read, as a reason.
fn malformed<'a>(insts: impl Iterator<Item = &'a Inst>) -> Option<String> {
    let bad: Vec<String> = insts
        .filter_map(|i| i.origin.as_ref().err().cloned())
        .collect();
    (!bad.is_empty()).then(|| bad.join("; "))
}

fn inst_lines(a: &Answers) -> Result<Vec<Inst>, String> {
    let text = match obs(a, &Remote::AptSim) {
        Obs::Out(s) => s,
        Obs::Undet(w) | Obs::Missing(w) => return Err(w),
        other => return Err(format!("W14 unexpected {other:?}")),
    };
    let mut out = Vec::new();
    for line in text.lines().filter(|l| l.starts_with("Inst ")) {
        let Some(name) = line.split_whitespace().nth(1) else {
            return Err(format!("W14 `Inst` line without a package: {line:?}"));
        };
        let origin = match (line.find('('), line.rfind(')')) {
            (Some(o), Some(c)) if c > o => {
                let g = &line[o + 1..c];
                match g.split_whitespace().next() {
                    Some(cand) => Ok((cand.to_string(), g.contains("-security"))),
                    None => Err(format!(
                        "W14 `Inst` line with an empty origin group: {line:?}"
                    )),
                }
            }
            (Some(_), _) => Err(format!(
                "W14 `Inst` line with an unterminated origin group: {line:?}"
            )),
            (None, _) => Err(format!("W14 `Inst` line without an origin group: {line:?}")),
        };
        out.push(Inst {
            name: name.to_string(),
            origin,
            line: line.trim().to_string(),
        });
    }
    Ok(out)
}

/// Exposure (canon v1.1 §3.1) and W14 candidates for B5 / K2.
pub fn cve_ctx(d: &Data, a: &Answers) -> CveCtx {
    let mut yes = BTreeSet::new();
    let mut unknown: Vec<String> = Vec::new();
    match &d.listeners {
        Err(e) => unknown.push(format!("W12: {e}")),
        // CONTRACT §12.10 item 10: a reachable host always listens on ssh, so
        // an empty `ss` output is unobserved, not "nothing is exposed".
        Ok(ls) if ls.is_empty() => {
            unknown.push("W12 printed no listening socket (sshd itself is not shown)".into())
        }
        Ok(ls) => {
            let pkgs = dpkg_pkgs(a);
            for l in ls.iter().filter(|l| l.external) {
                if l.pids.is_empty() {
                    unknown.push(format!("no process shown for {:?}", l.line));
                }
                for pid in &l.pids {
                    let pid = match pid {
                        Ok(p) => p,
                        Err(raw) => {
                            unknown.push(format!("pid {raw:?} is not numeric"));
                            continue;
                        }
                    };
                    match d.exes.get(pid) {
                        None => unknown.push(format!("pid {pid:?} was not examined")),
                        Some(Exe::Unread(w)) => unknown.push(w.clone()),
                        Some(Exe::Unsafe(raw)) => unknown.push(format!(
                            "executable path {raw:?} is unsafe to pass to dpkg -S; owner unknown"
                        )),
                        Some(Exe::Path(_, Err(w))) => unknown.push(w.clone()),
                        Some(Exe::Path(_, Ok(Owner::NotDpkg))) => {}
                        Some(Exe::Path(path, Ok(Owner::Dpkg(names)))) => match &pkgs {
                            Err(e) => unknown.push(format!("W2: {e}")),
                            Ok(pkgs) => {
                                for n in names {
                                    match pkgs.iter().find(|p| &p.name == n) {
                                        Some(p) => {
                                            yes.insert(p.source.clone());
                                        }
                                        None => unknown.push(format!(
                                            "{} is owned by {n}, which is not in W2",
                                            path.as_str()
                                        )),
                                    }
                                }
                            }
                        },
                    }
                }
            }
        }
    }
    unknown.dedup();
    // W14 failure stays an error: candidates are then "unknown", never "none".
    // A malformed line gives that package an unknown (`Err`) candidate.
    let candidates = inst_lines(a).map(|is| {
        is.into_iter()
            .map(|i| (i.name, i.origin.map(|(c, _)| c)))
            .collect()
    });
    CveCtx {
        exposure: Exposure {
            yes,
            unknown: (!unknown.is_empty()).then(|| unknown.join("; ")),
        },
        candidates,
    }
}

fn tool_missing(rule: &str, t: &str, id: &str, install: &str) -> Finding {
    Finding::undetermined(
        rule,
        t,
        vec![],
        &format!("ツール未導入のため確認不能 ({id}: command not found) — 導入: {install}"),
    )
}

// ───────────────────────────── S1 / K4 ─────────────────────────────────────

fn s1(t: &str, a: &Answers, fs: &[Finding]) -> Finding {
    let insts = match inst_lines(a) {
        Ok(i) => i,
        Err(e) => return Finding::undetermined("S1", t, vec![], &e),
    };
    let sec: Vec<&Inst> = insts.iter().filter(|i| i.security()).collect();
    let bad = malformed(insts.iter());
    if sec.is_empty() {
        if let Some(why) = bad {
            return Finding::undetermined("S1", t, vec![], &why);
        }
        return Finding::clean(
            "S1",
            t,
            vec!["apt-get -s dist-upgrade: no pending update from a -security origin".into()],
        );
    }
    let stale = fs
        .iter()
        .any(|f| f.rule == "B7" && f.verdict == Status::Violation);
    let mut ev: Vec<String> = sec.iter().map(|i| i.line.clone()).collect();
    if let Some(why) = bad {
        ev.push(format!("also not judged (origin unreadable): {why}"));
    }
    let mut names: Vec<&str> = sec.iter().map(|i| i.name.as_str()).collect();
    names.dedup();
    let mut fix = format!(
        "sudo apt update && sudo apt upgrade\n# 対象パッケージ: {}",
        names.join(" ")
    );
    if stale {
        let note = "B7 が Violation（apt index が古い）ため、この件数は下限です";
        ev.push(note.to_string());
        fix.push_str(&format!("\n# {note}"));
    }
    Finding::violation("S1", t, ev, &fix)
}

fn k4(t: &str, a: &Answers) -> Finding {
    let insts = match inst_lines(a) {
        Ok(i) => i,
        Err(e) => return Finding::undetermined("K4", t, vec![], &e),
    };
    let kernel = || insts.iter().filter(|i| i.name.starts_with("linux-"));
    let k: Vec<&Inst> = kernel().filter(|i| i.security()).collect();
    let bad = malformed(kernel());
    if k.is_empty() {
        if let Some(why) = bad {
            return Finding::undetermined("K4", t, vec![], &why);
        }
        Finding::clean(
            "K4",
            t,
            vec!["no pending kernel update from a -security origin".into()],
        )
    } else {
        let mut ev: Vec<String> = k.iter().map(|i| i.line.clone()).collect();
        if let Some(why) = bad {
            ev.push(format!("also not judged (origin unreadable): {why}"));
        }
        Finding::violation(
            "K4",
            t,
            ev,
            "sudo apt update && sudo apt upgrade → 計画停止枠で再起動 (sudo reboot)",
        )
    }
}

// ───────────────────────────── S2 / S3 ─────────────────────────────────────

fn s2(t: &str, os: &OsInfo, a: &Answers) -> Finding {
    if !os.is_ubuntu() {
        return Finding::not_applicable("S2", t, vec![], "Ubuntu Pro / ESM applies to Ubuntu only");
    }
    let text = match obs(a, &Remote::ProSecurity) {
        Obs::Out(s) => s,
        Obs::Missing(_) => {
            return tool_missing("S2", t, "pro", "sudo apt install ubuntu-pro-client")
        }
        Obs::Undet(w) => return Finding::undetermined("S2", t, vec![], &w),
        other => {
            return Finding::undetermined("S2", t, vec![], &format!("W15 unexpected {other:?}"))
        }
    };
    let v: serde_json::Value = match serde_json::from_str(text.trim()) {
        Ok(v) => v,
        Err(e) => {
            return Finding::undetermined(
                "S2",
                t,
                vec![],
                &format!("`pro security-status` output is not JSON: {e}"),
            )
        }
    };
    let Some(pkgs) = v["packages"].as_array() else {
        return Finding::undetermined(
            "S2",
            t,
            vec![],
            "`pro security-status` JSON has no `packages` array (unexpected schema)",
        );
    };
    let mut bad = Vec::new();
    for p in pkgs {
        let (Some(name), Some(status)) = (p["package"].as_str(), p["status"].as_str()) else {
            return Finding::undetermined(
                "S2",
                t,
                vec![],
                &format!("`pro security-status` entry without package / status: {p}"),
            );
        };
        if matches!(status, "pending_attach" | "pending_enable") {
            bad.push(format!(
                "{name}: {status} ({})",
                p["service_name"].as_str().unwrap_or("?")
            ));
        }
    }
    if bad.is_empty() {
        Finding::clean(
            "S2",
            t,
            vec![format!(
                "{} packages: none fixable only by an inactive ESM service",
                pkgs.len()
            )],
        )
    } else {
        Finding::violation(
            "S2",
            t,
            bad,
            "sudo pro attach <token> で ESM を有効化する（または後継リリースへの移行を計画する）",
        )
    }
}

fn s3(t: &str, a: &Answers) -> Finding {
    let text = match obs(a, &Remote::NeedRestart) {
        Obs::Out(s) => s,
        Obs::Missing(_) => {
            return tool_missing("S3", t, "needrestart", "sudo apt install needrestart")
        }
        Obs::Undet(w) => return Finding::undetermined("S3", t, vec![], &w),
        other => {
            return Finding::undetermined("S3", t, vec![], &format!("W16 unexpected {other:?}"))
        }
    };
    let svcs: Vec<&str> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("NEEDRESTART-SVC:"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !svcs.is_empty() {
        return Finding::violation(
            "S3",
            t,
            svcs.iter()
                .map(|s| format!("NEEDRESTART-SVC: {s}"))
                .collect(),
            &format!(
                "sudo systemctl restart {}（または計画再起動）",
                svcs.join(" ")
            ),
        );
    }
    let ksta = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("NEEDRESTART-KSTA:"))
        .map(str::trim)
        .next();
    match ksta {
        Some("1") => Finding::clean(
            "S3",
            t,
            vec!["no NEEDRESTART-SVC line; NEEDRESTART-KSTA: 1".into()],
        ),
        Some("0") => Finding::undetermined(
            "S3",
            t,
            vec![],
            "needrestart reports NEEDRESTART-KSTA: 0 (state unknown)",
        ),
        // canon v1.1 §4 defines only KSTA 0 and 1; anything else is not judged
        // here (the kernel is K3's subject) and is not a pass either.
        Some(k) => Finding::undetermined(
            "S3",
            t,
            vec![],
            &format!("NEEDRESTART-KSTA: {k} is not covered by canon v1.1 §4 S3 (only 0 / 1 are)"),
        ),
        None => Finding::undetermined(
            "S3",
            t,
            vec![],
            "needrestart output has neither NEEDRESTART-SVC nor NEEDRESTART-KSTA",
        ),
    }
}

// ───────────────────────────── S5 ──────────────────────────────────────────

const OFFICIAL: [&str; 6] = [
    "archive.ubuntu.com",
    "security.ubuntu.com",
    "ports.ubuntu.com",
    "esm.ubuntu.com",
    "deb.debian.org",
    "security.debian.org",
];

/// Host part of a repository URL (userinfo and port removed, lower-cased).
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = auth.rsplit('@').next().unwrap_or(auth);
    let host = if let Some(h) = host.strip_prefix('[') {
        h.split(']').next().unwrap_or(h)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn is_official(url: &str) -> bool {
    let Some(h) = url_host(url) else {
        return false;
    };
    if OFFICIAL.contains(&h.as_str()) {
        return true;
    }
    // `*.archive.ubuntu.com` (country / cloud mirrors such as `*.ec2.archive.ubuntu.com`)
    h.strip_suffix(".archive.ubuntu.com").is_some_and(|sub| {
        !sub.is_empty()
            && sub
                .split('.')
                .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    })
}

/// Repository URLs of a one-line `.list` or a deb822 `.sources` text.
/// deb822 field names are case-insensitive and a `URIs` value may continue
/// on following indented lines (or start on the next one) (CONTRACT §12.10
/// item 1).
fn source_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_uris = false;
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            in_uris = false;
            continue;
        }
        if l.starts_with('#') {
            continue;
        }
        if line.starts_with([' ', '\t']) && in_uris {
            out.extend(l.split_whitespace().map(str::to_string));
            continue;
        }
        in_uris = false;
        if let Some((key, rest)) = l.split_once(':') {
            if !key.is_empty() && !key.contains(char::is_whitespace) {
                if key.eq_ignore_ascii_case("uris") {
                    out.extend(rest.split_whitespace().map(str::to_string));
                    in_uris = true;
                }
                continue;
            }
        }
        let mut toks = l.split_whitespace();
        if matches!(toks.next(), Some("deb" | "deb-src")) {
            let mut in_opts = false;
            for tok in toks {
                if in_opts {
                    if tok.ends_with(']') {
                        in_opts = false;
                    }
                    continue;
                }
                if tok.starts_with('[') {
                    in_opts = !tok.ends_with(']');
                    continue;
                }
                out.push(tok.to_string());
                break;
            }
        }
    }
    out
}

fn s5(t: &str, d: &Data, a: &Answers) -> Finding {
    let mut texts: Vec<(String, String)> = Vec::new();
    match obs(a, &Remote::SourcesList) {
        Obs::Out(s) => texts.push(("/etc/apt/sources.list".into(), s)),
        Obs::Absent => {}
        Obs::Undet(w) | Obs::Missing(w) => return Finding::undetermined("S5", t, vec![], &w),
        other => {
            return Finding::undetermined("S5", t, vec![], &format!("W18 unexpected {other:?}"))
        }
    }
    match obs(a, &Remote::SourcesDir) {
        Obs::Out(_) => {}
        Obs::Undet(w) | Obs::Missing(w) => return Finding::undetermined("S5", t, vec![], &w),
        other => {
            return Finding::undetermined("S5", t, vec![], &format!("W18 ls unexpected {other:?}"))
        }
    }
    for n in &d.source_files {
        match obs(a, &Remote::SourcesFile(n.clone())) {
            Obs::Out(s) => texts.push((format!("/etc/apt/sources.list.d/{}", n.as_str()), s)),
            Obs::Undet(w) | Obs::Missing(w) => {
                return Finding::undetermined(
                    "S5",
                    t,
                    vec![],
                    &format!(
                        "listed source file /etc/apt/sources.list.d/{} could not be read: {w}",
                        n.as_str()
                    ),
                )
            }
            other => {
                return Finding::undetermined(
                    "S5",
                    t,
                    vec![],
                    &format!("W18 cat {} unexpected {other:?}", n.as_str()),
                )
            }
        }
    }
    let mut foreign = Vec::new();
    let mut files = BTreeSet::new();
    let mut total = 0usize;
    let mut unparsed = Vec::new();
    for (file, text) in &texts {
        let urls = source_urls(text);
        // CONTRACT §12.10 item 1: a listed .d file that yields no URL was not
        // understood; it is not a clean file.
        if urls.is_empty() && file.starts_with("/etc/apt/sources.list.d/") {
            unparsed.push(file.clone());
        }
        for url in urls {
            total += 1;
            if !is_official(&url) {
                foreign.push(format!("{file}: {url}"));
                files.insert(file.clone());
            }
        }
    }
    if !unparsed.is_empty() {
        return Finding::undetermined(
            "S5",
            t,
            foreign,
            &format!(
                "no repository URL could be read from {}",
                unparsed.join(", ")
            ),
        );
    }
    if foreign.is_empty() {
        Finding::clean(
            "S5",
            t,
            vec![format!("{total} repository entries, all official")],
        )
    } else {
        Finding::warning(
            "S5",
            t,
            foreign,
            &format!(
                "公式以外のリポジトリがあります。意図して導入したものか確認し、不要なら該当ファイル（{}）を削除するか該当行を削除してください",
                files.into_iter().collect::<Vec<_>>().join(", ")
            ),
        )
    }
}

// ───────────────────────────── S6 / S7 ─────────────────────────────────────

fn s6(t: &str, a: &Answers) -> Finding {
    match obs(a, &Remote::DpkgAudit) {
        Obs::Out(s) if s.trim().is_empty() => {
            Finding::clean("S6", t, vec!["dpkg --audit: no output".into()])
        }
        Obs::Out(s) => Finding::violation(
            "S6",
            t,
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .collect(),
            "sudo dpkg --configure -a",
        ),
        Obs::Undet(w) | Obs::Missing(w) => Finding::undetermined("S6", t, vec![], &w),
        other => Finding::undetermined("S6", t, vec![], &format!("W19 unexpected {other:?}")),
    }
}

fn s7(t: &str, a: &Answers, b5_bins: &Result<BTreeSet<String>, String>) -> Finding {
    let holds: Vec<String> = match obs(a, &Remote::Holds) {
        Obs::Out(s) => s
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Obs::Undet(w) | Obs::Missing(w) => return Finding::undetermined("S7", t, vec![], &w),
        other => {
            return Finding::undetermined("S7", t, vec![], &format!("W20 unexpected {other:?}"))
        }
    };
    if holds.is_empty() {
        return Finding::clean("S7", t, vec!["apt-mark showhold: no held package".into()]);
    }
    // (names with a -security origin, names whose origin is unreadable)
    let s1 = inst_lines(a).map(|is| {
        let sec = is
            .iter()
            .filter(|i| i.security())
            .map(|i| i.name.clone())
            .collect::<BTreeSet<String>>();
        let bad = is
            .iter()
            .filter(|i| i.origin.is_err())
            .map(|i| i.name.clone())
            .collect::<BTreeSet<String>>();
        (sec, bad)
    });
    let mut blocked: Vec<&String> = holds
        .iter()
        .filter(|h| {
            s1.as_ref().is_ok_and(|(s, _)| s.contains(*h))
                || b5_bins.as_ref().is_ok_and(|b| b.contains(*h))
        })
        .collect();
    blocked.dedup();
    let ev = vec![format!("held: {}", holds.join(" "))];
    if !blocked.is_empty() {
        let mut ev = ev;
        ev.extend(
            blocked
                .iter()
                .map(|b| format!("{b} is held but has a pending security fix (S1 / B5)")),
        );
        let fix = blocked
            .iter()
            .map(|b| format!("sudo apt-mark unhold {b}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Finding::violation("S7", t, ev, &format!("hold の理由を確認してから:\n{fix}"));
    }
    let mut undet = Vec::new();
    match &s1 {
        Err(e) => undet.push(format!("S1 (W14) undetermined: {e}")),
        Ok((_, bad)) => {
            let held_bad: Vec<&String> = holds.iter().filter(|h| bad.contains(*h)).collect();
            if !held_bad.is_empty() {
                undet.push(format!(
                    "S1 (W14) origin unreadable for held {}",
                    held_bad
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
        }
    }
    if let Err(e) = b5_bins {
        undet.push(format!("B5 undetermined: {e}"));
    }
    if undet.is_empty() {
        Finding::clean("S7", t, ev)
    } else {
        Finding::undetermined(
            "S7",
            t,
            ev,
            &format!(
                "holds exist and cannot be compared with pending fixes: {}",
                undet.join("; ")
            ),
        )
    }
}

// ───────────────────────────── S8 / S9 ─────────────────────────────────────

/// One monitored product occurrence (canon v1.1 §4: product × version × origin).
struct Item {
    label: String,
    slug: &'static str,
    version: Result<String, String>,
}

struct Judged {
    status: Status,
    line: String,
    fix: Option<String>,
    fetched: Option<i64>,
}

fn judge(ctx: &Ctx, memo: &mut Memo, it: &Item, warn_band: bool, s9: bool) -> Judged {
    let undet = |line: String| Judged {
        status: Status::Undetermined,
        line,
        fix: None,
        fetched: None,
    };
    let v = match &it.version {
        Ok(v) => v,
        Err(e) => return undet(format!("{}: {} version unknown — {e}", it.label, it.slug)),
    };
    let feed = match memo.eol(ctx, it.slug) {
        Ok(f) => f.clone(),
        Err(e) => {
            return undet(format!(
                "{}: {} {v} — endoflife.date `{}` unavailable: {e}",
                it.label, it.slug, it.slug
            ))
        }
    };
    let Some(c) = feeds::match_cycle(&feed.data, v) else {
        let mut j = undet(format!(
            "{}: {} {v} — no endoflife.date cycle matches",
            it.label, it.slug
        ));
        j.fetched = Some(feed.fetched_at);
        return j;
    };
    let head = format!(
        "{}: {} {v} cycle {} (eol {})",
        it.label,
        it.slug,
        c.cycle,
        feeds::support_text(c.eol)
    );
    let (status, fix) = match feeds::eol_state(c.eol, ctx.now) {
        Err(e) => (Status::Undetermined, Some(e)),
        Ok(Period::Ended) => (
            Status::Violation,
            Some(format!(
                "{}: {} {} は upstream のサポートが終了しています（security patch が提供されない版）。後継のサポート版へ移行してください{}",
                it.label,
                it.slug,
                c.cycle,
                if s9 { "（ディストリのバックポートの範囲は S2 を参照）" } else { "" }
            )),
        ),
        Ok(Period::Running(Some(d))) if warn_band && d <= S8_WARN_DAYS => (
            Status::Warning,
            Some(format!(
                "{}: {} {} のサポート期限まで {d} 日です。後継のサポート版への移行を計画してください",
                it.label, it.slug, c.cycle
            )),
        ),
        Ok(Period::Running(_)) => (Status::Clean, None),
    };
    let line = match (status, &fix) {
        (Status::Undetermined, Some(e)) => format!("{head} — {e}"),
        (Status::Violation, _) => format!("{head} — expired"),
        (Status::Warning, _) => format!("{head} — expires within {S8_WARN_DAYS} days"),
        _ => format!("{head} — supported"),
    };
    Judged {
        status,
        line,
        fix: if status == Status::Undetermined {
            None
        } else {
            fix
        },
        fetched: Some(feed.fetched_at),
    }
}

/// Fold the items of S8 / S9 into one finding (worst item, canon v1.1 §1).
fn fold(
    rule: &str,
    t: &str,
    ctx: &Ctx,
    memo: &mut Memo,
    items: &[Item],
    warn_band: bool,
    none_why: &str,
) -> Finding {
    if items.is_empty() {
        return Finding::not_applicable(rule, t, vec![], none_why);
    }
    let js: Vec<Judged> = items
        .iter()
        .map(|it| judge(ctx, memo, it, warn_band, rule == "S9"))
        .collect();
    let worst = js
        .iter()
        .map(|j| j.status)
        .max_by_key(|s| s.rank())
        .unwrap_or(Status::Undetermined);
    let ev: Vec<String> = js.iter().map(|j| j.line.clone()).collect();
    let fix: Vec<String> = js.iter().filter_map(|j| j.fix.clone()).collect();
    let reason: Vec<String> = js
        .iter()
        .filter(|j| j.status == Status::Undetermined)
        .map(|j| j.line.clone())
        .collect();
    let mut f = Finding::of(worst, rule, t, ev, &fix.join("\n"), &reason.join("; "));
    if let Some(at) = js.iter().filter_map(|j| j.fetched).min() {
        f = f.with("eol_fetched_at", json!(at));
    }
    f
}

fn s9_items(pkgs: &[Pkg]) -> Vec<Item> {
    pkgs.iter()
        .filter_map(|p| {
            let slug = products::dpkg_product(&p.name)?;
            Some(Item {
                label: format!("dpkg {} {}", p.name, p.version),
                slug,
                version: products::dpkg_version(&p.version)
                    .ok_or_else(|| format!("dpkg version {:?} has no leading number", p.version)),
            })
        })
        .collect()
}

fn version_item(label: String, slug: &'static str, o: &Obs) -> Option<Item> {
    let (slug, version) = match o {
        Obs::Absent => return None,
        Obs::Out(text) => match products::parse_output(slug, text) {
            Some((s, v)) => (s, Ok(v)),
            None => (
                slug,
                Err(format!("version output not understood: {:?}", text.trim())),
            ),
        },
        Obs::Undet(w) | Obs::Missing(w) => (slug, Err(w.clone())),
        other => (slug, Err(format!("unexpected {other:?}"))),
    };
    Some(Item {
        label,
        slug,
        version,
    })
}

/// S8: PATH (W21), non-dpkg listening executables (W22) and containers (W23).
fn s8(ctx: &Ctx, t: &str, d: &Data, a: &Answers, dpkg_items: &[Item], memo: &mut Memo) -> Finding {
    let mut items = Vec::new();
    // PATH (W21)
    for (i, (slug, cmd)) in products::W21.iter().enumerate() {
        let Some(it) = version_item(
            format!("PATH `{cmd}`"),
            slug,
            &obs(a, &Remote::ProductVersion(i)),
        ) else {
            continue;
        };
        // [choice] CONTRACT §12.6: same cycle as the dpkg package of the same
        // product = it is that dpkg binary (one S9 item, no S8 item).
        if let Ok(v) = &it.version {
            let feed = memo.eol(ctx, it.slug).as_ref().ok().map(|f| f.data.clone());
            let dup = dpkg_items.iter().any(|di| {
                di.slug == it.slug
                    && di.version.as_ref().is_ok_and(|dv| match &feed {
                        Some(cs) => match (feeds::match_cycle(cs, dv), feeds::match_cycle(cs, v)) {
                            (Some(x), Some(y)) => x.cycle == y.cycle,
                            _ => dv == v,
                        },
                        None => dv == v,
                    })
            });
            if dup {
                continue;
            }
        }
        items.push(it);
    }
    // listening processes (W12 → W12b → W13 → W22)
    match &d.listeners {
        Err(e) => items.push(Item {
            label: "listening processes".into(),
            slug: "-",
            version: Err(format!("W12 could not be observed: {e}")),
        }),
        Ok(ls) => {
            for l in ls {
                for p in l.pids.iter().filter_map(|p| p.as_ref().err()) {
                    items.push(Item {
                        label: format!("listener {:?}", l.line),
                        slug: "-",
                        version: Err(format!(
                            "pid {p:?} is not numeric; the process was not examined"
                        )),
                    });
                }
            }
            for (pid, exe) in &d.exes {
                match exe {
                    Exe::Unread(w) => items.push(Item {
                        label: format!("process {pid:?}"),
                        slug: "-",
                        version: Err(format!("executable could not be read: {w}")),
                    }),
                    Exe::Unsafe(raw) => {
                        if let Some((slug, _)) = product_of_basename(raw) {
                            items.push(Item {
                                label: format!("process {pid:?} {raw:?}"),
                                slug,
                                version: Err(
                                    "the executable path is unsafe to pass to ssh; never run"
                                        .into(),
                                ),
                            });
                        }
                    }
                    Exe::Path(path, owner) => {
                        let Some((slug, args)) = products::by_basename(path.basename()) else {
                            continue;
                        };
                        match owner {
                            Ok(Owner::Dpkg(_)) => {}
                            Err(w) => items.push(Item {
                                label: format!("process {}", path.as_str()),
                                slug,
                                version: Err(format!("dpkg ownership unknown: {w}")),
                            }),
                            Ok(Owner::NotDpkg) => {
                                let o = obs(a, &Remote::ExeVersion(path.clone(), args));
                                let it = version_item(
                                    format!("process {}", path.as_str()),
                                    slug,
                                    &o,
                                )
                                .unwrap_or(Item {
                                    label: format!("process {}", path.as_str()),
                                    slug,
                                    version: Err(
                                        "the version command reported the executable as not found"
                                            .into(),
                                    ),
                                });
                                items.push(it);
                            }
                        }
                    }
                }
            }
        }
    }
    // containers (W23)
    match obs(a, &Remote::DockerImages) {
        Obs::Missing(_) => {}
        Obs::Out(s) => {
            for img in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
                if let Some((slug, v)) = products::parse_image(img) {
                    items.push(Item {
                        label: format!("container {img}"),
                        slug,
                        version: v,
                    });
                }
            }
        }
        Obs::Undet(w) => items.push(Item {
            label: "containers".into(),
            slug: "-",
            version: Err(format!("docker ps could not be observed: {w}")),
        }),
        other => items.push(Item {
            label: "containers".into(),
            slug: "-",
            version: Err(format!("W23 unexpected {other:?}")),
        }),
    }
    fold(
        "S8",
        t,
        ctx,
        memo,
        &items,
        true,
        "no monitored product outside dpkg was found (PATH, listening processes, containers)",
    )
}

// ───────────────────────────── K1 ──────────────────────────────────────────

fn eol_slug(id: &str) -> Option<&'static str> {
    Some(match id {
        "ubuntu" => "ubuntu",
        "debian" => "debian",
        "rhel" => "rhel",
        "centos" => "centos",
        "amzn" => "amazon-linux",
        _ => return None,
    })
}

/// K1: release support from endoflife.date; only when that feed cannot be
/// obtained (or the OS has no endoflife.date product), Ubuntu / Debian fall
/// back to W17.
pub fn k1(ctx: &Ctx, t: &str, os: &OsInfo, w17: Option<&Obs>, memo: &mut Memo) -> Finding {
    const FIX: &str = "リリースアップグレードを計画してください（Ubuntu: `sudo do-release-upgrade` は人間が計画停止枠で実行。Debian: 次リリースへの移行手順に従う）";
    let label = format!("{} {}", os.id, os.version_id);
    let feed_why = match eol_slug(&os.id) {
        None => format!("no endoflife.date product for ID={:?}", os.id),
        Some(slug) => match memo.eol(ctx, slug) {
            Err(e) => format!("endoflife.date `{slug}` unavailable: {e}"),
            Ok(feed) => match feeds::match_cycle(&feed.data, &os.version_id) {
                // CONTRACT §12.10 item 11: W17 is the fallback for an
                // unobtainable feed only; a loaded feed without our cycle is
                // undetermined.
                None => {
                    return Finding::undetermined(
                        "K1",
                        t,
                        vec![],
                        &format!(
                            "endoflife.date `{slug}` has no cycle for VERSION_ID {:?}",
                            os.version_id
                        ),
                    )
                    .with("eol_fetched_at", json!(feed.fetched_at))
                }
                Some(c) => {
                    let ev = vec![format!(
                        "{label}: {slug} cycle {} eol {}, extendedSupport {}",
                        c.cycle,
                        feeds::support_text(c.eol),
                        feeds::support_text(c.ext)
                    )];
                    let at = feed.fetched_at;
                    let f = match feeds::eol_state(c.eol, ctx.now) {
                        Err(e) => Finding::undetermined("K1", t, ev, &e),
                        Ok(Period::Running(_)) => Finding::clean("K1", t, ev),
                        Ok(Period::Ended) if feeds::ext_running(c.ext, ctx.now) => {
                            Finding::warning(
                                "K1",
                                t,
                                ev,
                                &format!(
                                    "標準サポートは終了し、延長サポート中（ESM／ELTS の契約が要る）です。契約を確認し、{FIX}"
                                ),
                            )
                        }
                        Ok(Period::Ended) => Finding::violation("K1", t, ev, FIX),
                    };
                    return f.with("eol_fetched_at", json!(at));
                }
            },
        },
    };
    let Some(w17) = w17 else {
        return Finding::undetermined(
            "K1",
            t,
            vec![],
            &format!("{feed_why}; no distro-info fallback for this OS"),
        );
    };
    match w17 {
        Obs::Out(s) => {
            let Some(code) = &os.codename else {
                return Finding::undetermined(
                    "K1",
                    t,
                    vec![],
                    &format!("{feed_why}; W1 has no VERSION_CODENAME to look up in distro-info"),
                );
            };
            let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            // CONTRACT §12.10 item 11: only codename-shaped lines (one word of
            // lowercase letters) are a supported list; empty or any other
            // line means the output was not understood.
            let shaped = |l: &&str| l.chars().all(|c| c.is_ascii_lowercase());
            if lines.is_empty() || !lines.iter().all(shaped) {
                return Finding::undetermined(
                    "K1",
                    t,
                    vec![],
                    &format!(
                        "{feed_why}; W17 output is not a list of codenames: {:?}",
                        s.trim()
                    ),
                );
            }
            let supported = lines;
            let ev = vec![
                format!("{feed_why}; fallback distro-info --supported: {}", supported.join(" ")),
                format!("current codename: {code}"),
            ];
            if supported.contains(&code.as_str()) {
                Finding::clean("K1", t, ev)
            } else {
                Finding::violation("K1", t, ev, FIX)
            }
        }
        Obs::Missing(_) => Finding::undetermined(
            "K1",
            t,
            vec![],
            &format!(
                "{feed_why}; ツール未導入のため確認不能 (distro-info: command not found) — 導入: sudo apt install distro-info"
            ),
        ),
        Obs::Undet(w) => Finding::undetermined("K1", t, vec![], &format!("{feed_why}; W17: {w}")),
        other => Finding::undetermined(
            "K1",
            t,
            vec![],
            &format!("{feed_why}; W17 unexpected {other:?}"),
        ),
    }
}

// ───────────────────────────── K2 / K3 / K5 ────────────────────────────────

fn uname(a: &Answers) -> Result<String, String> {
    match obs(a, &Remote::Uname) {
        Obs::Out(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        Obs::Out(_) => Err("uname -r printed nothing".into()),
        Obs::Undet(w) | Obs::Missing(w) => Err(w),
        other => Err(format!("W24 unexpected {other:?}")),
    }
}

fn k2_fix(items: &[cve::Item]) -> String {
    let mut lines = vec![
        "新しいカーネルを導入して再起動してください: sudo apt update && sudo apt upgrade → 計画停止枠で sudo reboot".to_string(),
    ];
    let nofix: Vec<String> = items
        .iter()
        .filter(|i| i.fixed.is_none())
        .map(|i| i.cve.clone().unwrap_or_else(|| i.id.clone()))
        .collect();
    if !nofix.is_empty() {
        lines.push(format!(
            "回避策が必要 (修正版なし): {} — 影響する機能の無効化・接続元の制限",
            nofix.join(", ")
        ));
    }
    let mut ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    ids.sort();
    ids.dedup();
    lines.push(format!("# advisories: {}", ids.join(" ")));
    lines.join("\n")
}

fn k2(ctx: &Ctx, t: &str, a: &Answers, memo: &mut Memo, cx: &CveCtx) -> baseline::CveOutcome {
    let fail = |why: String| baseline::CveOutcome {
        finding: Finding::undetermined("K2", t, vec![], &why),
        items: Err(why),
    };
    let os = match obs(a, &Remote::OsRelease) {
        Obs::Out(s) => s,
        other => return fail(format!("W1 unexpected {other:?}")),
    };
    let u = match uname(a) {
        Ok(u) => u,
        Err(e) => return fail(e),
    };
    let pkgs = match dpkg_pkgs(a) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let want = format!("linux-image-{u}");
    let Some(p) = pkgs.iter().find(|p| p.name == want) else {
        return fail(format!(
            "the running kernel's package {want} is not in the dpkg list (W2)"
        ));
    };
    let spec = CveRule {
        rule: "K2",
        scope: format!(
            "running kernel {u} ({} {}, source {} {})",
            p.name, p.version, p.source, p.source_version
        ),
        fix: k2_fix,
    };
    baseline::cve_rule(ctx, t, &spec, &os, std::slice::from_ref(p), memo, cx)
}

/// `(numeric part, flavour)` of a kernel release (`5.15.0-100-generic` →
/// (`5.15.0-100`, `generic`)).
fn flavour(release: &str) -> (String, String) {
    let parts: Vec<&str> = release.split('-').collect();
    let n = parts
        .iter()
        .take_while(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .count();
    (parts[..n].join("-"), parts[n..].join("-"))
}

fn k3(t: &str, a: &Answers, k2_violation: bool) -> Finding {
    let u = match uname(a) {
        Ok(u) => u,
        Err(e) => return Finding::undetermined("K3", t, vec![], &e),
    };
    let pkgs = match dpkg_pkgs(a) {
        Ok(p) => p,
        Err(e) => return Finding::undetermined("K3", t, vec![], &e),
    };
    let want = format!("linux-image-{u}");
    let Some(run) = pkgs.iter().find(|p| p.name == want) else {
        return Finding::undetermined(
            "K3",
            t,
            vec![],
            &format!("the running kernel's package {want} is not in the dpkg list (W2)"),
        );
    };
    let (_, fl) = flavour(&u);
    let newest = pkgs
        .iter()
        .filter(|p| {
            p.name.strip_prefix("linux-image-").is_some_and(|r| {
                let (num, f) = flavour(r);
                !num.is_empty() && f == fl
            })
        })
        .max_by(|x, y| osv::dpkg_cmp(&x.version, &y.version));
    match newest {
        Some(n) if osv::dpkg_cmp(&n.version, &run.version) == std::cmp::Ordering::Greater => {
            let mut ev = vec![
                format!("running {u} ({} {})", run.name, run.version),
                format!("installed newer {} {} (flavour -{fl})", n.name, n.version),
            ];
            if k2_violation {
                ev.push("K2: 動作中カーネルの脆弱性は再起動まで残ります".into());
            }
            Finding::violation(
                "K3",
                t,
                ev,
                "計画停止枠で再起動してください (sudo reboot)。再起動するまで動作中カーネルの脆弱性（K2）は残ります",
            )
        }
        _ => Finding::clean(
            "K3",
            t,
            vec![format!(
                "running {u} ({}) is the newest installed -{fl} kernel",
                run.version
            )],
        ),
    }
}

fn k5(t: &str, os: &OsInfo, a: &Answers, k2: &Result<Vec<cve::Item>, String>) -> Finding {
    if !os.is_ubuntu() {
        return Finding::not_applicable(
            "K5",
            t,
            vec![],
            "Livepatch (Ubuntu Pro) applies to Ubuntu only",
        );
    }
    let text = match obs(a, &Remote::ProStatus) {
        Obs::Out(s) => s,
        Obs::Missing(_) => {
            return tool_missing("K5", t, "pro", "sudo apt install ubuntu-pro-client")
        }
        Obs::Undet(w) => return Finding::undetermined("K5", t, vec![], &w),
        other => {
            return Finding::undetermined("K5", t, vec![], &format!("W25 unexpected {other:?}"))
        }
    };
    let v: serde_json::Value = match serde_json::from_str(text.trim()) {
        Ok(v) => v,
        Err(e) => {
            return Finding::undetermined(
                "K5",
                t,
                vec![],
                &format!("`pro status` output is not JSON: {e}"),
            )
        }
    };
    let Some(svcs) = v["services"].as_array() else {
        return Finding::undetermined(
            "K5",
            t,
            vec![],
            "`pro status` JSON has no `services` array (unexpected schema)",
        );
    };
    let lp = svcs
        .iter()
        .find(|s| s["name"].as_str() == Some("livepatch"))
        .and_then(|s| s["status"].as_str())
        .unwrap_or("absent");
    let ev = vec![format!("livepatch: {lp}")];
    if lp == "enabled" {
        return Finding::clean("K5", t, ev);
    }
    match k2 {
        Err(e) => Finding::undetermined(
            "K5",
            t,
            ev,
            &format!("livepatch is not enabled and K2 is undetermined: {e}"),
        ),
        Ok(items) => {
            let hi: Vec<String> = items
                .iter()
                .filter(|i| matches!(i.severity, "high" | "critical"))
                .map(|i| format!("{} ({})", i.cve.as_deref().unwrap_or(&i.id), i.severity))
                .collect();
            if hi.is_empty() {
                Finding::clean("K5", t, ev)
            } else {
                let mut ev = ev;
                ev.push(format!(
                    "K2 high/critical: {} — 再起動まで無防備",
                    hi.join(", ")
                ));
                Finding::warning(
                    "K5",
                    t,
                    ev,
                    "sudo pro enable livepatch（livepatch が無いと、動作中カーネルの high/critical 脆弱性は再起動まで無防備）",
                )
            }
        }
    }
}

// ───────────────────────────── all rules ───────────────────────────────────

/// S1–S3, S5–S9, K1–K5 for a Debian-family host. `fs` = the B findings
/// (S1 reads B7), `b5_bins` = B5-affected binary packages (S7).
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    ctx: &Ctx,
    t: &str,
    os: &OsInfo,
    d: &Data,
    a: &Answers,
    fs: &[Finding],
    b5_bins: &Result<BTreeSet<String>, String>,
    cx: &CveCtx,
    memo: &mut Memo,
) -> Vec<Finding> {
    let w17 = obs(
        a,
        &if os.is_ubuntu() {
            Remote::DistroInfoUbuntu
        } else {
            Remote::DistroInfoDebian
        },
    );
    let (s9, dpkg_items) = match dpkg_pkgs(a) {
        Ok(pkgs) => {
            let items = s9_items(&pkgs);
            let f = fold(
                "S9",
                t,
                ctx,
                memo,
                &items,
                false,
                "no monitored product is installed by dpkg",
            );
            (f, items)
        }
        Err(e) => (Finding::undetermined("S9", t, vec![], &e), Vec::new()),
    };
    let s8 = s8(ctx, t, d, a, &dpkg_items, memo);
    let k2o = k2(ctx, t, a, memo, cx);
    let k2_violation = k2o.finding.verdict == Status::Violation;
    vec![
        s1(t, a, fs),
        s2(t, os, a),
        s3(t, a),
        s5(t, d, a),
        s6(t, a),
        s7(t, a, b5_bins),
        s8,
        s9,
        k1(ctx, t, os, Some(&w17), memo),
        k5(t, os, a, &k2o.items),
        k2o.finding,
        k3(t, a, k2_violation),
        k4(t, a),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_hosts() {
        assert!(is_official("http://archive.ubuntu.com/ubuntu"));
        assert!(is_official(
            "http://ap-northeast-1.ec2.archive.ubuntu.com/ubuntu"
        ));
        for bad in [
            "http://archive.ubuntu.com.evil.example/ubuntu",
            "http://evilarchive.ubuntu.com/ubuntu",
            "http://archive.ubuntu.com@evil.example/ubuntu",
            "http://evil.example/archive.ubuntu.com/ubuntu",
            "archive.ubuntu.com/ubuntu",
        ] {
            assert!(!is_official(bad), "{bad}");
        }
    }

    #[test]
    fn listener_pids() {
        let ls = parse_listeners(
            "LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"a\",pid=$(id),fd=3),(\"b\",pid=12,fd=4))\n",
        )
        .unwrap_or_default();
        assert_eq!(ls.len(), 1);
        assert!(ls[0].external);
        assert!(ls[0].pids[0].is_err());
        assert_eq!(ls[0].pids[1], Pid::parse("12").ok_or_else(String::new));
    }

    #[test]
    fn kernel_flavour() {
        assert_eq!(
            flavour("5.15.0-100-generic"),
            ("5.15.0-100".into(), "generic".into())
        );
        assert_eq!(
            flavour("6.1.0-25-amd64"),
            ("6.1.0-25".into(), "amd64".into())
        );
        assert_eq!(flavour("generic"), (String::new(), "generic".into()));
    }

    #[test]
    fn deb822_and_options() {
        let u = source_urls("deb [arch=amd64 signed-by=/x.gpg] https://a.example/x stable main\n# deb http://b\nURIs: http://c.example http://d.example\n");
        assert_eq!(
            u,
            [
                "https://a.example/x",
                "http://c.example",
                "http://d.example"
            ]
        );
    }
}
