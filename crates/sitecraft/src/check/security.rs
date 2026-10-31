//! `sitecraft check security` — block on:
//!
//! * secrets in build output (`build_dir`, including `*.map`): fixed-prefix
//!   patterns (AWS access key, GitHub tokens, Stripe secret keys, private-key
//!   blocks with a body, Google API keys); matches are printed redacted;
//! * values from the root `.env` / `.env.*` files (templates `.env.example`,
//!   `.env.sample`, `.env.template` excluded; keys `NODE_ENV`, `VITE_*`,
//!   `NEXT_PUBLIC_*`, `PUBLIC_*` excluded as public by convention), parsed
//!   with dotenv semantics, appearing in build output;
//! * dangerous HTML sinks in sources (`innerHTML` assignment of a
//!   non-constant, `dangerouslySetInnerHTML`, `v-html`, `{@html}`, `eval`,
//!   `new Function`), excluding `node_modules` and `dist`;
//! * `npm audit --json` (`$SITECRAFT_NPM` / `npm`) reporting high or critical
//!   vulnerabilities, judged from the JSON shape, not the exit code.
//!
//! Undetermined: audit JSON without all six `metadata.vulnerabilities` keys,
//! an `error` object (including ENOLOCK), empty or invalid stdout, missing
//! npm, zero files to scan, or a file walk exceeding its entry limit.

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

/// Upper bound on directory entries one walk visits. Aliased directories are
/// walked under every path (see [`walk_inner`]), so a chain of directories
/// each holding several links to the next grows exponentially; past this
/// bound the walk gives up with an error (Undetermined), never a partial list.
const MAX_WALK_ENTRIES: usize = 100_000;

/// Recursively list regular files under `dir`. `skip` decides, per directory
/// name, whether to prune. Any IO error, or visiting more than
/// [`MAX_WALK_ENTRIES`] entries, is returned as an error (never a partial list).
fn walk(
    dir: &Path,
    skip: &dyn Fn(&Path, &str) -> bool,
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let mut stack = Vec::new();
    let mut budget = MAX_WALK_ENTRIES;
    walk_inner(dir, skip, out, &mut stack, &mut budget).map_err(|e| {
        if budget == 0 {
            format!(
                "too many paths under {} (walk limit {MAX_WALK_ENTRIES} entries reached, likely a chain of directory symlinks or a large non-source directory), so the scan did not complete; remove the symlink aliases or move the large directory out of the project",
                dir.display()
            )
        } else {
            e
        }
    })
}

/// `stack` holds the canonical paths of the directories currently being
/// walked (the ancestors of `dir`, inclusive). A link back to one of them is
/// a loop whose contents are already being scanned; any other directory
/// (including an alias of a sibling) is walked, so every path under which a
/// file is reachable is reported. Callers dedupe by canonical file path
/// AFTER their own path filter.
fn walk_inner(
    dir: &Path,
    skip: &dyn Fn(&Path, &str) -> bool,
    out: &mut Vec<PathBuf>,
    stack: &mut Vec<PathBuf>,
    budget: &mut usize,
) -> Result<(), String> {
    let canon = std::fs::canonicalize(dir).map_err(|e| {
        format!(
            "cannot resolve directory {}: {e}; fix permissions or remove the broken link",
            dir.display()
        )
    })?;
    if stack.contains(&canon) {
        return Ok(()); // symlink loop: already being scanned via the ancestor
    }
    stack.push(canon);
    let rd = std::fs::read_dir(dir).map_err(|e| {
        format!(
            "cannot read directory {}: {e}; fix permissions",
            dir.display()
        )
    })?;
    for ent in rd {
        let ent =
            ent.map_err(|e| format!("cannot list {}: {e}; fix permissions", dir.display()))?;
        if *budget == 0 {
            return Err("walk limit reached".to_string());
        }
        *budget -= 1;
        let path = ent.path();
        // Follows symlinks; a dangling link is an error, not a skip.
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                // Hint only; both arms are errors (Undetermined) either way.
                let link_hint = matches!(
                    std::fs::symlink_metadata(&path),
                    Ok(m) if m.file_type().is_symlink()
                );
                return Err(if link_hint {
                    format!(
                        "dangling or unreadable symlink {}: {e}; remove the link or fix its target",
                        path.display()
                    )
                } else {
                    format!("cannot stat {}: {e}; fix permissions", path.display())
                });
            }
        };
        if meta.is_dir() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if skip(&path, &name) {
                continue;
            }
            walk_inner(&path, skip, out, stack, budget)?;
        } else if meta.is_file() {
            out.push(path);
        }
    }
    stack.pop();
    Ok(())
}

/// Keep the first path per canonical file (aliases reach the same file).
fn dedup_canonical(files: Vec<PathBuf>) -> Result<Vec<PathBuf>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for f in files {
        let c = std::fs::canonicalize(&f).map_err(|e| {
            format!(
                "cannot resolve {}: {e}; fix permissions or remove the broken link",
                f.display()
            )
        })?;
        if seen.insert(c) {
            out.push(f);
        }
    }
    Ok(out)
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
        let defs: [(&str, &str, bool); 5] = [
            ("AWS access key", r"AKIA[0-9A-Z]{16}", false),
            (
                "GitHub token",
                r"(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})",
                false,
            ),
            ("Stripe secret key", r"sk_(?:live|test)_[A-Za-z0-9]{24,}", false),
            ("Google API key", r"AIza[0-9A-Za-z_\-]{35}", false),
            (
                "private key block",
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?:\s|\\+[nr])*(?:Proc-Type|DEK-Info|[A-Za-z0-9+/]{16,})",
                true,
            ),
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
    if let Err(e) = walk(&build, &|_, _| false, &mut files).and_then(|()| {
        files = dedup_canonical(std::mem::take(&mut files))?;
        Ok(())
    }) {
        return Verdict::undetermined(format!("sitecraft check security: {e}"));
    }
    if files.is_empty() {
        return Verdict::undetermined(format!(
            "sitecraft check security: build_dir {} contains no files, so nothing was scanned for secrets; build the project first",
            build.display()
        ));
    }
    files.sort();
    let env_values = match env_candidates(root) {
        Ok(v) => v,
        Err(e) => return Verdict::undetermined(format!("sitecraft check security: {e}")),
    };
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
        for (env_file, key, value) in &env_values {
            if let Some(off) = text.find(value.as_str()) {
                findings.push(Reason::new(format!(
                    "{}:{}: the value of {key} from {env_file} appears in build output ({} chars, redacted); keep it server-side, remove it from client code, rotate it, and rebuild",
                    rel(root, f),
                    line_of(&text, off),
                    value.chars().count()
                )));
            }
        }
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

/// Committed-on-purpose placeholder files: never read.
const ENV_TEMPLATES: [&str; 3] = [".env.example", ".env.sample", ".env.template"];

/// Keys public by framework convention (bundled on purpose): `NODE_ENV` and
/// the `VITE_` / `NEXT_PUBLIC_` / `PUBLIC_` prefixes.
fn is_public_env_key(key: &str) -> bool {
    key == "NODE_ENV"
        || ["VITE_", "NEXT_PUBLIC_", "PUBLIC_"]
            .iter()
            .any(|p| key.starts_with(p))
}

/// The `.env` files at the project root: `.env` and every `.env.*` except
/// [`ENV_TEMPLATES`], sorted. A root that cannot be listed is an error.
fn env_files(root: &Path) -> Result<Vec<String>, String> {
    let rd = std::fs::read_dir(root).map_err(|e| {
        format!(
            "cannot list {} to find .env files: {e}; fix permissions",
            root.display()
        )
    })?;
    let mut names = Vec::new();
    for ent in rd {
        let ent = ent.map_err(|e| {
            format!(
                "cannot list {} to find .env files: {e}; fix permissions",
                root.display()
            )
        })?;
        let name = ent.file_name().to_string_lossy().into_owned();
        if (name == ".env" || name.starts_with(".env.")) && !ENV_TEMPLATES.contains(&name.as_str())
        {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// Parse dotenv text into `(key, value)` pairs, following dotenv semantics:
/// lines end at `\r\n`, `\n` or a lone `\r`; `export ` is optional; the
/// separator is `=` or `:`; a value in matching `'`, `"` or `` ` `` quotes is
/// the quoted content (a `#` inside it is part of the value, and a double
/// quoted value may span lines); an unquoted value ends at the first `#`
/// and is trimmed.
fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_start();
        i += 1;
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let key_len = line
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
            .unwrap_or(line.len());
        if key_len == 0 {
            continue; // blank, comment, or not a KEY=VALUE line
        }
        let key = &line[..key_len];
        let rest = line[key_len..].trim_start();
        let Some(value) = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':')) else {
            continue;
        };
        let value = value.trim_start();
        let quote = value
            .chars()
            .next()
            .filter(|c| matches!(c, '"' | '\'' | '`'));
        let parsed = match quote {
            Some(q) => {
                let body = &value[1..];
                if let Some(end) = body.find(q) {
                    Some(body[..end].to_string())
                } else if q == '"' {
                    // multi-line double-quoted value: join until the closing quote
                    let mut acc = body.to_string();
                    let mut j = i;
                    let mut closed = None;
                    while j < lines.len() {
                        acc.push('\n');
                        if let Some(end) = lines[j].find(q) {
                            acc.push_str(&lines[j][..end]);
                            closed = Some(j + 1);
                            break;
                        }
                        acc.push_str(lines[j]);
                        j += 1;
                    }
                    closed.map(|next| {
                        i = next;
                        acc
                    })
                } else {
                    None
                }
            }
            None => None,
        };
        let v = match parsed {
            Some(v) => v,
            // unquoted (or unterminated quote, kept literally like dotenv)
            None => value.split('#').next().unwrap_or("").trim().to_string(),
        };
        out.push((key.to_string(), v));
    }
    out
}

/// `(file, key, value)` for every root `.env` / `.env.*` value (templates
/// excluded) that is a leak candidate: key not public by convention, value at
/// least 8 chars, not boolean, not numeric. A present-but-unreadable file is
/// an error (its values were never compared).
fn env_candidates(root: &Path) -> Result<Vec<(String, String, String)>, String> {
    let mut out = Vec::new();
    for name in env_files(root)? {
        let path = root.join(&name);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "{} is a dangling symlink; remove the link or fix its target",
                    path.display()
                ))
            }
            Err(e) => {
                return Err(format!(
                    "cannot read {}: {e}; fix its permissions (its values were not compared with the build output)",
                    path.display()
                ))
            }
        };
        for (key, v) in parse_dotenv(&String::from_utf8_lossy(&bytes)) {
            if is_public_env_key(&key) || v.chars().count() < 8 || is_trivial_env_value(&v) {
                continue;
            }
            out.push((name.clone(), key, v));
        }
    }
    Ok(out)
}

fn is_trivial_env_value(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    matches!(l.as_str(), "true" | "false" | "yes" | "no" | "on" | "off")
        || l.parse::<f64>().is_ok()
        || l.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
}

// ---------------------------------------------------------------------------
// dangerous sinks
// ---------------------------------------------------------------------------

/// Sink patterns. The assignment / call patterns are matched against the
/// whole file (so Prettier-style multi-line forms are seen); the value they
/// receive is the first non-blank text after the operator or argument
/// separator, up to the end of that line ([`value_after`]).
struct Sinks {
    inner_html: Regex,
    constant: Regex,
    obj_assign: Regex,
    obj_html: Regex,
    adjacent: Regex,
    doc_write: Regex,
    others: Vec<(&'static str, Regex)>,
}

fn sinks() -> Result<&'static Sinks, String> {
    static S: OnceLock<Result<Sinks, String>> = OnceLock::new();
    S.get_or_init(|| {
        let mk =
            |p: &str| Regex::new(p).map_err(|e| format!("internal sink pattern {p} invalid: {e}"));
        Ok(Sinks {
            inner_html: mk(r#"\b((?:inner|outer)HTML)(?:['"`]\s*\])?\s*(\+=|\|\|=|&&=|\?\?=|=+)"#)?,
            constant: mk(r#"^(?:"[^"\\]*"|'[^'\\]*'|`[^`$\\]*`)[\s;,)}]*$"#)?,
            obj_assign: mk(r"\bObject\s*\.\s*assign\s*\(")?,
            obj_html: mk(r#"\b((?:inner|outer)HTML)\s*:"#)?,
            adjacent: mk(r"\binsertAdjacentHTML\s*\(")?,
            doc_write: mk(r"\bdocument\s*\.\s*write(?:ln)?\s*\(")?,
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

/// The value text starting at byte `from`: skip whitespace (including line
/// breaks), then everything up to the end of that line.
fn value_after(text: &str, from: usize) -> &str {
    let rest = text[from..].trim_start();
    rest.split('\n').next().unwrap_or("").trim()
}

/// Scan JS from byte `from` (just inside an opening bracket), skipping string
/// literals and nested brackets. Returns `(index, byte)` of the first
/// top-level `,` or the closing bracket, or `None` at end of text.
fn scan_top_level(text: &str, from: usize) -> Option<(usize, u8)> {
    let b = text.as_bytes();
    let mut depth = 0usize;
    let mut i = from;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            c @ (b')' | b']' | b'}') => {
                if depth == 0 {
                    return Some((i, c));
                }
                depth -= 1;
            }
            b',' if depth == 0 => return Some((i, b',')),
            _ => {}
        }
        i += 1;
    }
    None
}

/// End (exclusive) of the call whose arguments start at `from`; end of text
/// when unbalanced, so an unterminated call is still scanned to the end.
fn call_end(text: &str, from: usize) -> usize {
    let mut i = from;
    loop {
        match scan_top_level(text, i) {
            Some((j, b',')) => i = j + 1,
            Some((j, _)) => return j,
            None => return text.len(),
        }
    }
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
        return Verdict::undetermined(format!("sitecraft check security: {e}"));
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
    let sources = match dedup_canonical(sources) {
        Ok(v) => v,
        Err(e) => return Verdict::undetermined(format!("sitecraft check security: {e}")),
    };
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
        let file = rel(root, f);
        for c in sk.inner_html.captures_iter(&text) {
            let (Some(whole), Some(prop), Some(op)) = (c.get(0), c.get(1), c.get(2)) else {
                continue;
            };
            if op.as_str() == "==" || op.as_str() == "===" {
                continue; // comparison, not assignment
            }
            if !sk.constant.is_match(value_after(&text, whole.end())) {
                findings.push(Reason::new(format!(
                    "{file}:{}: {} assigned a non-constant value; use textContent, or sanitize with a vetted sanitizer (e.g. DOMPurify) before assignment",
                    line_of(&text, whole.start()),
                    prop.as_str()
                )));
            }
        }
        for call in sk.obj_assign.find_iter(&text) {
            let span = &text[call.end()..call_end(&text, call.end())];
            for c in sk.obj_html.captures_iter(span) {
                let (Some(whole), Some(prop)) = (c.get(0), c.get(1)) else {
                    continue;
                };
                if !sk.constant.is_match(value_after(span, whole.end())) {
                    findings.push(Reason::new(format!(
                        "{file}:{}: Object.assign sets {} to a non-constant value; use textContent, or sanitize with a vetted sanitizer (e.g. DOMPurify)",
                        line_of(&text, call.end() + whole.start()),
                        prop.as_str()
                    )));
                }
            }
        }
        for (label, re, skip_first_arg) in [
            ("insertAdjacentHTML", &sk.adjacent, true),
            ("document.write", &sk.doc_write, false),
        ] {
            for m in re.find_iter(&text) {
                let arg_start = if skip_first_arg {
                    match scan_top_level(&text, m.end()) {
                        Some((comma, b',')) => Some(comma + 1),
                        // no second argument found: the HTML argument cannot be
                        // shown constant, so it is reported (restrictive side)
                        _ => None,
                    }
                } else {
                    Some(m.end())
                };
                let constant =
                    arg_start.is_some_and(|s| sk.constant.is_match(value_after(&text, s)));
                if !constant {
                    findings.push(Reason::new(format!(
                        "{file}:{}: {label} called with a non-constant argument; use textContent / DOM construction, or sanitize with a vetted sanitizer (e.g. DOMPurify)",
                        line_of(&text, m.start())
                    )));
                }
            }
        }
        for (i, line) in text.lines().enumerate() {
            let n = i + 1;
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

/// How long the output pipe may stay open after npm itself has exited. A
/// grandchild still holding stdout means the output is not known complete.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// True once the child has exited but is NOT yet reaped (`WNOWAIT`), so its
/// pid/pgid cannot be recycled while we still signal the group.
fn exited_unreaped(pid: u32) -> Result<bool, String> {
    // SAFETY: waitid(2) with a zeroed siginfo_t we own; WNOWAIT leaves the
    // child waitable, WNOHANG makes it non-blocking.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(info.si_signo != 0)
}

/// SIGKILL npm's process group. Call only while the leader is unreaped (or
/// still running) so the group id cannot belong to anyone else.
fn kill_group(child: &std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: plain kill(2) on the process group we created.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

fn audit(root: &Path) -> Verdict {
    use std::os::unix::process::CommandExt;

    let npm = crate::program_from_env(crate::ENV_NPM, "npm");
    let mut child = match Command::new(&npm)
        .args(["audit", "--json"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return audit_undet(format!("cannot run {}: {e}", npm.to_string_lossy())),
    };
    let Some(mut stdout) = child.stdout.take() else {
        kill_group(&child);
        let _ = child.wait();
        return audit_undet("no stdout pipe");
    };
    let (tx, rx) = std::sync::mpsc::channel();
    // Detached on purpose: if the pipe never closes we do not join it.
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let r = stdout.read_to_end(&mut buf).map(|_| buf);
        let _ = tx.send(r);
    });
    let start = Instant::now();
    loop {
        match exited_unreaped(child.id()) {
            Ok(true) => break,
            Ok(false) => {
                if start.elapsed() > AUDIT_TIMEOUT {
                    kill_group(&child);
                    let _ = child.wait();
                    return audit_undet(format!("timed out after {}s", AUDIT_TIMEOUT.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                kill_group(&child);
                let _ = child.wait();
                return audit_undet(format!("cannot wait for npm: {e}"));
            }
        }
    }
    let received = rx.recv_timeout(PIPE_GRACE);
    // npm is a zombie here, so its group id is still ours: clean up the group.
    kill_group(&child);
    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => return audit_undet(format!("cannot reap npm: {e}")),
    };
    let bytes = match received {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return audit_undet(format!("cannot read npm output: {e}")),
        Err(_) => {
            return audit_undet(
                "npm exited but its output pipe stayed open (a background process holds stdout), so the output is not known complete",
            )
        }
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
