//! Atomic single-write append for JSONL sinks.
//!
//! Every hook/agent appends one record per event to a shared `.jsonl` log, and
//! parallel subagents run these appends concurrently (each in its own process,
//! each opening the file with its own `O_APPEND` handle). `writeln!(f, "{line}")`
//! on an unbuffered [`std::fs::File`] emits the body and the trailing `\n` as
//! **two separate `write()` syscalls**. With only `O_APPEND` for ordering, two
//! writers can interleave as `bodyA · bodyB · \nA · \nB`, concatenating two
//! complete JSON objects onto one physical line and corrupting the log — the
//! write-write race reported in issue #15.
//!
//! Writing the body **and** its newline from a single buffer collapses the
//! record to one `write()` syscall, which `O_APPEND` guarantees is atomic for
//! sizes up to `PIPE_BUF` (>= 4096 bytes on Linux); these records are far
//! smaller. This is the single-write half of the fix — sinks that additionally
//! read-modify-write (dedup by id) hold an advisory lock across the whole
//! critical section (see [`crate::lessons`]); pure-append sinks only need this.

use std::io::Write;
use std::path::Path;

/// Append `line` plus a single trailing newline to `path` as **one** atomic
/// `write()`, creating the parent dir and the file on demand.
///
/// **Returns the failure instead of swallowing it.** This function used to
/// discard every error (dir create, open, write) on the rationale that "a hook
/// never breaks on a metrics write". That rationale conflated two things: not
/// *breaking* on a lost record, and not *reporting* one. Every sink here is a
/// review surface that a human reads as a complete history, so a silently
/// dropped record makes "the event did not happen" and "the write failed"
/// byte-for-byte indistinguishable — the fail-open shape CLAUDE.md §1 names
/// ("沈黙は許容される degrade ではない") and §3 forbids resolving to the clean
/// side. Callers still must not *fail* on a logging error; they must **say** so.
/// [`crate::undetermined`] already had to re-implement this function privately
/// for exactly that reason, which is the evidence that `()` was the wrong shape.
///
/// `line` must be a single serialized record with **no trailing newline** (this
/// adds exactly one); embedded newlines would split the record across lines, so
/// callers pass one JSON object per call.
///
/// Do **not** reintroduce `writeln!`/two-step writes here: the body and `\n`
/// must share one buffer or concurrent `O_APPEND` writers interleave (issue #15).
pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // Body + '\n' in ONE buffer → ONE write() syscall → atomic under O_APPEND.
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    f.write_all(buf.as_bytes())
}

/// Append `line`, reporting a failure on stderr rather than dropping it.
///
/// The shared shape for the JSONL sinks: the record is best-effort (a logging
/// failure never changes a verdict or an exit code) but it is **never silent**.
/// `sink` names the log in the diagnostic so a reader can tell which history
/// now has a hole in it.
pub fn append_line_reporting(path: &Path, line: &str, sink: &str) {
    if let Err(e) = append_line(path, line) {
        eprintln!(
            "warning: {sink}: could not append to {} ({e}) — this record is LOST; \
             the log is no longer a complete history",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_line_writes_one_record_terminated_by_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sink.jsonl");
        append_line(&path, r#"{"a":1}"#).expect("append");
        append_line(&path, r#"{"b":2}"#).expect("append");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "{\"a\":1}\n{\"b\":2}\n");
        // Every physical line parses as exactly one JSON object.
        for l in text.lines() {
            serde_json::from_str::<serde_json::Value>(l).expect("one JSON object per line");
        }
    }

    #[test]
    fn append_line_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/deeper/sink.jsonl");
        append_line(&path, r#"{"ok":true}"#).expect("append");
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"ok\":true}\n");
    }

    /// The regression guard for issue #15: many writers appending concurrently,
    /// each with its own `O_APPEND` handle (mirroring separate subagent
    /// processes), must never concatenate two records onto one line. With the
    /// old `writeln!` (body then `\n` as two syscalls) this interleaves and some
    /// lines fail to parse; with the single-buffer write every line stays a lone
    /// JSON object and the count is exact.
    #[test]
    fn concurrent_appends_never_interleave_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("race.jsonl");

        const THREADS: usize = 8;
        const PER_THREAD: usize = 500;

        std::thread::scope(|scope| {
            for t in 0..THREADS {
                let path = path.clone();
                scope.spawn(move || {
                    for i in 0..PER_THREAD {
                        // Vary payload length so a torn write would land at
                        // different offsets, not just a fixed boundary.
                        let pad = "x".repeat(i % 64);
                        let line = format!(r#"{{"t":{t},"i":{i},"pad":"{pad}"}}"#);
                        append_line(&path, &line).expect("append");
                    }
                });
            }
        });

        let text = std::fs::read_to_string(&path).unwrap();
        let mut count = 0usize;
        for (n, l) in text.lines().enumerate() {
            if l.is_empty() {
                continue;
            }
            serde_json::from_str::<serde_json::Value>(l)
                .unwrap_or_else(|e| panic!("line {n} is not a single JSON object: {e} — {l:.120}"));
            count += 1;
        }
        assert_eq!(
            count,
            THREADS * PER_THREAD,
            "every appended record must survive as exactly one parseable line"
        );
    }
}
