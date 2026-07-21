//! Built-in structural / pattern checks plus the config-driven custom-rule
//! engine. Each `check_*` pushes `Issue`s onto the shared list. The line/area
//! numbering in comments refers to the original PowerShell hook's checks so the
//! port stays auditable.

pub mod linters;
pub mod review;

use crate::classify::{ext_of, norm, Classifier};
use crate::config::{Config, Severity};
use crate::git;
use crate::model::Issue;
use globset::{Glob, GlobMatcher};
use regex::Regex;
use std::cell::RefCell;
use std::path::Path;

/// Everything a check needs. Built once in main.
pub struct Ctx<'a> {
    pub root: &'a Path,
    pub cfg: &'a Config,
    pub cls: &'a Classifier<'a>,
    pub files: &'a [String],
    /// Fail-closed latch: git errors hit mid-scan (a file whose diff could not be
    /// read, a grep that errored) are recorded here so the audit BLOCKS on an
    /// incomplete scan instead of silently missing patterns it could not read.
    pub incomplete: &'a RefCell<Vec<String>>,
}

impl<'a> Ctx<'a> {
    /// Added lines for `file`, latching any git error (fail-closed: an unscannable
    /// file must not silently contribute zero added lines to the pattern scan).
    fn added_lines(&self, file: &str) -> Vec<String> {
        match git::added_lines(self.root, file) {
            Ok(v) => v,
            Err(e) => {
                self.incomplete
                    .borrow_mut()
                    .push(format!("diff of {file}: {e}"));
                Vec::new()
            }
        }
    }

    /// `git grep` for `pattern`, latching any git error (fail-closed).
    fn grep_files(&self, pattern: &str) -> Vec<String> {
        match git::grep_files(self.root, pattern) {
            Ok(v) => v,
            Err(e) => {
                self.incomplete
                    .borrow_mut()
                    .push(format!("grep {pattern}: {e}"));
                Vec::new()
            }
        }
    }

    pub fn sources(&self) -> Vec<&'a str> {
        self.files
            .iter()
            .filter(|f| self.cls.is_source(f))
            .map(|s| s.as_str())
            .collect()
    }

    pub fn tests(&self) -> Vec<&'a str> {
        self.files
            .iter()
            .filter(|f| self.cls.is_test(f))
            .map(|s| s.as_str())
            .collect()
    }

    fn exists(&self, file: &str) -> bool {
        self.root.join(file).exists()
    }

    fn read_head(&self, file: &str, n: usize) -> String {
        let path = self.root.join(file);
        match std::fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes)
                .lines()
                .take(n)
                .collect::<Vec<_>>()
                .join("\n"),
            Err(_) => String::new(),
        }
    }

    fn read_full(&self, file: &str) -> Option<String> {
        std::fs::read(self.root.join(file))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }
}

fn re(pat: &str) -> Regex {
    Regex::new(pat).expect("static regex")
}

/// File-level suppression: `audit-ignore-file: <reason>` in the head.
fn head_suppressed(head: &str) -> bool {
    re(r"audit-ignore-file:\s*\S").is_match(head)
}

// ---------------------------------------------------------------------------
// Check 1: source changed without any test changed
// ---------------------------------------------------------------------------
pub fn check_missing_test(ctx: &Ctx, out: &mut Vec<Issue>) {
    let sources = ctx.sources();
    let tests = ctx.tests();
    if !sources.is_empty() && tests.is_empty() {
        let mut listed: Vec<&str> = sources.iter().take(5).copied().collect();
        let mut s = listed.join(", ");
        if sources.len() > 5 {
            s.push_str(&format!(" (+{} more)", sources.len() - 5));
        }
        listed.clear();
        out.push(Issue::block(
            "TEST MISSING",
            format!("TEST MISSING: source files changed but no test files changed/added: {s}"),
        ));
    }
}

// ---------------------------------------------------------------------------
// Checks 2a-2c: red-flag patterns over the merged set of added lines.
// (Project-specific 2d/.env and 2g/namespace live as custom [[rule]] entries.)
// ---------------------------------------------------------------------------

/// Collect added lines across all scannable, non-suppressed files.
fn merged_added_lines(ctx: &Ctx) -> Vec<String> {
    let mut all = Vec::new();
    for file in ctx.files {
        if ctx.cls.is_excluded(file) {
            continue;
        }
        if !ctx.cls.is_scannable(file) {
            continue;
        }
        if ctx.exists(file) && head_suppressed(&ctx.read_head(file, 20)) {
            continue;
        }
        all.extend(ctx.added_lines(file));
    }
    all
}

pub fn check_hardcoded_ip(ctx: &Ctx, added: &[String], out: &mut Vec<Issue>) {
    let ip_re = re(r"\b(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})\b");
    let comment_re = re(r"^\+\s*(#|//|--|\*)");
    let benign = &ctx.cfg.hardcoded_ip.benign;
    let mut hits: Vec<String> = Vec::new();
    for line in added {
        if comment_re.is_match(line) {
            continue;
        }
        for m in ip_re.find_iter(line) {
            let ip = m.as_str();
            if benign.iter().any(|b| ip.starts_with(b.as_str())) {
                continue;
            }
            hits.push(format!("  {ip}  ::  {}", line.trim()));
        }
    }
    if !hits.is_empty() {
        let sample = hits.iter().take(3).cloned().collect::<Vec<_>>().join("\n");
        out.push(Issue::block(
            "HARD-CODED IP",
            format!("HARD-CODED IP detected in added lines:\n{sample}"),
        ));
    }
}

pub fn check_hardcoded_secret(ctx: &Ctx, added: &[String], out: &mut Vec<Issue>) {
    let secret_re = re(
        r#"(?i)(password|passwd|secret|api[_-]?key|token|private[_-]?key)\s*=\s*["'][^"'`${}\s][^"']{4,}["']"#,
    );
    let comment_re = re(r"^\+\s*(#|//)");
    let allow = &ctx.cfg.hardcoded_secret.allow;
    let mut hits: Vec<String> = Vec::new();
    for line in added {
        if comment_re.is_match(line) {
            continue;
        }
        if secret_re.is_match(line) {
            if allow.iter().any(|a| line.contains(a.as_str())) {
                continue;
            }
            hits.push(format!("  {}", line.trim()));
        }
    }
    if !hits.is_empty() {
        let sample = hits.iter().take(3).cloned().collect::<Vec<_>>().join("\n");
        out.push(Issue::block(
            "POSSIBLE HARD-CODED SECRET",
            format!("POSSIBLE HARD-CODED SECRET:\n{sample}"),
        ));
    }
}

pub fn check_swallowed_error(ctx: &Ctx, added: &[String], out: &mut Vec<Issue>) {
    let base = re(
        r"^\+\s*(except\s*:|except\s+Exception\s*:\s*$|except\s+Exception\s+as\s+\w+\s*:\s*pass|\|\|\s*true\s*$)",
    );
    let extra: Vec<Regex> = ctx
        .cfg
        .swallowed_error
        .extra_patterns
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();
    let mut hits: Vec<String> = Vec::new();
    for line in added {
        if base.is_match(line) || extra.iter().any(|r| r.is_match(line)) {
            hits.push(format!("  {}", line.trim()));
        }
    }
    if !hits.is_empty() {
        let sample = hits.iter().take(3).cloned().collect::<Vec<_>>().join("\n");
        out.push(Issue::block(
            "FALL-THROUGH / SWALLOWED ERROR",
            format!("FALL-THROUGH / SWALLOWED ERROR added:\n{sample}"),
        ));
    }
}

// ---------------------------------------------------------------------------
// Check 2e: duplicate function definitions (heuristic shared-code reuse)
// ---------------------------------------------------------------------------
pub fn check_duplicate_function(ctx: &Ctx, out: &mut Vec<Issue>) {
    let py_def = re(r"^\+\s*def\s+([A-Za-z_][A-Za-z0-9_]{3,})\s*\(");
    let js_def =
        re(r"^\+\s*(?:export\s+)?(?:async\s+)?function\s+([A-Za-z_][A-Za-z0-9_]{3,})\s*\(");
    let sh_def = re(r"^\+\s*(?:function\s+)?([A-Za-z_][A-Za-z0-9_]{3,})\s*\(\)\s*\{?");
    let common = &ctx.cfg.duplicate_function.common_names;
    let mut hits: Vec<String> = Vec::new();

    for file in ctx.files {
        if ctx.cls.is_excluded(file) || ctx.cls.is_test(file) || !ctx.exists(file) {
            continue;
        }
        let ext = match ext_of(file) {
            Some(e) => e,
            None => continue,
        };
        let added = ctx.added_lines(file);
        let def_re = match ext.as_str() {
            ".py" => &py_def,
            ".ts" | ".tsx" | ".js" | ".jsx" => &js_def,
            ".sh" => &sh_def,
            _ => continue,
        };
        let mut names: Vec<String> = Vec::new();
        for ln in &added {
            if let Some(c) = def_re.captures(ln) {
                names.push(c[1].to_string());
            }
        }
        names.sort();
        names.dedup();
        for name in &names {
            if common.iter().any(|c| c == name) {
                continue;
            }
            let esc = regex::escape(name);
            let pat = if ext == ".py" {
                format!(r"(^|\s)def\s+{esc}\s*\(")
            } else {
                format!(r"(^|\s)(function\s+)?{esc}\s*\(")
            };
            let others: Vec<String> = ctx
                .grep_files(&pat)
                .into_iter()
                .filter(|f| {
                    let f = norm(f);
                    f != norm(file) && !ctx.cls.is_test(&f) && !ctx.cls.is_excluded(&f)
                })
                .collect();
            if !others.is_empty() {
                let listed = others
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                hits.push(format!(
                    "  {name}()  defined in {file}; same name already in: {listed}"
                ));
            }
        }
    }
    if !hits.is_empty() {
        let sample = hits.iter().take(5).cloned().collect::<Vec<_>>().join("\n");
        out.push(Issue::block(
            "POSSIBLE DUPLICATE",
            format!(
                "POSSIBLE DUPLICATE function/signature (consider reusing existing impl):\n{sample}"
            ),
        ));
    }
}

// ---------------------------------------------------------------------------
// Check 2f: `local VAR=$(...)` in a `set -e` script (silent failure)
// ---------------------------------------------------------------------------
pub fn check_local_capture(ctx: &Ctx, out: &mut Vec<Issue>) {
    let errexit_a = re(r"(?m)^\s*set\s+-[A-Za-z]*e");
    let errexit_b = re(r"(?m)^\s*set\s+-o\s+errexit");
    let pat = re(
        r"^\+\s*(local|declare|readonly|export|typeset)(\s+-\w+)?\s+[A-Za-z_][A-Za-z0-9_]*=(\$\(|`)",
    );
    let mut hits: Vec<String> = Vec::new();
    'outer: for file in ctx.files {
        if ctx.cls.is_excluded(file) || ctx.cls.is_test(file) || !ctx.exists(file) {
            continue;
        }
        if ext_of(file).as_deref() != Some(".sh") {
            continue;
        }
        let head = ctx.read_head(file, 30);
        if head.is_empty() {
            continue;
        }
        if !(errexit_a.is_match(&head) || errexit_b.is_match(&head)) {
            continue;
        }
        if head_suppressed(&head) {
            continue;
        }
        for ln in ctx.added_lines(file) {
            if pat.is_match(&ln) {
                hits.push(format!("  {file}: {}", ln.trim_start_matches('+').trim()));
                if hits.len() >= 8 {
                    break 'outer;
                }
            }
        }
    }
    if !hits.is_empty() {
        let sample = hits.iter().take(8).cloned().collect::<Vec<_>>().join("\n");
        let mut msg = format!(
            "LOCAL=$(...) IN `set -e` SCRIPT (silent failure — bash builtin returns 0 even when $(cmd) fails):\n{sample}\nFix: split the declaration from the capture so set -e sees the rc.\n    BAD : local x=$(some_cmd)\n    GOOD: local x\n          x=$(some_cmd)"
        );
        if !ctx.cfg.local_capture.doc_ref.is_empty() {
            msg.push_str(&format!("\nSee {}.", ctx.cfg.local_capture.doc_ref));
        }
        out.push(Issue::block("LOCAL", msg));
    }
}

// ---------------------------------------------------------------------------
// Check 3d: broken markdown links
// ---------------------------------------------------------------------------
pub fn check_markdown_links(ctx: &Ctx, out: &mut Vec<Issue>) {
    let link_re = re(r"\[[^\]]*\]\(([^)]+)\)");
    let skip_re = re(r"^(https?:|mailto:|#|<|\$)");
    let mut broken: Vec<String> = Vec::new();
    'outer: for file in ctx.files {
        if ctx.cls.is_excluded(file) || ext_of(file).as_deref() != Some(".md") || !ctx.exists(file)
        {
            continue;
        }
        let content = match ctx.read_full(file) {
            Some(c) => c,
            None => continue,
        };
        let file_norm = norm(file);
        let file_dir = match file_norm.rfind('/') {
            Some(i) => &file_norm[..i],
            None => ".",
        };
        for cap in link_re.captures_iter(&content) {
            let mut target = cap[1].trim().to_string();
            target = target.split_whitespace().next().unwrap_or("").to_string();
            if target.is_empty() || skip_re.is_match(&target) {
                continue;
            }
            target = target.split(['#', '?']).next().unwrap_or("").to_string();
            if target.is_empty() {
                continue;
            }
            let resolved = if let Some(stripped) = target.strip_prefix('/') {
                ctx.root.join(stripped)
            } else {
                ctx.root.join(file_dir).join(&target)
            };
            if !resolved.exists() {
                broken.push(format!("  {file} -> {target}"));
                if broken.len() >= 8 {
                    break 'outer;
                }
            }
        }
    }
    if !broken.is_empty() {
        let sample = broken
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        out.push(Issue::block(
            "BROKEN MARKDOWN LINKS",
            format!("BROKEN MARKDOWN LINKS in changed .md files:\n{sample}"),
        ));
    }
}

// ---------------------------------------------------------------------------
// Check 3g: line endings (CRLF vs LF per extension)
// ---------------------------------------------------------------------------
pub fn check_line_endings(ctx: &Ctx, out: &mut Vec<Issue>) {
    let mut fails: Vec<String> = Vec::new();
    for file in ctx.files {
        if ctx.cls.is_excluded(file) || !ctx.exists(file) {
            continue;
        }
        let ext = match ext_of(file) {
            Some(e) => e,
            None => continue,
        };
        let want_crlf = ctx.cfg.line_endings.crlf_exts.iter().any(|e| e == &ext);
        let want_lf = ctx.cfg.line_endings.lf_exts.iter().any(|e| e == &ext);
        if !want_crlf && !want_lf {
            continue;
        }
        let bytes = match std::fs::read(ctx.root.join(file)) {
            Ok(b) if !b.is_empty() => b,
            _ => continue,
        };
        let mut has_crlf = false;
        for w in bytes.windows(2) {
            if w[0] == 0x0D && w[1] == 0x0A {
                has_crlf = true;
                break;
            }
        }
        let mut has_lf_only = false;
        for (i, &b) in bytes.iter().enumerate() {
            if b == 0x0A && (i == 0 || bytes[i - 1] != 0x0D) {
                has_lf_only = true;
                break;
            }
        }
        if want_crlf && has_lf_only && !has_crlf {
            fails.push(format!(
                "  {file}: LF-only but must be CRLF (Windows script)"
            ));
        } else if want_lf && has_crlf {
            fails.push(format!(
                "  {file}: contains CRLF but must be LF (POSIX shebang)"
            ));
        }
    }
    if !fails.is_empty() {
        out.push(Issue::block(
            "LINE ENDING MISMATCH",
            format!(
                "LINE ENDING MISMATCH:\n{}\nFix: \"dos2unix <file>\" for LF, or rewrite with CRLF.",
                fails.join("\n")
            ),
        ));
    }
}

// ---------------------------------------------------------------------------
// Check 6: file length (WARNING only)
// ---------------------------------------------------------------------------
pub fn check_file_length(ctx: &Ctx, out: &mut Vec<Issue>) {
    let limit = ctx.cfg.file_length.limit;
    let mut warnings: Vec<String> = Vec::new();
    for file in ctx.files {
        if !ctx.cls.is_source(file) || !ctx.exists(file) {
            continue;
        }
        if head_suppressed(&ctx.read_head(file, 20)) {
            continue;
        }
        let count = match std::fs::read(ctx.root.join(file)) {
            Ok(b) => String::from_utf8_lossy(&b).lines().count(),
            Err(_) => continue,
        };
        if count > limit {
            warnings.push(format!("  {file}: {count} lines (> {limit})"));
        }
    }
    if !warnings.is_empty() {
        let sample = warnings
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        out.push(Issue::warn(
            "FILE TOO LONG",
            format!(
                "FILE TOO LONG (> {limit} lines — consider splitting):\n{sample}\nWarning only; add 'audit-ignore-file: <reason>' in the first 20 lines to silence."
            ),
        ));
    }
}

// ---------------------------------------------------------------------------
// Config-driven custom rules (project-specific policy)
// ---------------------------------------------------------------------------

struct CompiledRule {
    id: String,
    pattern: Regex,
    unless: Vec<Regex>,
    include: Vec<GlobMatcher>,
    exclude: Vec<GlobMatcher>,
    skip_comments: bool,
    severity: Severity,
    message: String,
}

fn glob(p: &str) -> Option<GlobMatcher> {
    Glob::new(p).ok().map(|g| g.compile_matcher())
}

pub fn check_custom_rules(ctx: &Ctx, out: &mut Vec<Issue>) {
    let comment_re = re(r"^\+\s*(#|//|--|\*|;)");
    let mut compiled: Vec<CompiledRule> = Vec::new();
    for r in &ctx.cfg.rules {
        let pattern = match Regex::new(&r.pattern) {
            Ok(re) => re,
            Err(e) => {
                eprintln!("precommit-audit: rule '{}' has invalid pattern: {e}", r.id);
                continue;
            }
        };
        compiled.push(CompiledRule {
            id: r.id.clone(),
            pattern,
            unless: r.unless.iter().filter_map(|u| Regex::new(u).ok()).collect(),
            include: r.include_globs.iter().filter_map(|g| glob(g)).collect(),
            exclude: r.exclude_globs.iter().filter_map(|g| glob(g)).collect(),
            skip_comments: r.skip_comments,
            severity: r.severity,
            message: r.message.clone(),
        });
    }
    if compiled.is_empty() {
        return;
    }

    // hits[i] = matched lines for rule i.
    let mut hits: Vec<Vec<String>> = vec![Vec::new(); compiled.len()];
    for file in ctx.files {
        if ctx.cls.is_excluded(file) {
            continue;
        }
        let fnorm = norm(file);
        // Which rules apply to this file (glob scoping)?
        let applicable: Vec<usize> = (0..compiled.len())
            .filter(|&i| {
                let r = &compiled[i];
                let inc = r.include.is_empty() || r.include.iter().any(|g| g.is_match(&fnorm));
                let exc = r.exclude.iter().any(|g| g.is_match(&fnorm));
                inc && !exc
            })
            .collect();
        if applicable.is_empty() {
            continue;
        }
        let added = ctx.added_lines(file);
        for ln in &added {
            for &i in &applicable {
                let r = &compiled[i];
                if hits[i].len() >= 8 {
                    continue;
                }
                if r.skip_comments && comment_re.is_match(ln) {
                    continue;
                }
                if r.pattern.is_match(ln) && !r.unless.iter().any(|u| u.is_match(ln)) {
                    hits[i].push(format!("  {file}: {}", ln.trim_start_matches('+').trim()));
                }
            }
        }
    }
    for (i, r) in compiled.iter().enumerate() {
        if hits[i].is_empty() {
            continue;
        }
        let sample = hits[i]
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let cat = r.id.to_uppercase();
        let msg = format!("{}:\n{sample}\n{}", cat, r.message);
        out.push(Issue {
            category: cat,
            message: msg,
            severity: r.severity,
        });
    }
}

/// Run all enabled checks (excludes linters and the review contract, which the
/// caller drives separately so they can be toggled / measured independently).
pub fn run_static_checks(ctx: &Ctx, out: &mut Vec<Issue>) {
    let c = &ctx.cfg.checks;
    if c.missing_test {
        check_missing_test(ctx, out);
    }
    if c.hardcoded_ip || c.hardcoded_secret || c.swallowed_error {
        let added = merged_added_lines(ctx);
        if c.hardcoded_ip {
            check_hardcoded_ip(ctx, &added, out);
        }
        if c.hardcoded_secret {
            check_hardcoded_secret(ctx, &added, out);
        }
        if c.swallowed_error {
            check_swallowed_error(ctx, &added, out);
        }
    }
    if c.duplicate_function {
        check_duplicate_function(ctx, out);
    }
    if c.local_capture {
        check_local_capture(ctx, out);
    }
    if c.markdown_links {
        check_markdown_links(ctx, out);
    }
    if c.line_endings {
        check_line_endings(ctx, out);
    }
    if c.custom_rules {
        check_custom_rules(ctx, out);
    }
    if c.file_length {
        check_file_length(ctx, out);
    }
}
