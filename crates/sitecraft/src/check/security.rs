//! `sitecraft check security` — block on:
//!
//! * secrets in build output (`build_dir`, including `*.map`): fixed-prefix
//!   patterns (AWS access key, GitHub tokens, Stripe secret keys, private-key
//!   blocks with a body, Google API keys); matches are printed redacted;
//! * dangerous HTML sinks in sources (`innerHTML` assignment of a
//!   non-constant, `dangerouslySetInnerHTML`, `v-html`, `{@html}`, `eval`,
//!   `new Function`), excluding `node_modules` and `dist`;
//! * `npm audit --json` (`$SITECRAFT_NPM` / `npm`) reporting high or critical
//!   vulnerabilities, judged from the JSON shape, not the exit code.
//!
//! Undetermined: audit JSON without all six `metadata.vulnerabilities` keys,
//! an `error` object (including ENOLOCK), empty or invalid stdout, missing
//! npm, or zero files to scan.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;

use crate::config::{self, Config, Loaded};
use crate::Verdict;
use harness_core::verdict::Reason;

/// Upper bound for one `npm audit` run; a hung audit is undetermined.
const AUDIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Run the security check for `root`.
///
/// Contract: config not loadable (`Absent` or `Undetermined`) →
/// `Undetermined`; otherwise [`check_security`].
pub fn run(root: &Path) -> Verdict {
    match config::load(root) {
        Loaded::Config(cfg, _) => check_security(&cfg, root),
        Loaded::Absent => Verdict::undetermined(format!(
            "sitecraft check security: no {} in {}; create it (see sitecraft docs) or pass --root <project>",
            config::FILE_NAME,
            root.display()
        )),
        Loaded::Undetermined(why) => Verdict::undetermined(format!(
            "sitecraft check security: config unusable: {why}; fix {}",
            config::FILE_NAME
        )),
    }
}

/// Run the three scans for `root` with `config` and combine them with
/// [`Verdict::worst_of`]. `Clean` only when every scan ran on a non-empty
/// target set and found nothing; violation output names file, line and fix.
pub fn check_security(config: &Config, root: &Path) -> Verdict {
    Verdict::worst_of([
        scan_secrets(config, root),
        scan_sinks(config, root),
        audit(root),
    ])
}

// ---------------------------------------------------------------------------
// file walking
// ---------------------------------------------------------------------------

/// Recursively list regular files under `dir`. `skip` decides, per directory
/// name, whether to prune. Any IO error is returned (never a partial list).
fn walk(
    dir: &Path,
    skip: &dyn Fn(&Path, &str) -> bool,
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read directory {}: {e}", dir.display()))?;
    for ent in rd {
        let ent = ent.map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        let path = ent.path();
        // Follows symlinks for files; a dangling link is an error, not a skip.
        let meta =
            std::fs::metadata(&path).map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
        let is_link = ent
            .file_type()
            .map_err(|e| format!("cannot stat {}: {e}", path.display()))?
            .is_symlink();
        if meta.is_dir() {
            if is_link {
                continue; // do not follow directory links (loops)
            }
            let name = ent.file_name().to_string_lossy().into_owned();
            if skip(&path, &name) {
                continue;
            }
            walk(&path, skip, out)?;
        } else if meta.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn read_lossy(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn line_of(text: &str, byte_off: usize) -> usize {
    text[..byte_off].bytes().filter(|b| *b == b'\n').count() + 1
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

// ---------------------------------------------------------------------------
// secrets
// ---------------------------------------------------------------------------

struct SecretPattern {
    label: &'static str,
    re: Regex,
    /// Print only the label (never any characters of the match).
    label_only: bool,
}

fn secret_patterns() -> Result<&'static Vec<SecretPattern>, String> {
    static P: OnceLock<Result<Vec<SecretPattern>, String>> = OnceLock::new();
    P.get_or_init(|| {
        let defs: [(&str, &str, bool); 6] = [
            ("AWS access key", r"AKIA[0-9A-Z]{16}", false),
            (
                "GitHub token",
                r"(?:gh[po]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})",
                false,
            ),
            ("Stripe secret key", r"sk_(?:live|test)_[A-Za-z0-9]{24,}", false),
            ("Google API key", r"AIza[0-9A-Za-z_\-]{35}", false),
            (
                "private key block",
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?:\s|\\n|\\r)*(?:Proc-Type|DEK-Info|[A-Za-z0-9+/]{16,})",
                true,
            ),
            ("GitHub token", r"\bghs_[A-Za-z0-9]{36,}", false),
        ];
        defs.iter()
            .map(|(label, pat, label_only)| {
                Regex::new(pat)
                    .map(|re| SecretPattern {
                        label,
                        re,
                        label_only: *label_only,
                    })
                    .map_err(|e| format!("internal secret pattern {label} invalid: {e}"))
            })
            .collect()
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn redact(m: &str) -> String {
    let head: String = m.chars().take(4).collect();
    format!("{head}...[redacted, {} chars]", m.chars().count())
}

fn scan_secrets(config: &Config, root: &Path) -> Verdict {
    let pats = match secret_patterns() {
        Ok(p) => p,
        Err(e) => return Verdict::undetermined(format!("sitecraft check security: {e}")),
    };
    let build = root.join(&config.build_dir);
    if !build.is_dir() {
        return Verdict::undetermined(format!(
            "sitecraft check security: build_dir {} does not exist or is not a directory, so no secrets scan ran; build the project first (e.g. npm run build) or fix build_dir in {}",
            build.display(),
            config::FILE_NAME
        ));
    }
    let mut files = Vec::new();
    if let Err(e) = walk(&build, &|_, _| false, &mut files) {
        return Verdict::undetermined(format!("sitecraft check security: {e}; fix permissions"));
    }
    if files.is_empty() {
        return Verdict::undetermined(format!(
            "sitecraft check security: build_dir {} contains no files, so nothing was scanned for secrets; build the project first",
            build.display()
        ));
    }
    files.sort();
    let mut findings = Vec::new();
    for f in &files {
        let text = match read_lossy(f) {
            Ok(t) => t,
            Err(e) => {
                return Verdict::undetermined(format!(
                    "sitecraft check security: {e}; fix permissions"
                ))
            }
        };
        for p in pats {
            for m in p.re.find_iter(&text) {
                let shown = if p.label_only {
                    "[key body redacted]".to_string()
                } else {
                    redact(m.as_str())
                };
                findings.push(Reason::new(format!(
                    "{}:{}: {} found in build output ({shown}); remove it from the source, rotate the credential, and rebuild (use a server-side secret or env var instead of bundling it)",
                    rel(root, f),
                    line_of(&text, m.start()),
                    p.label
                )));
            }
        }
    }
    Verdict::from_findings(findings)
}

// ---------------------------------------------------------------------------
// dangerous sinks
// ---------------------------------------------------------------------------

struct Sinks {
    inner_html: Regex,
    constant: Regex,
    others: Vec<(&'static str, Regex)>,
}

fn sinks() -> Result<&'static Sinks, String> {
    static S: OnceLock<Result<Sinks, String>> = OnceLock::new();
    S.get_or_init(|| {
        let mk =
            |p: &str| Regex::new(p).map_err(|e| format!("internal sink pattern {p} invalid: {e}"));
        Ok(Sinks {
            inner_html: mk(r"\binnerHTML\s*(\+?=+)\s*(.*)")?,
            constant: mk(r#"^(?:"[^"\\]*"|'[^'\\]*'|`[^`$\\]*`)\s*;?\s*$"#)?,
            others: vec![
                (
                    "dangerouslySetInnerHTML",
                    mk(r"\bdangerouslySetInnerHTML\b")?,
                ),
                ("v-html", mk(r"\bv-html\b")?),
                ("{@html}", mk(r"\{@html\b")?),
                ("eval()", mk(r"\beval\s*\(")?),
                ("new Function", mk(r"\bnew\s+Function\b")?),
            ],
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn source_globs(config: &Config) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for g in &config.web_globs {
        b.add(Glob::new(g).map_err(|e| format!("web glob {g:?} does not compile: {e}"))?);
    }
    b.build()
        .map_err(|e| format!("cannot build web_globs: {e}"))
}

fn scan_sinks(config: &Config, root: &Path) -> Verdict {
    let sk = match sinks() {
        Ok(s) => s,
        Err(e) => return Verdict::undetermined(format!("sitecraft check security: {e}")),
    };
    let globs = match source_globs(config) {
        Ok(g) => g,
        Err(e) => return Verdict::undetermined(format!("sitecraft check security: {e}")),
    };
    let build = root.join(&config.build_dir);
    let skip = |p: &Path, name: &str| {
        name == "node_modules" || name == "dist" || name == ".git" || p == build
    };
    let mut all = Vec::new();
    if let Err(e) = walk(root, &skip, &mut all) {
        return Verdict::undetermined(format!("sitecraft check security: {e}; fix permissions"));
    }
    let mut sources: Vec<PathBuf> = all
        .into_iter()
        .filter(|p| {
            p.strip_prefix(root)
                .map(|r| globs.is_match(r))
                .unwrap_or(false)
        })
        .collect();
    if sources.is_empty() {
        return Verdict::undetermined(format!(
            "sitecraft check security: no source files match web_globs {:?} under {} (node_modules and dist excluded), so nothing was scanned for dangerous sinks; fix web_globs in {}",
            config.web_globs,
            root.display(),
            config::FILE_NAME
        ));
    }
    sources.sort();
    let mut findings = Vec::new();
    for f in &sources {
        let text = match read_lossy(f) {
            Ok(t) => t,
            Err(e) => {
                return Verdict::undetermined(format!(
                    "sitecraft check security: {e}; fix permissions"
                ))
            }
        };
        for (i, line) in text.lines().enumerate() {
            let n = i + 1;
            for c in sk.inner_html.captures_iter(line) {
                let op = c.get(1).map_or("", |m| m.as_str());
                if op != "=" && op != "+=" {
                    continue; // comparison, not assignment
                }
                let rhs = c.get(2).map_or("", |m| m.as_str()).trim();
                if !sk.constant.is_match(rhs) {
                    findings.push(Reason::new(format!(
                        "{}:{n}: innerHTML assigned a non-constant value; use textContent, or sanitize with a vetted sanitizer (e.g. DOMPurify) before assignment",
                        rel(root, f)
                    )));
                }
            }
            for (label, re) in &sk.others {
                if re.is_match(line) {
                    findings.push(Reason::new(format!(
                        "{}:{n}: dangerous sink {label}; replace it with a safe API (text binding / textContent, JSON.parse instead of eval) or sanitize the input with a vetted sanitizer",
                        rel(root, f)
                    )));
                }
            }
        }
    }
    Verdict::from_findings(findings)
}

// ---------------------------------------------------------------------------
// npm audit
// ---------------------------------------------------------------------------

fn audit_undet(why: impl std::fmt::Display) -> Verdict {
    Verdict::undetermined(format!(
        "sitecraft check security: npm audit could not be judged: {why}; make sure npm is installed and a package-lock.json exists (npm i --package-lock-only), the registry is reachable, then re-run"
    ))
}

fn audit(root: &Path) -> Verdict {
    let npm = crate::program_from_env(crate::ENV_NPM, "npm");
    let mut child = match Command::new(&npm)
        .args(["audit", "--json"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return audit_undet(format!("cannot run {}: {e}", npm.to_string_lossy())),
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return audit_undet("no stdout pipe");
    };
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).map(|_| buf)
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if start.elapsed() > AUDIT_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return audit_undet(format!("timed out after {}s", AUDIT_TIMEOUT.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return audit_undet(format!("cannot wait for npm: {e}"));
            }
        }
    };
    let bytes = match reader.join() {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return audit_undet(format!("cannot read npm output: {e}")),
        Err(_) => return audit_undet("output reader panicked"),
    };
    // npm audit exits 1 whenever vulnerabilities exist; the JSON decides.
    // Anything other than 0/1 (signal, 97, ...) is not an audit result.
    match status.code() {
        Some(0 | 1) => {}
        other => return audit_undet(format!("npm exited with {other:?}")),
    }
    judge_audit(&bytes)
}

fn judge_audit(bytes: &[u8]) -> Verdict {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return audit_undet("empty output");
    }
    let v: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => return audit_undet(format!("output is not valid JSON: {e}")),
    };
    if let Some(err) = v.get("error") {
        let code = err
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or("unknown");
        return audit_undet(format!("npm reported an error ({code})"));
    }
    let Some(vulns) = v
        .get("metadata")
        .and_then(|m| m.get("vulnerabilities"))
        .and_then(|x| x.as_object())
    else {
        return audit_undet("JSON has no metadata.vulnerabilities object");
    };
    let mut counts = [0u64; 6];
    for (slot, key) in counts
        .iter_mut()
        .zip(["info", "low", "moderate", "high", "critical", "total"])
    {
        match vulns.get(key).and_then(|n| n.as_u64()) {
            Some(n) => *slot = n,
            None => {
                return audit_undet(format!(
                    "metadata.vulnerabilities.{key} is missing or not a count"
                ))
            }
        }
    }
    let (high, critical) = (counts[3], counts[4]);
    if high > 0 || critical > 0 {
        Verdict::violation(format!(
            "npm audit reports {high} high and {critical} critical vulnerabilities; run `npm audit` for details, then `npm audit fix` or upgrade the affected packages"
        ))
    } else {
        Verdict::from_findings(vec![])
    }
}
