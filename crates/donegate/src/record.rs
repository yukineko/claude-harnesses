//! The session record of a failing check's full output, and the short,
//! decision-relevant digest that is DISPLAYED instead of the raw output.
//!
//! User ruling (verbatim): 「stderrをsession記録としてもっておくのはいい、でも判断を
//! うながすのでなければ表示するのはノイズにすぎない。ログがおおいと大事な情報が流れる」.
//! So the full raw output of every failing check is KEPT, byte for byte, in a
//! durable per-session file, and the block reason shows only what prompts a
//! decision (failing test names, panic locations + messages, the summary line)
//! plus the absolute path of that file.
//!
//! ## Where records live, and their retention
//!
//! `<state_dir>/records/<session>/<unix-ms>-a<attempt>-<check>.log`, where
//! `<session>` is the sanitised session key (`nosession` if it sanitises to
//! nothing) and `<check>` the sanitised check name. After each save, retention
//! keeps the newest [`KEEP_SESSIONS`] session directories (by mtime) and the
//! newest [`KEEP_FILES_PER_SESSION`] records inside the current session. The
//! current session's directory is never deleted, and the record just written is
//! never deleted (it is the newest). Retention errors other than `NotFound` are
//! not swallowed: they are carried in [`Record::Saved`] and displayed as
//! `record retention failed: …`.
//!
//! ## Fail-closed when the record cannot be saved
//!
//! A record that cannot be saved does not change the verdict (the check failed;
//! the stop still blocks). It changes the display: the reason says plainly that
//! the full output could not be saved and why, and shows MORE — the digest plus
//! the raw tail of `max(output_tail_lines, FALLBACK_MIN_LINES)` lines — never less
//! with no record (CLAUDE.md §3/§4).

use std::path::{Path, PathBuf};

/// Session directories kept under `<state_dir>/records/` (newest by mtime).
pub const KEEP_SESSIONS: usize = 20;
/// Records kept inside the current session's directory (newest by name, which
/// starts with the unix-ms timestamp).
pub const KEEP_FILES_PER_SESSION: usize = 50;
/// The raw tail shown when no record could be kept is at least this long.
pub const FALLBACK_MIN_LINES: usize = 40;
/// Upper bound on the digest lines displayed per failing check.
pub const DIGEST_MAX_LINES: usize = 24;
/// Lines shown for output with no recognisable test structure.
const PLAIN_TAIL_LINES: usize = 5;
/// Message lines kept after each `panicked at` line.
const PANIC_MSG_MAX: usize = 4;
const FAILURE_NAMES_MAX: usize = 10;
const ERROR_HEADERS_MAX: usize = 5;
/// Lines after a diagnostic header searched for its `-->` location.
const DIAG_LOCATION_WINDOW: usize = 3;

/// Where a check's full output ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// No output log exists (a passing check, whose log is discarded).
    None,
    /// The full output is in a temporary log that has not been moved into the
    /// session record yet (see [`crate::gate::save_records`]).
    Pending(PathBuf),
    /// The full raw output is durably saved at this absolute path. The second
    /// field is why bounded retention failed after the save, if it did (shown
    /// as `record retention failed: …`).
    Saved(PathBuf, Option<String>),
    /// The record could not be saved. `temp` is the temporary log if it still
    /// exists (not retained by donegate); `tail_lines` is how many raw lines the
    /// fallback display shows.
    NotSaved {
        why: String,
        temp: Option<PathBuf>,
        tail_lines: usize,
    },
}

pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn session_dir_name(session: &str) -> String {
    let s = sanitize(session);
    if s.trim_matches(|c| c == '-' || c == '_').is_empty() {
        "nosession".to_string()
    } else {
        s
    }
}

/// Move the temporary log `tmp` into the session record. On success the record's
/// absolute path is returned and retention is applied (its failure, if any, in
/// [`Saved::retention`]); on failure to save, why.
pub fn save(
    state_dir: &Path,
    session: &str,
    attempt: u32,
    check: &str,
    tmp: &Path,
) -> Result<Saved, String> {
    let state_dir = std::path::absolute(state_dir)
        .map_err(|e| format!("cannot resolve {}: {e}", state_dir.display()))?;
    let root = state_dir.join("records");
    let dir = root.join(session_dir_name(session));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut dest = dir.join(format!("{ms:013}-a{attempt}-{}.log", sanitize(check)));
    let mut n = 1;
    while dest.exists() {
        n += 1;
        dest = dir.join(format!("{ms:013}-a{attempt}-{}-{n}.log", sanitize(check)));
    }
    if std::fs::rename(tmp, &dest).is_err() {
        // Cross-device (e.g. a temp fallback outside state_dir): copy, then drop
        // the temporary.
        std::fs::copy(tmp, &dest).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
        let _ = std::fs::remove_file(tmp);
    }
    let retention = prune(&root, &dir).err();
    Ok(Saved {
        path: dest,
        retention,
    })
}

/// A record that was saved, plus why retention failed (if it did). A retention
/// failure never undoes the save or changes the verdict, but it is SHOWN
/// (`record retention failed: …`) rather than swallowed: unbounded growth of
/// the record store must be visible, not silent.
pub struct Saved {
    pub path: PathBuf,
    pub retention: Option<String>,
}

/// Bounded retention, fail-closed in the sense that every failure is reported:
/// `Ok(())` means retention ran to completion. Only `NotFound` (nothing there
/// to prune, e.g. removed concurrently) is treated as success. An entry whose
/// type or mtime cannot be read is NOT deleted (it is reported instead): an
/// unreadable mtime must not make a directory look oldest.
fn prune(root: &Path, current: &Path) -> Result<(), String> {
    use std::io::ErrorKind::NotFound;
    let mut errors: Vec<String> = Vec::new();

    // Inside the current session: keep the newest KEEP_FILES_PER_SESSION.
    match std::fs::read_dir(current) {
        Ok(rd) => {
            let mut files: Vec<PathBuf> = Vec::new();
            for entry in rd {
                match entry {
                    Ok(e) => match e.file_type() {
                        Ok(t) if t.is_file() => files.push(e.path()),
                        Ok(_) => {}
                        Err(err) if err.kind() == NotFound => {}
                        Err(err) => errors.push(format!("{}: {err}", e.path().display())),
                    },
                    Err(err) => errors.push(format!("{}: {err}", current.display())),
                }
            }
            files.sort();
            if files.len() > KEEP_FILES_PER_SESSION {
                let excess = files.len() - KEEP_FILES_PER_SESSION;
                for p in &files[..excess] {
                    match std::fs::remove_file(p) {
                        Ok(()) => {}
                        Err(err) if err.kind() == NotFound => {}
                        Err(err) => errors.push(format!("cannot remove {}: {err}", p.display())),
                    }
                }
            }
        }
        Err(err) if err.kind() == NotFound => {}
        Err(err) => errors.push(format!("cannot list {}: {err}", current.display())),
    }

    // Across sessions: keep the newest KEEP_SESSIONS directories, current always.
    match std::fs::read_dir(root) {
        Ok(rd) => {
            let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
            for entry in rd {
                let e = match entry {
                    Ok(e) => e,
                    Err(err) => {
                        errors.push(format!("{}: {err}", root.display()));
                        continue;
                    }
                };
                let p = e.path();
                if p == current {
                    continue;
                }
                match e.file_type() {
                    Ok(t) if t.is_dir() => {}
                    Ok(_) => continue,
                    Err(err) if err.kind() == NotFound => continue,
                    Err(err) => {
                        errors.push(format!("{}: {err}", p.display()));
                        continue;
                    }
                }
                match e.metadata().and_then(|m| m.modified()) {
                    Ok(t) => dirs.push((t, p)),
                    Err(err) if err.kind() == NotFound => {}
                    Err(err) => errors.push(format!("{}: {err}", p.display())),
                }
            }
            dirs.sort_by_key(|d| std::cmp::Reverse(d.0));
            // `current` occupies one of the KEEP_SESSIONS slots.
            for (_, p) in dirs.iter().skip(KEEP_SESSIONS.saturating_sub(1)) {
                match std::fs::remove_dir_all(p) {
                    Ok(()) => {}
                    Err(err) if err.kind() == NotFound => {}
                    Err(err) => errors.push(format!("cannot remove {}: {err}", p.display())),
                }
            }
        }
        Err(err) if err.kind() == NotFound => {}
        Err(err) => errors.push(format!("cannot list {}: {err}", root.display())),
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn is_failed_test_line(l: &str) -> bool {
    l.starts_with("test ") && l.trim_end().ends_with(" ... FAILED")
}

fn is_panic_line(l: &str) -> bool {
    l.contains(" panicked at ")
}

/// A line that ends a panic message: the next section of cargo's output.
fn is_section_break(l: &str) -> bool {
    let t = l.trim();
    t.is_empty()
        || l.starts_with("---- ")
        || l.starts_with("--- ")
        || l.starts_with("note: ")
        || l.starts_with("stack backtrace:")
        || t == "failures:"
        || l.starts_with("test ")
        || is_panic_line(l)
}

fn is_error_header(l: &str) -> bool {
    l.starts_with("error[") || l.starts_with("error:")
}

fn is_warning_header(l: &str) -> bool {
    l.starts_with("warning[") || l.starts_with("warning:")
}

/// The `--> file:line:col` location line of the diagnostic whose header is
/// `lines[i]`: the first such line before a blank line or the next header,
/// within a few lines of the header.
fn diagnostic_location<'a>(lines: &[&'a str], i: usize) -> Option<&'a str> {
    lines[i + 1..]
        .iter()
        .take(DIAG_LOCATION_WINDOW)
        .take_while(|l| !l.trim().is_empty() && !is_error_header(l) && !is_warning_header(l))
        .find(|l| l.trim_start().starts_with("--> "))
        .copied()
}

/// `true` when `prefix` ends in a source location: tsc's `path(line,col)` or
/// gcc/clang's `path:line:col` / `path:line`, with a non-empty path that does
/// not start with whitespace (an indented code excerpt is not a header).
fn ends_in_location(prefix: &str) -> bool {
    if prefix.is_empty() || prefix.starts_with(char::is_whitespace) {
        return false;
    }
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    // tsc: `path(3,5)`
    if let Some(inner) = prefix.strip_suffix(')') {
        if let Some((path, lc)) = inner.rsplit_once('(') {
            if let Some((line, col)) = lc.split_once(',') {
                return !path.is_empty() && digits(line) && digits(col);
            }
        }
        return false;
    }
    // gcc / clang: `path:3:5` or `path:3`
    let Some((rest, last)) = prefix.rsplit_once(':') else {
        return false;
    };
    if !digits(last) {
        return false;
    }
    match rest.rsplit_once(':') {
        Some((path, line)) if digits(line) => !path.is_empty(),
        _ => !rest.is_empty(),
    }
}

/// A single-line located diagnostic — the header carries its own location:
/// tsc's `path(3,5): error TS2322: …` and gcc/clang's `path:3:5: error: …`
/// (also `fatal error:` and `warning`). `Some(true)` for an error, `Some(false)`
/// for a warning, `None` for anything else.
fn single_line_diagnostic(l: &str) -> Option<bool> {
    for (sep, err) in [
        (": error", true),
        (": fatal error", true),
        (": warning", false),
    ] {
        let mut from = 0;
        while let Some(p) = l[from..].find(sep) {
            let at = from + p;
            let after = &l[at + sep.len()..];
            if (after.starts_with(':') || after.starts_with(' ')) && ends_in_location(&l[..at]) {
                return Some(err);
            }
            from = at + sep.len();
        }
    }
    None
}

/// A compiler's error-count summary line that has no `error:` prefix: tsc's
/// `Found N error(s)…` and gcc/clang's `N error(s) generated.` (also
/// `N warnings and N errors generated.`).
fn is_diagnostic_summary(l: &str) -> bool {
    let t = l.trim();
    let starts_with_count = |s: &str| {
        let n = s.bytes().take_while(u8::is_ascii_digit).count();
        n > 0 && s[n..].starts_with(' ')
    };
    if let Some(rest) = t.strip_prefix("Found ") {
        return starts_with_count(rest) && rest.contains(" error");
    }
    starts_with_count(t) && t.ends_with(" generated.") && t.contains(" error")
}

/// The compiler/lint diagnostics of `lines`, skipping those already shown
/// elsewhere in `skip`. A diagnostic is identified by its header AND its
/// location, not by its header text alone: the same `error[E0425]: …` header
/// in several files is several diagnostics, and one whose header text is in
/// `skip` is skipped only if its location line is in `skip` too (an unlocated
/// header is skipped when its header is in `skip`). Located diagnostics are
/// either a header with a `-->` location (rustc / clippy; shown as the header
/// plus its first location line) or a single line carrying its own location
/// (tsc `path(3,5): error TS…: …`, gcc/clang `path:3:5: error: …`; shown as
/// that line, skipped when it is in `skip`). Up to [`ERROR_HEADERS_MAX`] of
/// them are shown, `error` ones before `warning` ones, with a `… and N more
/// diagnostics` line for the rest. Every unlocated `error` header (summaries
/// such as `error: could not compile …`) and every compiler count summary
/// (tsc `Found N errors…`, gcc/clang `N errors generated.`) not in `skip`
/// follows; only the digest's overall [`DIGEST_MAX_LINES`] cap bounds them.
/// Unlocated `warning` headers (`warning: … generated N warnings`) are not
/// shown.
fn diagnostics(lines: &[&str], skip: &[&str]) -> Vec<String> {
    // `(header, Some(location line))` for rustc-style, `(line, None)` for a
    // single-line diagnostic whose location is in the line itself.
    let mut located_err: Vec<(&str, Option<&str>)> = Vec::new();
    let mut located_warn: Vec<(&str, Option<&str>)> = Vec::new();
    let mut unlocated_err: Vec<&str> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if let Some(err) = single_line_diagnostic(l) {
            if !skip.contains(l) {
                if err {
                    located_err.push((l, None));
                } else {
                    located_warn.push((l, None));
                }
            }
            continue;
        }
        if is_diagnostic_summary(l) {
            if !skip.contains(l) {
                unlocated_err.push(l);
            }
            continue;
        }
        let err = is_error_header(l);
        if !(err || is_warning_header(l)) {
            continue;
        }
        let loc = diagnostic_location(lines, i);
        if skip.contains(l) && loc.is_none_or(|loc| skip.contains(&loc)) {
            continue;
        }
        match (loc, err) {
            (Some(loc), true) => located_err.push((l, Some(loc))),
            (Some(loc), false) => located_warn.push((l, Some(loc))),
            (None, true) => unlocated_err.push(l),
            (None, false) => {}
        }
    }
    let located: Vec<(&str, Option<&str>)> = located_err.into_iter().chain(located_warn).collect();
    let mut out: Vec<String> = Vec::new();
    for (h, loc) in located.iter().take(ERROR_HEADERS_MAX) {
        out.push(h.trim_end().to_string());
        if let Some(loc) = loc {
            out.push(loc.trim_end().to_string());
        }
    }
    if located.len() > ERROR_HEADERS_MAX {
        out.push(format!(
            "… and {} more diagnostics",
            located.len() - ERROR_HEADERS_MAX
        ));
    }
    for h in &unlocated_err {
        out.push(h.trim_end().to_string());
    }
    out
}

/// Non-cargo lines that report a failure: any line containing `FAILED`
/// (`FAILED …`, pytest's `FAILED tests/…::name`, `… build FAILED`); go test's
/// `--- FAIL: TestName` (also indented, for subtests); python unittest's
/// `FAIL: test_name (module.Class)`; and jest's / go's `FAIL <path>` (a `FAIL`
/// followed by whitespace and a name). A bare `FAIL` line names nothing and is
/// not matched.
fn is_plain_failed_line(l: &str) -> bool {
    let t = l.trim_start();
    l.contains("FAILED")
        || t.starts_with("--- FAIL:")
        || t.starts_with("FAIL:")
        || (t.len() > 4
            && t.starts_with("FAIL")
            && t[4..].starts_with(char::is_whitespace)
            && !t[4..].trim().is_empty())
}

/// The decision-relevant lines of a failing check's output. Never empty.
///
/// * cargo-style output (a `test … FAILED`, `panicked at` or `test result:
///   FAILED` line is present): each `panicked at` line (it names the test
///   thread and the location) with its message lines up to the next section
///   marker; the last `failures:` name list (or, if there is none, the `test …
///   FAILED` lines); every `test result: FAILED` line; the diagnostics (see
///   below); and the final line if it says FAILED.
/// * anything else: the diagnostics, then the failing-test lines of the common
///   runners — lines containing `FAILED`, go's `--- FAIL: …`, python
///   unittest's `FAIL: …`, jest's/go's `FAIL <path>` (up to
///   [`FAILURE_NAMES_MAX`], then `… and N more FAILED lines`), then the last
///   [`PLAIN_TAIL_LINES`] non-empty lines (lines already in that tail are not
///   repeated above it; a diagnostic is "in the tail" only if both its header
///   and its location are).
///
/// Diagnostics: each `error[…]`/`error:`/`warning[…]`/`warning:` header that
/// has a `--> file:line:col` location is shown with that location (up to
/// [`ERROR_HEADERS_MAX`], errors first, then `… and N more diagnostics`); the
/// location is what the next action needs. A single-line diagnostic that
/// carries its own location — tsc `path(3,5): error TS2322: …`, gcc/clang
/// `path:3:5: error: …` (and their warnings) — counts against the same cap.
/// `error` headers with no location (`error: could not compile …`, `error: test
/// failed …`) and compiler count summaries (tsc `Found N errors…`, gcc/clang
/// `N errors generated.`) are all shown too.
///
/// Raw `--- stdout ---`/`--- stderr ---` sections, `---- name stdout ----`
/// banners, backtrace notes and passing-test progress lines are never selected.
/// The result is capped at [`DIGEST_MAX_LINES`]; what the cap drops is counted
/// in a final `… (N more summary lines; see the full output)` line, and the
/// full output is in the record file.
pub fn digest(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let cargo = lines.iter().any(|l| {
        is_failed_test_line(l) || is_panic_line(l) || l.starts_with("test result: FAILED")
    });
    let mut out: Vec<String> = Vec::new();
    // Append a line once (a summary repeated by several test binaries is shown once).
    fn push(out: &mut Vec<String>, l: &str) {
        let l = l.trim_end().to_string();
        if !out.contains(&l) {
            out.push(l);
        }
    }
    if cargo {
        let mut i = 0;
        while i < lines.len() {
            if is_panic_line(lines[i]) {
                push(&mut out, lines[i]);
                let mut j = i + 1;
                while j < lines.len() && j - i <= PANIC_MSG_MAX && !is_section_break(lines[j]) {
                    push(&mut out, lines[j]);
                    j += 1;
                }
                i = j;
            } else {
                i += 1;
            }
        }
        // The last `failures:` header directly followed by indented names.
        let mut names: Vec<&str> = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            if l.trim() == "failures:" {
                let block: Vec<&str> = lines[i + 1..]
                    .iter()
                    .take_while(|n| n.starts_with("    ") && !n.trim().is_empty())
                    .copied()
                    .collect();
                if !block.is_empty() {
                    names = block;
                }
            }
        }
        if !names.is_empty() {
            push(&mut out, "failures:");
            for n in names.iter().take(FAILURE_NAMES_MAX) {
                push(&mut out, n);
            }
            if names.len() > FAILURE_NAMES_MAX {
                push(
                    &mut out,
                    &format!("    … and {} more", names.len() - FAILURE_NAMES_MAX),
                );
            }
        } else {
            for l in lines.iter().filter(|l| is_failed_test_line(l)) {
                push(&mut out, l);
            }
        }
        for l in lines
            .iter()
            .filter(|l| l.starts_with("test result: FAILED"))
        {
            push(&mut out, l);
        }
        for l in diagnostics(&lines, &[]) {
            push(&mut out, &l);
        }
        if let Some(last) = lines.iter().rev().find(|l| !l.trim().is_empty()) {
            if last.contains("FAILED") {
                push(&mut out, last);
            }
        }
    } else {
        let tail: Vec<&str> = {
            let ne: Vec<&str> = lines
                .iter()
                .copied()
                .filter(|l| !l.trim().is_empty())
                .collect();
            ne[ne.len().saturating_sub(PLAIN_TAIL_LINES)..].to_vec()
        };
        let diags = diagnostics(&lines, &tail);
        for d in &diags {
            push(&mut out, d);
        }
        let failed: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| is_plain_failed_line(l) && !tail.contains(l))
            .collect();
        for l in failed.iter().take(FAILURE_NAMES_MAX) {
            push(&mut out, l);
        }
        if failed.len() > FAILURE_NAMES_MAX {
            push(
                &mut out,
                &format!(
                    "… and {} more FAILED lines",
                    failed.len() - FAILURE_NAMES_MAX
                ),
            );
        }
        if !diags.is_empty() || !failed.is_empty() {
            push(&mut out, "…");
        }
        for l in tail {
            push(&mut out, l);
        }
    }
    if out.is_empty() {
        out.push("(the check produced no output)".to_string());
    }
    if out.len() > DIGEST_MAX_LINES {
        let more = out.len() - (DIGEST_MAX_LINES - 1);
        out.truncate(DIGEST_MAX_LINES - 1);
        out.push(format!(
            "… ({more} more summary lines; see the full output)"
        ));
    }
    out
}

/// The last `n` lines of `text`.
pub fn raw_tail(text: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_of_empty_output_is_not_empty() {
        assert_eq!(
            digest(""),
            vec!["(the check produced no output)".to_string()]
        );
    }

    #[test]
    fn digest_is_capped() {
        let mut s = String::new();
        for i in 0..100 {
            s.push_str(&format!(
                "thread 't{i}' panicked at a.rs:{i}:1:\nmsg {i}\n\n"
            ));
        }
        let d = digest(&s);
        assert_eq!(d.len(), DIGEST_MAX_LINES);
        assert!(d.last().is_some_and(|l| l.contains("more summary lines")));
    }

    #[test]
    fn plain_output_keeps_error_headers_and_last_lines() {
        let s = "error[E0425]: cannot find value `x`\n  --> a.rs:1:1\nmore\nmore\nmore\nmore\nmore\nerror: could not compile `a`\n";
        let d = digest(s);
        assert_eq!(d[0], "error[E0425]: cannot find value `x`");
        assert_eq!(
            d.last().map(String::as_str),
            Some("error: could not compile `a`")
        );
    }

    #[test]
    fn retention_keeps_current_session_and_bounds_others() {
        let base = std::env::temp_dir().join(format!("donegate-rec-ret-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("records");
        for i in 0..(KEEP_SESSIONS + 5) {
            std::fs::create_dir_all(root.join(format!("old{i}"))).unwrap();
        }
        let tmp = base.join("t.log");
        std::fs::write(&tmp, "x").unwrap();
        let s = save(&base, "cur", 1, "c", &tmp).unwrap();
        assert!(s.path.exists());
        assert_eq!(s.retention, None, "retention must report success here");
        let n = std::fs::read_dir(&root).unwrap().count();
        assert_eq!(n, KEEP_SESSIONS);
        assert!(root.join("cur").is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }
}
