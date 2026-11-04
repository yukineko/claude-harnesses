// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! INTENTIONALLY RED reproduction (filed 2026-11-03): `harness_core::hook::read_stdin`
//! (via its pure core `read_capped`) erases a stdin READ ERROR into an
//! ordinary-looking payload.
//!
//! `read_capped` does `let _ = r.take(MAX_STDIN_BYTES).read_to_end(&mut buf);`
//! and returns `String`. So:
//!
//! * a reader that fails before yielding any byte returns `""` — the exact
//!   value a hook gets when nothing was piped at all. Verdict-bearing callers
//!   read `""` as "no payload": e.g. `fetchguard scan` does
//!   `if raw.trim().is_empty() { return; }` (silence = no warning), and
//!   `stuckguard watch` returns early on an unparseable payload.
//! * a reader that fails part way returns the prefix it got, indistinguishable
//!   from a complete payload of those bytes.
//!
//! CLAUDE.md §3: an IO failure is a cannot-determine and must not be
//! representable as a clean/empty input. The property asserted here: a read
//! error must NOT come back as the same `Ok(String)` an error-free read of the
//! same bytes would produce. A fix may panic (caught by the fail-closed
//! barriers) or change the return type (then this file must be re-targeted at
//! the new API — a compile failure is NOT the fix being proven).
//!
//! Counter-reading: the doc comment on `read_stdin` says "Errors are swallowed
//! (returns what was read, possibly empty) so a hook never aborts on a read
//! hiccup", and the in-crate test
//! `read_capped_returns_partial_content_when_the_reader_errors_midway` pins
//! that. If that is the intended contract, this file is the wrong spec and
//! the decision belongs to a human, not to whoever fixes it.
//!
//! Written by a disinterested author (CLAUDE.md §2(a)); `src/` untouched.

use harness_core::hook::read_capped;
use std::io::Read;

/// Serves `head`, then fails every further read.
struct ErrAfter {
    head: Vec<u8>,
}

impl Read for ErrAfter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.head.is_empty() {
            return Err(std::io::Error::other("synthetic stdin read failure"));
        }
        let n = self.head.len().min(buf.len());
        buf[..n].copy_from_slice(&self.head[..n]);
        self.head.drain(..n);
        Ok(n)
    }
}

/// A panic inside `read_capped` is an acceptable (restrictive) outcome, so it
/// is caught rather than counted as a failure. The panic hook is deliberately
/// left alone: it is process-global and tests run in parallel.
fn quiet<T>(f: impl FnOnce() -> T + std::panic::UnwindSafe) -> std::thread::Result<T> {
    std::panic::catch_unwind(f)
}

#[test]
fn read_error_before_any_byte_is_not_the_empty_payload() {
    let clean_empty = read_capped(&b""[..]);
    let errored = quiet(|| read_capped(ErrAfter { head: Vec::new() }));
    if let Ok(got) = errored {
        assert_ne!(
            got, clean_empty,
            "a stdin read ERROR came back as Ok({got:?}) — identical to 'nothing \
             was piped'; the failure is invisible to the caller"
        );
    }
}

#[test]
fn read_error_midway_is_not_a_complete_payload() {
    let clean = read_capped(&b"{\"a\":1}"[..]);
    let errored = quiet(|| {
        read_capped(ErrAfter {
            head: b"{\"a\":1}".to_vec(),
        })
    });
    if let Ok(got) = errored {
        assert_ne!(
            got, clean,
            "a stdin read that FAILED after {got:?} came back identical to a \
             complete, error-free read of the same bytes"
        );
    }
}
