//! Verifier-stage invariants — enforced by the binary, not by SKILL.md prose.
//!
//! Two failure modes of the LLM verifier stage are made mechanical here so they
//! cannot drift out of the skill:
//!
//! 1. **Shared blind spot** (`resolve_verifier_model`): the verifier model must
//!    never equal the worker model. When fugu-router is absent both sides used to
//!    fall back to the same tier (sonnet), so generation and verification shared
//!    the same blind spots. The resolver guarantees a distinct, independent tier.
//!
//! 2. **Behavioral criteria never skip the verifier** (`classify_criteria`):
//!    only *purely mechanical* done_criteria (a runnable check with no judgement
//!    words) may bypass the LLM verifier. For behavioral criteria a passing test
//!    is only *evidence handed to* the verifier, never a substitute for it. When
//!    classification is ambiguous we fail toward RUNNING the verifier (safe side).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

/// How many trailing lines of raw output to retain in [`FailureDigest::output_tail`].
const OUTPUT_TAIL_LINES: usize = 20;

/// A deterministic, structured distillation of a failing command's raw output.
///
/// The verifier→worker reflux used to carry only a boolean plus an undistilled
/// output blob. `FailureDigest` extracts the *why-it-failed* signal — failing
/// test names, assertion evidence, and a bounded output tail — so a worker (or
/// the /condukt skill's retry prompt) can self-correct in the same run. The
/// FORMATTING here is deterministic Rust; only the fix DECISION is the LLM's job.
///
/// The `condukt verify digest` subcommand exposes [`distill_failure`] so the
/// skill can distill ANY worker/verifier raw output into the retry reflux prompt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FailureDigest {
    /// Names of failing tests (deduplicated, first-seen order).
    pub failing_tests: Vec<String>,
    /// Assertion / panic evidence lines (trimmed short strings).
    pub assertion_diffs: Vec<String>,
    /// The last [`OUTPUT_TAIL_LINES`] lines of the raw output, joined by `\n`.
    pub output_tail: String,
}

/// Distill a failing command's raw output into a [`FailureDigest`].
///
/// Pure and deterministic: no LLM, no network, no filesystem, no clock. Handles
/// empty / garbage / non-cargo input gracefully (empty vecs, whatever tail
/// exists) and never panics.
///
/// - `failing_tests`: names from cargo-test `test <name> ... FAILED` lines and
///   from the indented `failures:` summary block. Deduplicated, first-seen order.
/// - `assertion_diffs`: `assertion \`...\` failed` lines, following `left:` /
///   `right:` lines, and `panicked at ...` / `thread '...' panicked` lines.
/// - `output_tail`: the last [`OUTPUT_TAIL_LINES`] lines (or all, if shorter).
pub fn distill_failure(raw_output: &str) -> FailureDigest {
    let mut failing_tests: Vec<String> = Vec::new();
    let mut assertion_diffs: Vec<String> = Vec::new();

    let push_unique = |v: &mut Vec<String>, s: String| {
        if !s.is_empty() && !v.contains(&s) {
            v.push(s);
        }
    };

    // First pass: `test <name> ... FAILED` result lines.
    for line in raw_output.lines() {
        let t = line.trim();
        if let Some(name) = parse_test_result_failed(t) {
            push_unique(&mut failing_tests, name);
        }
    }

    // Second pass: the `failures:` summary block lists each failing test name on
    // its own indented line, terminated by a blank line or a `test result:` line.
    let mut in_failures_block = false;
    for line in raw_output.lines() {
        let t = line.trim();
        if t == "failures:" {
            in_failures_block = true;
            continue;
        }
        if in_failures_block {
            // The block ends at a blank line, a `test result:` summary, or the
            // start of the error-detail sub-listing that cargo repeats.
            if t.is_empty() || t.starts_with("test result:") {
                in_failures_block = false;
                continue;
            }
            // Names in the summary are bare identifiers (e.g. `foo::bar`); ignore
            // any lines that look like prose/evidence rather than a test path.
            if is_test_name_line(t) {
                push_unique(&mut failing_tests, t.to_string());
            }
        }
    }

    // Assertion / panic evidence: the "why" beyond the boolean.
    for line in raw_output.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let is_assertion = t.contains("assertion") && t.contains("failed");
        let is_left = t.starts_with("left:");
        let is_right = t.starts_with("right:");
        let is_panic =
            t.starts_with("panicked at") || (t.starts_with("thread '") && t.contains("panicked"));
        if is_assertion || is_left || is_right || is_panic {
            push_unique(&mut assertion_diffs, t.to_string());
        }
    }

    let output_tail = tail_lines(raw_output, OUTPUT_TAIL_LINES);

    FailureDigest {
        failing_tests,
        assertion_diffs,
        output_tail,
    }
}

/// Parse a cargo-test result line `test <name> ... FAILED`, returning `<name>`.
/// Tolerates a leading log prefix by matching on the `test ` token boundary and
/// the trailing ` ... FAILED`. Returns `None` for non-matching lines.
fn parse_test_result_failed(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("test ")?;
    // Must end in the FAILED result marker (cargo emits `... FAILED`).
    if !rest.ends_with("FAILED") {
        return None;
    }
    let name_part = rest.split(" ... ").next()?.trim();
    if name_part.is_empty() || name_part == "result:" {
        return None;
    }
    Some(name_part.to_string())
}

/// Heuristic: does a `failures:`-block line look like a bare test-name path
/// (e.g. `foo::bar`) rather than prose or evidence?
fn is_test_name_line(t: &str) -> bool {
    !t.is_empty()
        && !t.contains(' ')
        && t.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '-')
}

/// Return the last `n` lines of `s` joined by `\n`. If `s` has `n` or fewer
/// lines, all of them are returned. Empty input yields an empty string.
fn tail_lines(s: &str, n: usize) -> String {
    if s.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// A deterministic, structured distillation of a target's *runtime* signals.
///
/// The phase-2 companion [`FailureDigest`] distills a failing command's *test*
/// output (failing test names, assertion evidence). `RuntimeDigest` is the
/// phase-3 counterpart: it distills the signals from actually *running* the
/// target — its exit code, any panic/exception evidence, and bounded tails of
/// both output streams — so the verifier→worker reflux carries the runtime
/// *why-it-broke*, not just a boolean. The FORMATTING here is deterministic
/// Rust; only the fix DECISION is the LLM's job.
///
/// Exposed by the `condukt verify runtime` subcommand (symmetric to `verify
/// digest`) and embedded into the verifier→worker reflux verdict on a runtime
/// failure by [`runtime_reflux_verdict`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeDigest {
    /// The process exit code, or `None` when unknown (e.g. signal termination).
    pub exit_code: Option<i32>,
    /// Panic / exception evidence lines (deduplicated, first-seen order, stderr
    /// preferred). Matches `panicked at`, `thread '...' panicked`, `Exception`,
    /// `Traceback`, and `Error:` markers.
    pub panics: Vec<String>,
    /// The last [`OUTPUT_TAIL_LINES`] lines of stderr, joined by `\n`.
    pub stderr_tail: String,
    /// The last [`OUTPUT_TAIL_LINES`] lines of stdout, joined by `\n`.
    pub stdout_tail: String,
}

/// Distill a target's runtime output into a [`RuntimeDigest`].
///
/// Pure and deterministic: no LLM, no network, no filesystem, no clock. Handles
/// empty / garbage / non-UTF8-ish input gracefully (empty vecs, whatever tail
/// exists) and never panics.
///
/// - `exit_code`: threaded through verbatim (`None` when the caller could not
///   determine one, e.g. signal termination).
/// - `panics`: panic/exception evidence lines gathered from BOTH streams, with
///   stderr scanned first so its lines win first-seen order. Deduplicated via
///   the same policy as [`distill_failure`]. Markers: `panicked at`,
///   `thread '...' panicked`, `Exception`, `Traceback`, `Error:`.
/// - `stderr_tail` / `stdout_tail`: the last [`OUTPUT_TAIL_LINES`] lines of each
///   stream (or all, if shorter).
pub fn distill_runtime(stdout: &str, stderr: &str, exit_code: Option<i32>) -> RuntimeDigest {
    let mut panics: Vec<String> = Vec::new();

    let push_unique = |v: &mut Vec<String>, s: String| {
        if !s.is_empty() && !v.contains(&s) {
            v.push(s);
        }
    };

    // Scan stderr first (preferred first-seen), then stdout, for panic/exception
    // evidence. A line qualifies if it carries any recognised runtime marker.
    for stream in [stderr, stdout] {
        for line in stream.lines() {
            let t = line.trim();
            if is_panic_evidence(t) {
                push_unique(&mut panics, t.to_string());
            }
        }
    }

    RuntimeDigest {
        exit_code,
        panics,
        stderr_tail: tail_lines(stderr, OUTPUT_TAIL_LINES),
        stdout_tail: tail_lines(stdout, OUTPUT_TAIL_LINES),
    }
}

/// Build the verifier→worker reflux verdict for a target's *runtime* signals.
///
/// The phase-2 companion [`mechanical_skip_verdict`] embeds a [`FailureDigest`]
/// under `"failure_digest"` when a mechanical *test* check fails; this is the
/// phase-3 counterpart for a *runtime* failure. It is pure and deterministic:
///
/// - it decides pass/fail from the mechanical facts alone — a runtime failure is
///   a non-zero exit code (`Some(c)` with `c != 0`) OR any panic/exception
///   evidence in [`RuntimeDigest::panics`];
/// - on failure it embeds the structured [`RuntimeDigest`] under `"runtime_digest"`
///   so the reflux carries the runtime *why* (exit code, panic lines, and the
///   stderr/stdout tails), not merely the boolean; the passing shape omits it.
///
/// The FORMATTING here is deterministic Rust; the verdict states only observable
/// facts and carries NO fix decision — how to fix stays with the LLM worker.
pub fn runtime_reflux_verdict(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> serde_json::Value {
    let digest = distill_runtime(stdout, stderr, exit_code);
    // Mechanical failure predicate: a non-zero exit OR any panic/exception line.
    let nonzero_exit = digest.exit_code.is_some_and(|c| c != 0);
    let runtime_failed = nonzero_exit || !digest.panics.is_empty();
    let mut out = serde_json::json!({
        "kind": "runtime",
        "passed": !runtime_failed,
    });
    // Attach the deterministic structured digest ONLY on failure, mirroring the
    // `failure_digest` embedding: the passing-case shape stays a bare boolean.
    if runtime_failed {
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "runtime_digest".to_string(),
                serde_json::to_value(&digest).unwrap_or(serde_json::Value::Null),
            );
        }
    }
    out
}

/// True iff a trimmed line looks like panic / exception evidence from a running
/// process. Language-agnostic: covers Rust panics plus common Python/JVM/other
/// exception markers. Empty lines never qualify.
fn is_panic_evidence(t: &str) -> bool {
    if t.is_empty() {
        return false;
    }
    t.starts_with("panicked at")
        || (t.starts_with("thread '") && t.contains("panicked"))
        || t.contains("Exception")
        || t.contains("Traceback")
        || t.contains("Error:")
}

/// Launch `cmd` as a real subprocess inside the blastguard-validated envelope,
/// capture its runtime signals, and reflux them through the existing
/// deterministic verdict path. This is the IO-bearing companion to the pure
/// [`runtime_reflux_verdict`]: formatting stays with that one function, so this
/// launcher never re-implements digest shaping.
///
/// The command is run through `sh -c` (no Docker/VM/sandbox — the existing
/// `sh -c` + `wait-timeout` envelope is the whole isolation story) with a
/// bounded timeout.
///
/// **never-break-a-turn**: this function never `panic!`/`unwrap`/`expect`s on an
/// external-input or absent-tool path. Every branch returns a verdict (JSON):
///
/// - **blastguard `Deny`**: the command is refused *fail-closed* and is NEVER
///   spawned — a fail-soft runtime-failure verdict carries the refusal reason in
///   its stderr tail. No shell runs, so a destructive payload cannot execute.
/// - **spawn failure** (missing target / not executable): a fail-soft failure
///   verdict (`exit_code` null, stderr carries the error, `note = "spawn-error"`).
/// - **timeout**: the child is killed; a fail-soft failure verdict (`exit_code`
///   null, `note = "timeout"`).
/// - **normal exit**: stdout/stderr/exit code are refluxed through
///   [`runtime_reflux_verdict`], whose pass/fail predicate decides the verdict.
///
/// The verdict carries only observable facts (pass/fail, the runtime digest, and
/// a mechanical `note` for the fail-soft branches) — never a fix decision. How
/// to fix stays with the LLM worker.
pub fn launch_and_reflux(cmd: &str, timeout_secs: u64) -> serde_json::Value {
    // (a) blastguard gate — validate BEFORE spawning, reusing the same pure
    // detector the PreToolUse hook uses (no reimplementation). A flagged command
    // is refused fail-closed and never reaches the shell.
    let input = serde_json::json!({ "command": cmd });
    if let blastguard::model::Decision::Deny(reason) =
        blastguard::detect::detect("Bash", Some(&input))
    {
        let stderr = format!(
            "[blastguard] launch command `{cmd}` refused before sh -c (fail-closed) — {reason}"
        );
        return fail_soft_launch_verdict("", &stderr, None, "blastguard-denied");
    }

    // (b) spawn via `sh -c`, piping both streams so we can capture them.
    let timeout = timeout_secs.max(1);
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            // Fail-soft: the target could not even be started. No panic.
            let stderr = format!("failed to spawn `{cmd}`: {e}");
            return fail_soft_launch_verdict("", &stderr, None, "spawn-error");
        }
    };

    // (c) wait with a timeout; a timed-out child is killed and reaped.
    match child.wait_timeout(Duration::from_secs(timeout)) {
        Ok(Some(status)) => {
            // (d) normal exit — read both streams and reflux through the pure fn.
            let (stdout, stderr) = read_child_streams(&mut child);
            runtime_reflux_verdict(&stdout, &stderr, status.code())
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            let stderr = format!("launch of `{cmd}` timed out after {timeout}s and was killed");
            fail_soft_launch_verdict("", &stderr, None, "timeout")
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            let stderr = format!("failed to wait on `{cmd}`: {e}");
            fail_soft_launch_verdict("", &stderr, None, "wait-error")
        }
    }
}

/// Build a fail-soft launch verdict that is ALWAYS a failure, regardless of the
/// [`runtime_reflux_verdict`] predicate (which keys off exit code / panic
/// markers that the fail-soft branches — deny / spawn-error / timeout — may not
/// carry). It mirrors the runtime verdict shape (`kind` + `passed` + an embedded
/// `runtime_digest`) and adds a mechanical `note` naming the fail-soft cause.
fn fail_soft_launch_verdict(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    note: &str,
) -> serde_json::Value {
    let digest = distill_runtime(stdout, stderr, exit_code);
    let mut out = serde_json::json!({
        "kind": "runtime",
        "passed": false,
        "note": note,
    });
    if let Some(obj) = out.as_object_mut() {
        obj.insert(
            "runtime_digest".to_string(),
            serde_json::to_value(&digest).unwrap_or(serde_json::Value::Null),
        );
    }
    out
}

/// Read a finished child's piped stdout/stderr into lossy-UTF8 strings. The
/// child has already exited when this is called (so the bounded pipe buffers
/// hold everything), and read errors degrade to whatever was captured — never a
/// panic.
fn read_child_streams(child: &mut std::process::Child) -> (String, String) {
    let mut stdout_buf = Vec::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_end(&mut stdout_buf);
    }
    let mut stderr_buf = Vec::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_end(&mut stderr_buf);
    }
    (
        String::from_utf8_lossy(&stdout_buf).into_owned(),
        String::from_utf8_lossy(&stderr_buf).into_owned(),
    )
}

/// Parse a health URL into (host, port, path).
/// Expected format: `http://host:port/path` or `http://host/path` (port defaults to 80).
/// Returns None on parse failure (e.g., missing host, bad URL format, or unparseable host:port).
#[allow(dead_code)]
fn parse_health_url(url: &str) -> Option<(String, u16, String)> {
    let url = url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let (host_port, path) = if let Some(idx) = url.find('/') {
        (url[..idx].to_string(), url[idx..].to_string())
    } else {
        (url.to_string(), "/".to_string())
    };

    let (host, port) = if let Some(colon_idx) = host_port.rfind(':') {
        let h = host_port[..colon_idx].trim();
        let p = host_port[colon_idx + 1..].trim();
        let port_num = p.parse::<u16>().ok()?;
        (h.to_string(), port_num)
    } else {
        (host_port.trim().to_string(), 80)
    };

    if host.is_empty() {
        return None;
    }

    // Validate that host:port resolves to a socket address via the OS resolver,
    // so hostnames (e.g. "localhost") work — not just IP literals. Resolution
    // failure (bad host, unresolvable name) => None => "health-bad-url".
    (host.as_str(), port).to_socket_addrs().ok()?.next()?;

    Some((host, port, path))
}

/// Probe a health URL with raw HTTP/1.1 GET, retrying until the status is 200 or timeout.
/// Returns true iff a 200 status was received.
#[allow(dead_code)]
fn probe_health_url(host: &str, port: u16, path: &str, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(100);

    loop {
        if start.elapsed() >= timeout {
            return false;
        }

        // Resolve host:port via the OS resolver each attempt (hostnames + IPs),
        // then try to connect and send an HTTP GET.
        if let Some(addr) = (host, port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
        {
            match TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
                Ok(mut stream) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                    let request = format!(
                        "GET {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n",
                        path, host, port
                    );
                    if stream.write_all(request.as_bytes()).is_ok() {
                        // Read response looking for status line with 200.
                        let mut buf = [0u8; 512];
                        if let Ok(n) = stream.read(&mut buf) {
                            if n > 0 {
                                let response = String::from_utf8_lossy(&buf[..n]);
                                if response.contains(" 200 ") {
                                    return true;
                                }
                            }
                        }
                    }
                    // If we got a non-200 response or read error, treat as unhealthy but don't retry
                    // the same cycle — break and recheck after interval.
                }
                Err(_) => {
                    // Connection refused / timeout — server may still be starting, retry.
                }
            }
        }

        // Brief sleep before retry.
        std::thread::sleep(poll_interval);
    }
}

/// Launch `cmd` as a real subprocess, probe its health endpoint, and return a
/// structured verdict. Unlike [`launch_and_reflux`], this does NOT wait for the
/// process to exit; instead, it:
///
/// 1. Validates `cmd` with blastguard (fail-closed, no spawn if Deny).
/// 2. Spawns the process in background with piped stdout/stderr.
/// 3. Polls `health_url` (raw HTTP/1.1 GET) until either HTTP 200 is received
///    or `startup_timeout_secs` expires.
/// 4. On health-check success or final failure, kills the process and reads
///    bounded stdout/stderr.
/// 5. Returns a verdict JSON (shape mirrors [`runtime_reflux_verdict`] + fail-soft notes).
///
/// **Health URL format**: `http://host:port/path` (port defaults to 80 if omitted).
///
/// **Verdict shape**:
/// - `passed: true` when health check succeeds (HTTP 200 observed).
/// - `passed: false` with a `note` field for fail-soft cases:
///   - `"health-timeout"`: health check timed out.
///   - `"health-bad-url"`: URL parse failed.
///   - `"health-non-200"`: server responded with non-200 status.
///   - `"blastguard-denied"`: command refused before spawn.
///   - `"spawn-error"`: failed to spawn the process.
/// - `runtime_digest` embedded on failure (with bounded stdout/stderr tails).
#[allow(dead_code)]
pub fn launch_server_and_probe(
    cmd: &str,
    health_url: &str,
    startup_timeout_secs: u64,
) -> serde_json::Value {
    // (a) Validate URL early — parse failure means never spawn.
    let (host, port, path) = match parse_health_url(health_url) {
        Some((h, p, path)) => (h, p, path),
        None => {
            return fail_soft_launch_verdict(
                "",
                &format!("failed to parse health URL: {}", health_url),
                None,
                "health-bad-url",
            );
        }
    };

    // (b) Blastguard gate — validate BEFORE spawning.
    let input = serde_json::json!({ "command": cmd });
    if let blastguard::model::Decision::Deny(reason) =
        blastguard::detect::detect("Bash", Some(&input))
    {
        let stderr = format!(
            "[blastguard] launch command `{cmd}` refused before sh -c (fail-closed) — {reason}"
        );
        return fail_soft_launch_verdict("", &stderr, None, "blastguard-denied");
    }

    // (c) Spawn via `sh -c`, piping both streams.
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let stderr = format!("failed to spawn `{cmd}`: {e}");
            return fail_soft_launch_verdict("", &stderr, None, "spawn-error");
        }
    };

    // (d) Poll health endpoint until timeout.
    let timeout = Duration::from_secs(startup_timeout_secs.max(1));
    let health_ok = probe_health_url(&host, port, &path, timeout);

    // (e) Kill the process.
    let _ = child.kill();
    let _ = child.wait();
    // Don't try to read from the pipes — the process was killed, so the pipes may
    // not close properly. Drop them instead to ensure no blocking.
    let _ = child.stdout.take();
    let _ = child.stderr.take();

    // (f) Return verdict based on health check result.
    if health_ok {
        // Health returned 200 — that IS the pass signal. Do NOT re-derive pass/
        // fail by scanning the (killed) server's logs through the panic detector:
        // a healthy server may legitimately log benign "Error:"/"Exception"/
        // "Traceback" lines during startup, and runtime_reflux_verdict would flip
        // those into a false failure. Return a clean pass, mirroring the bare
        // {kind,passed} shape (no runtime_digest on pass).
        serde_json::json!({ "kind": "runtime", "passed": true, "note": "health-ok" })
    } else {
        // Health check failed — return a fail-soft verdict with empty output.
        fail_soft_launch_verdict("", "", None, "health-timeout")
    }
}

/// Optional container hardening layered on top of the always-on
/// `--network=none` network isolation. Every field means "no cap" when
/// unset, so `SandboxLimits::default()` reproduces the legacy network-only
/// argv byte-for-byte (backward compatible).
#[derive(Debug, Default, Clone)]
pub struct SandboxLimits {
    /// `--memory` value (e.g. "512m"). None = no memory cap.
    pub memory: Option<String>,
    /// `--cpus` value (e.g. "1.5"). None = no cpu cap.
    pub cpus: Option<String>,
    /// `--pids-limit` value (e.g. 256). None = no pid cap.
    pub pids_limit: Option<i64>,
    /// When true, add `--read-only` (read-only container root fs). The
    /// `-v <workdir>:<workdir>` bind mount stays read-write so the worker can
    /// still edit/build inside its worktree — only the container root hardens.
    pub read_only: bool,
}

/// Build the argv (excluding the leading `docker`) for running `cmd` isolated
/// inside `image`, with `workdir` bind-mounted at the same path and no network:
/// `run --rm --network=none [hardening] -v <workdir>:<workdir> -w <workdir>
/// <image> sh -c <cmd>`. Any `limits` flags are inserted between
/// `--network=none` and the `-v` mount; with `SandboxLimits::default()` the
/// result is byte-identical to the legacy network-only argv. Pure and
/// unit-testable — no spawn.
fn docker_run_args(cmd: &str, image: &str, workdir: &str, limits: &SandboxLimits) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--network=none".to_string(),
    ];
    if limits.read_only {
        args.push("--read-only".to_string());
    }
    if let Some(m) = &limits.memory {
        args.push("--memory".to_string());
        args.push(m.clone());
    }
    if let Some(c) = &limits.cpus {
        args.push("--cpus".to_string());
        args.push(c.clone());
    }
    if let Some(p) = limits.pids_limit {
        args.push("--pids-limit".to_string());
        args.push(p.to_string());
    }
    args.extend([
        "-v".to_string(),
        format!("{workdir}:{workdir}"),
        "-w".to_string(),
        workdir.to_string(),
        image.to_string(),
        "sh".to_string(),
        "-c".to_string(),
        cmd.to_string(),
    ]);
    args
}

/// True iff the `docker` CLI is present AND its daemon is reachable, checked
/// by spawning `docker info` and inspecting the result. Fail-soft: a missing
/// binary (spawn error) or a non-zero exit (daemon down / permission denied)
/// both report `false` — never a panic.
fn docker_available() -> bool {
    match Command::new("docker")
        .arg("info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

/// Launch `cmd` isolated inside a Docker container and reflux its runtime
/// signals through the same deterministic verdict path as
/// [`launch_and_reflux`]. This is the container-backed sibling of that host
/// launcher: same blastguard gate, same `wait_timeout` envelope, same
/// [`distill_runtime`] shaping — only the spawn target differs (`docker run`
/// instead of `sh -c`).
///
/// **never-break-a-turn**: every branch returns a fail-soft verdict (JSON);
/// this function never `panic!`/`unwrap`/`expect`s on an external-input or
/// absent-tool path.
///
/// - **blastguard `Deny`**: `cmd` is refused *fail-closed* and NEVER reaches
///   docker (checked before the availability probe, so a flagged command
///   cannot even trigger a `docker info` call).
/// - **docker unavailable** (binary missing, or `docker info` exits non-zero
///   — daemon down / permission denied): a fail-soft verdict with
///   `note: "docker_unavailable"`. The command is NEVER run outside the
///   container as a fallback.
/// - **spawn failure** (docker binary vanished between the availability probe
///   and spawn): a fail-soft failure verdict (`note = "spawn-error"`).
/// - **timeout**: the container process is killed; fail-soft (`note =
///   "timeout"`).
/// - **normal exit**: the container's stdout/stderr/exit code are refluxed
///   through [`runtime_reflux_verdict`], whose pass/fail predicate decides the
///   verdict (exit 0 && no panic evidence ⇒ pass).
pub fn launch_in_container(
    cmd: &str,
    timeout_secs: u64,
    image: &str,
    workdir: &str,
    limits: &SandboxLimits,
) -> serde_json::Value {
    // (a) blastguard gate — validate BEFORE even checking docker availability,
    // reusing the same pure detector as the host launcher. A flagged command
    // is refused fail-closed and never reaches docker.
    let input = serde_json::json!({ "command": cmd });
    if let blastguard::model::Decision::Deny(reason) =
        blastguard::detect::detect("Bash", Some(&input))
    {
        let stderr = format!(
            "[blastguard] launch command `{cmd}` refused before docker run (fail-closed) — {reason}"
        );
        return fail_soft_launch_verdict("", &stderr, None, "blastguard-denied");
    }

    // (b) Docker availability gate — binary present AND daemon reachable.
    if !docker_available() {
        let stderr = format!(
            "docker unavailable (binary missing, daemon down, or permission denied); \
             cmd `{cmd}` was NOT run"
        );
        return fail_soft_launch_verdict("", &stderr, None, "docker_unavailable");
    }

    // (c) Spawn `docker run ...`, piping both streams so we can capture them.
    let timeout = timeout_secs.max(1);
    let args = docker_run_args(cmd, image, workdir, limits);
    let mut child = match Command::new("docker")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            // Fail-soft: docker could not even be started. No panic.
            let stderr = format!("failed to spawn `docker {}`: {e}", args.join(" "));
            return fail_soft_launch_verdict("", &stderr, None, "spawn-error");
        }
    };

    // (d) wait with a timeout; a timed-out container is killed and reaped.
    match child.wait_timeout(Duration::from_secs(timeout)) {
        Ok(Some(status)) => {
            // (e) normal exit — read both streams and reflux through the pure fn.
            let (stdout, stderr) = read_child_streams(&mut child);
            runtime_reflux_verdict(&stdout, &stderr, status.code())
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            let stderr =
                format!("container launch of `{cmd}` timed out after {timeout}s and was killed");
            fail_soft_launch_verdict("", &stderr, None, "timeout")
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            let stderr = format!("failed to wait on container running `{cmd}`: {e}");
            fail_soft_launch_verdict("", &stderr, None, "wait-error")
        }
    }
}

/// Deterministic RUN-POLICY gate: consult [`crate::run_policy::decide_run_policy`]
/// and invoke the injected container launcher ONLY on the `EscalateDocker`
/// verdict. No LLM, no direct I/O beyond calling the injected launcher — the
/// escalation DECISION is pure Rust, the actual container run is whatever the
/// caller injects (in the CLI: [`launch_in_container`]).
///
/// This is the deterministic in-code consumer the `run_policy` module refers to:
/// instead of the `/condukt` SKILL acting on the verdict via prose, the gate
/// mechanically routes `EscalateDocker` (and only that verdict) to the container
/// path. The other three verdicts (VerifyOnly / EscalateShip / AskHuman) NEVER
/// touch the launcher.
///
/// Returns a `serde_json::Value` object carrying at least the run-policy
/// `verdict` (snake_case), `reason`, `container_launched` (bool), and — only on
/// `EscalateDocker` — the nested container `launch` verdict. Never panics.
pub fn run_policy_gate<F>(
    cheap_verify: &str,
    divergence: &str,
    change_risk: &str,
    launch_container: F,
) -> serde_json::Value
where
    F: FnOnce() -> serde_json::Value,
{
    let decision = crate::run_policy::decide_run_policy(cheap_verify, divergence, change_risk);
    let verdict_str = match decision.verdict {
        crate::run_policy::RunPolicyVerdict::VerifyOnly => "verify_only",
        crate::run_policy::RunPolicyVerdict::EscalateDocker => "escalate_docker",
        crate::run_policy::RunPolicyVerdict::EscalateShip => "escalate_ship",
        crate::run_policy::RunPolicyVerdict::AskHuman => "ask_human",
    };
    let mut out = serde_json::json!({
        "kind": "run_policy_gate",
        "verdict": verdict_str,
        "reason": decision.reason,
        "cheap_verify": decision.cheap_verify,
        "divergence": decision.divergence,
        "change_risk": decision.change_risk,
        "container_launched": false,
    });
    // ONLY EscalateDocker takes the container path. The launcher is injected, so
    // this seam is unit-testable with a spy closure and needs no real docker.
    if matches!(
        decision.verdict,
        crate::run_policy::RunPolicyVerdict::EscalateDocker
    ) {
        let launch = launch_container();
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "container_launched".to_string(),
                serde_json::Value::Bool(true),
            );
            obj.insert("launch".to_string(), launch);
        }
    }
    out
}

/// Known model tiers, cheapest → strongest.
const TIERS: [&str; 3] = ["haiku", "sonnet", "opus"];

/// Collapse a model string to its canonical tier keyword when recognised
/// (e.g. `"claude-sonnet-4"` → `"sonnet"`), else the trimmed lowercase string.
/// Two models are "the same model" iff their canonical forms are equal.
fn canonical(model: &str) -> String {
    let m = model.trim().to_lowercase();
    for t in TIERS {
        if m.contains(t) {
            return t.to_string();
        }
    }
    m
}

/// Position of a model within [`TIERS`] (by canonical tier), if recognised.
fn tier_index(model: &str) -> Option<usize> {
    let c = canonical(model);
    TIERS.iter().position(|t| *t == c)
}

/// True iff `a` and `b` denote the same model (so using both for worker and
/// verifier would share a blind spot).
pub fn same_model(a: &str, b: &str) -> bool {
    canonical(a) == canonical(b)
}

/// Resolve the verifier model, guaranteeing it differs from the worker model.
///
/// `suggested` is the `verifier_model` from `route.json` (may be absent/empty).
/// Invariant: the returned model is never the same model as `worker`.
///
/// - A distinct, non-empty `suggested` is honoured as-is.
/// - Otherwise a distinct tier is chosen: prefer a *stronger* verifier than the
///   worker (independent, higher-signal check); if the worker is already at the
///   top tier, step down one tier so the verifier is still independent.
/// - An unrecognised worker model defaults the verifier to the strongest tier.
pub fn resolve_verifier_model(worker: &str, suggested: Option<&str>) -> String {
    if let Some(s) = suggested {
        let s = s.trim();
        if !s.is_empty() && !same_model(s, worker) {
            return s.to_string();
        }
    }
    match tier_index(worker) {
        Some(i) if i + 1 < TIERS.len() => TIERS[i + 1].to_string(),
        Some(i) if i > 0 => TIERS[i - 1].to_string(),
        // haiku with no stronger-but-that-branch-taken (i==0 handled above) or
        // an unrecognised model → strongest independent tier.
        _ => "opus".to_string(),
    }
}

/// Markers that mean the criteria demands judgement about implementation /
/// logic / design / behaviour / correctness. Their presence forces the LLM
/// verifier to run even if an accompanying command exits 0. Bilingual because
/// the skill and its decompositions mix Japanese and English.
const BEHAVIORAL_MARKERS: &[&str] = &[
    // English
    "implement",
    "logic",
    "design",
    "behavior",
    "behaviour",
    "correct",
    "refactor",
    "handle",
    "ensure",
    "semantic",
    "invariant",
    "properly",
    "prove",
    "prevent",
    "enforce",
    // Runtime / health markers: a criteria that asks about *running* behaviour
    // (server starts, /health returns 200, no runtime panic) demands the verifier
    // actually launch the target — a passing unit test is evidence, not a
    // substitute. These force the verifier even when a command is embedded.
    "runtime",
    "health",
    // Japanese (SKILL.md wording: 実装/ロジック/設計/コード/振る舞い …)
    "実装",
    "ロジック",
    "設計",
    "コード",
    "振る舞い",
    "挙動",
    "正しく",
    "妥当",
    "検証",
    // Japanese runtime / health markers.
    "実行時",
    "起動",
    "稼働",
];

/// True iff `done_criteria` carries any behavioral marker — i.e. it asks about
/// *what the code does*, not merely an observable mechanical fact.
pub fn criteria_is_behavioral(done_criteria: &str) -> bool {
    let lower = done_criteria.to_lowercase();
    BEHAVIORAL_MARKERS
        .iter()
        .any(|m| lower.contains(&m.to_lowercase()))
}

/// Classification of a done_criteria for the verifier-skip decision.
#[derive(Debug, Clone)]
pub struct Classification {
    /// The criteria carries behavioral markers (judgement required).
    pub behavioral: bool,
    /// A runnable mechanical check derived from the criteria, if any.
    pub mechanical_cmd: Option<Vec<String>>,
    /// The LLM verifier may be skipped ONLY when this is true: a mechanical
    /// command exists AND the criteria carries no behavioral markers. Any
    /// ambiguity resolves to `false` (run the verifier — the safe side).
    pub skip_eligible: bool,
}

/// Classify a done_criteria: behavioral vs purely mechanical, and whether the
/// verifier may be skipped. Behavioral criteria are never skip-eligible even
/// when they embed a runnable command.
pub fn classify_criteria(done_criteria: &str) -> Classification {
    let behavioral = criteria_is_behavioral(done_criteria);
    let mechanical_cmd = mechanical_cmd(done_criteria);
    let skip_eligible = !behavioral && mechanical_cmd.is_some();
    Classification {
        behavioral,
        mechanical_cmd,
        skip_eligible,
    }
}

/// Build the verify-gate verdict for the purely-mechanical branch.
///
/// [`classify_criteria`] sets `skip_eligible` only when `mechanical_cmd.is_some()`,
/// so a `skip_eligible` classification with no command is supposed to be impossible.
/// If that invariant is ever violated (schema drift, external JSON, a future
/// refactor) we must NOT panic in an unattended run — a panic there breaks the turn.
/// Instead we fail soft: emit a verdict that refuses to skip the verifier, since
/// running the verifier is the safe side.
///
/// `run` runs the mechanical command, returning `(passed, output)`; it is only
/// invoked when a command actually exists.
///
/// Returns `(verdict_json, gate_failed)`. `gate_failed` is true only when a real
/// mechanical command was run and failed (the caller then fails this gate). A
/// missing command never fails the gate — the verifier still runs.
pub fn mechanical_skip_verdict(
    cls: &Classification,
    run: impl FnOnce(&[String]) -> (bool, String),
) -> (serde_json::Value, bool) {
    let Some(cmd) = cls.mechanical_cmd.as_ref() else {
        // Invariant-violating input: skip_eligible with no command. Fail soft —
        // refuse to skip the verifier rather than panicking an unattended run.
        let out = serde_json::json!({
            "mechanical": true,
            "behavioral": cls.behavioral,
            "passed": false,
            "skip_verifier": false,
            "reason": "skip_eligible classification carried no mechanical command; \
                       refusing to skip the verifier (safe side)",
        });
        return (out, false);
    };
    let (passed, output) = run(cmd);
    let mut out = serde_json::json!({
        "mechanical": true,
        "behavioral": false,
        "passed": passed,
        "skip_verifier": passed,
        "cmd": cmd,
        "output": output,
    });
    // On failure, attach the deterministic structured digest alongside the raw
    // output so the verifier→worker reflux carries the *why*, not just a boolean.
    // The passing-case shape is left unchanged.
    if !passed {
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "failure_digest".to_string(),
                serde_json::to_value(distill_failure(&output)).unwrap_or(serde_json::Value::Null),
            );
        }
    }
    (out, !passed)
}

/// Extract a runnable command from a done_criteria string for mechanical gate
/// checking. Returns `None` when no mechanical check can be derived (the LLM
/// verifier is then required). This is intentionally about *runnability* only;
/// [`classify_criteria`] layers the behavioral veto on top.
pub fn mechanical_cmd(done_criteria: &str) -> Option<Vec<String>> {
    // Prefer an explicit backtick command: `cargo test -p condukt`
    if let Ok(re) = regex::Regex::new(r"`([^`]+)`") {
        for caps in re.captures_iter(done_criteria) {
            if let Some(inner) = caps.get(1) {
                let argv: Vec<String> = inner
                    .as_str()
                    .split_whitespace()
                    .map(String::from)
                    .collect();
                if argv.first().is_some_and(|p| is_criteria_runner(p)) {
                    return Some(argv);
                }
            }
        }
    }
    // Fall back to recognised test-runner prose.
    let lower = done_criteria.to_lowercase();
    if lower.contains("cargo test") {
        let mut cmd = vec!["cargo".to_string(), "test".to_string()];
        if let Ok(re2) = regex::Regex::new(r"-p\s+([A-Za-z0-9_-]+)") {
            if let Some(c) = re2.captures(done_criteria).and_then(|c| c.get(1)) {
                cmd.push("-p".to_string());
                cmd.push(c.as_str().to_string());
            }
        }
        return Some(cmd);
    }
    if lower.contains("npm test") {
        return Some(vec!["npm".to_string(), "test".to_string()]);
    }
    if lower.contains("pytest") {
        return Some(vec!["pytest".to_string()]);
    }
    if lower.contains("go test") {
        return Some(vec!["go".to_string(), "test".to_string()]);
    }
    None
}

fn is_criteria_runner(tok: &str) -> bool {
    matches!(
        tok,
        "cargo"
            | "npm"
            | "npx"
            | "pytest"
            | "go"
            | "make"
            | "bash"
            | "sh"
            | "python"
            | "python3"
            | "node"
            | "yarn"
            | "pnpm"
            | "just"
    )
}

/// Deterministic set-difference of failing-test names: the tests that fail
/// *now* but were **not** already failing at baseline (i.e. genuine regressions
/// introduced by the change under verification).
///
/// Pure and deterministic: `BTreeSet` fixes a stable sort order, so the same
/// inputs always yield the same `Vec` in the same order. A test that was already
/// red at baseline and is still red is **not** a regression (it is pre-existing),
/// and a baseline failure that disappears is likewise never a regression.
///
/// Input sets are typically built from [`distill_failure`]'s `failing_tests`
/// (e.g. `current.failing_tests.iter().cloned().collect()`).
pub fn regressions(
    current_failing: &BTreeSet<String>,
    baseline_failing: &BTreeSet<String>,
) -> Vec<String> {
    current_failing
        .difference(baseline_failing)
        .cloned()
        .collect()
}

/// `true` iff there are no regressions relative to baseline (the verify gate's
/// baseline-failure-excluded pass condition). Thin, deterministic companion to
/// [`regressions`].
pub fn regressions_passed(
    current_failing: &BTreeSet<String>,
    baseline_failing: &BTreeSet<String>,
) -> bool {
    regressions(current_failing, baseline_failing).is_empty()
}

/// Build the set of failing-test names from either a captured test-output blob
/// OR a bare newline-list of names. Reuses [`distill_failure`]'s extraction
/// (the same `test <name> ... FAILED` / `failures:` parsing); when that yields
/// nothing — the input is a plain name list, not cargo output — each non-empty
/// trimmed line is taken verbatim as a name. Pure and deterministic.
pub fn failing_name_set(raw: &str) -> BTreeSet<String> {
    let digest = distill_failure(raw);
    if !digest.failing_tests.is_empty() {
        return digest.failing_tests.into_iter().collect();
    }
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Verifier confidence, derived from *observed facts* rather than the LLM's
/// free-text self-report. Its lowercase [`VerifierConfidence::as_str`] matches
/// the existing string confidence vocabulary (`"high" | "medium" | "low"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifierConfidence {
    High,
    Medium,
    Low,
}

impl VerifierConfidence {
    /// The canonical lowercase token, aligned with the verifier's existing
    /// `confidence` string field (`"high" | "medium" | "low"`).
    pub fn as_str(self) -> &'static str {
        match self {
            VerifierConfidence::High => "high",
            VerifierConfidence::Medium => "medium",
            VerifierConfidence::Low => "low",
        }
    }
}

/// The observed facts of a verification run, from which confidence is derived.
/// Purely mechanical inputs — no LLM judgement — so [`derive_confidence`] is a
/// deterministic function of what actually happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyFacts {
    /// A runnable check / repro command existed **and was actually executed**.
    /// `false` means either none was available or it never ran (verification
    /// then rests on free-text argument only).
    pub check_executed: bool,
    /// The executed check / repro exited `0`. Meaningful only when
    /// `check_executed` is `true`.
    pub check_exit_zero: bool,
    /// No regressions relative to baseline (see [`regressions`]).
    pub no_regressions: bool,
}

/// Derive verifier confidence from observed facts (a pure, deterministic
/// front-stage that supplements — never replaces — the LLM verifier's own
/// self-reported confidence).
///
/// - A runnable check/repro **ran, exited 0, and produced no regressions** →
///   [`VerifierConfidence::High`].
/// - **No runnable check/repro, or it never ran** (verification leans on
///   free-text argument only) → [`VerifierConfidence::Low`].
/// - Anything in between — it ran but failed or regressed —
///   → [`VerifierConfidence::Medium`].
pub fn derive_confidence(facts: &VerifyFacts) -> VerifierConfidence {
    if !facts.check_executed {
        return VerifierConfidence::Low;
    }
    if facts.check_exit_zero && facts.no_regressions {
        return VerifierConfidence::High;
    }
    VerifierConfidence::Medium
}

// ── declared deterministic checks (machine oracle for done_criteria) ──────────

/// Pure, deterministic pass/fail for one check given the OBSERVED result of
/// running its command. Passes iff the exit code matches `expect_exit` (default
/// 0) AND — when `expect_substring` is set — the combined output contains it.
pub fn check_passed(
    check: &crate::model::Check,
    observed_exit: i64,
    observed_output: &str,
) -> bool {
    let expected_exit = check.expect_exit.unwrap_or(0);
    if observed_exit != expected_exit {
        return false;
    }
    match check.expect_substring.as_deref() {
        Some(needle) => observed_output.contains(needle),
        None => true,
    }
}

/// Aggregate verdict over a set of per-check results: `true` iff every result is
/// `true`. An empty slice is a vacuous pass (`true`).
pub fn checks_verdict(results: &[bool]) -> bool {
    results.iter().all(|&r| r)
}

/// The outcome of running one declared check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckResult {
    /// The shell command that was run.
    pub cmd: String,
    /// Whether the observed exit/substring satisfied the check (see
    /// [`check_passed`]).
    pub passed: bool,
    /// The observed process exit code (`-1` when the process was killed by a
    /// signal and produced no code).
    pub exit: i64,
}

/// The aggregate report of running a set of declared checks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckReport {
    /// `true` iff every check passed (see [`checks_verdict`]).
    pub all_passed: bool,
    /// Per-check results, in declaration order.
    pub results: Vec<CheckResult>,
}

/// Execute one declared check: run its command via `sh -c` (mirroring the
/// launch pattern elsewhere in this module), capture the exit code and combined
/// stdout+stderr, and evaluate it with the pure [`check_passed`]. Fail-soft: a
/// spawn error yields a non-passing result with `exit = -1` rather than panicking.
pub fn run_check(check: &crate::model::Check, cwd: Option<&std::path::Path>) -> CheckResult {
    let mut command = Command::new("sh");
    command.arg("-c").arg(&check.cmd).stdin(Stdio::null());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    match command.output() {
        Ok(output) => {
            let exit = output.status.code().map(i64::from).unwrap_or(-1);
            let mut combined = output.stdout;
            combined.extend_from_slice(&output.stderr);
            let combined = String::from_utf8_lossy(&combined);
            CheckResult {
                cmd: check.cmd.clone(),
                passed: check_passed(check, exit, &combined),
                exit,
            }
        }
        Err(_) => CheckResult {
            cmd: check.cmd.clone(),
            passed: false,
            exit: -1,
        },
    }
}

/// Execute a set of declared checks in order and aggregate the verdict.
pub fn run_checks(checks: &[crate::model::Check], cwd: Option<&std::path::Path>) -> CheckReport {
    let results: Vec<CheckResult> = checks.iter().map(|c| run_check(c, cwd)).collect();
    let all_passed = checks_verdict(&results.iter().map(|r| r.passed).collect::<Vec<_>>());
    CheckReport {
        all_passed,
        results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── t1: deterministic regression set-diff (baseline-failure exclusion) ──

    fn nameset(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A newly-failing test (present in current, absent at baseline) is a
    /// regression; the gate does not pass.
    #[test]
    fn regression_new_failure_is_flagged() {
        let baseline = nameset(&["A"]);
        let current = nameset(&["A", "B"]);
        assert_eq!(regressions(&current, &baseline), vec!["B".to_string()]);
        assert!(!regressions_passed(&current, &baseline));
    }

    /// No change in the failing set → no regressions, gate passes.
    #[test]
    fn regression_identical_sets_pass() {
        let baseline = nameset(&["A"]);
        let current = nameset(&["A"]);
        assert!(regressions(&current, &baseline).is_empty());
        assert!(regressions_passed(&current, &baseline));
    }

    /// A pre-existing baseline failure that clears is NOT a regression; the gate
    /// still passes (baseline reds are excluded, never counted against current).
    #[test]
    fn regression_cleared_baseline_failure_is_not_regression() {
        let baseline = nameset(&["A", "B"]);
        let current = nameset(&["A"]);
        assert!(regressions(&current, &baseline).is_empty());
        assert!(regressions_passed(&current, &baseline));
    }

    /// Determinism: identical inputs yield identical, stably-sorted output.
    #[test]
    fn regression_output_is_deterministic_and_sorted() {
        let baseline = nameset(&["x"]);
        let current = nameset(&["c", "a", "x", "b"]);
        let r1 = regressions(&current, &baseline);
        let r2 = regressions(&current, &baseline);
        assert_eq!(r1, r2);
        assert_eq!(r1, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    // ── t2: derive verifier confidence from observed facts ─────────────────

    /// Ran a check, it exited 0, no regressions → High.
    #[test]
    fn confidence_high_when_ran_clean_and_no_regression() {
        let facts = VerifyFacts {
            check_executed: true,
            check_exit_zero: true,
            no_regressions: true,
        };
        assert_eq!(derive_confidence(&facts), VerifierConfidence::High);
        assert_eq!(derive_confidence(&facts).as_str(), "high");
    }

    /// No runnable check executed (free-text argument only) → Low, regardless of
    /// the other flags.
    #[test]
    fn confidence_low_when_no_check_executed() {
        let facts = VerifyFacts {
            check_executed: false,
            check_exit_zero: true,
            no_regressions: true,
        };
        assert_eq!(derive_confidence(&facts), VerifierConfidence::Low);
        assert_eq!(derive_confidence(&facts).as_str(), "low");
    }

    /// Ran but a regression appeared → Medium (intermediate).
    #[test]
    fn confidence_medium_when_ran_but_regressed() {
        let facts = VerifyFacts {
            check_executed: true,
            check_exit_zero: true,
            no_regressions: false,
        };
        assert_eq!(derive_confidence(&facts), VerifierConfidence::Medium);
        assert_eq!(derive_confidence(&facts).as_str(), "medium");
    }

    /// Ran but exited non-zero → Medium (intermediate), even with no regression
    /// set computed.
    #[test]
    fn confidence_medium_when_ran_but_nonzero_exit() {
        let facts = VerifyFacts {
            check_executed: true,
            check_exit_zero: false,
            no_regressions: true,
        };
        assert_eq!(derive_confidence(&facts), VerifierConfidence::Medium);
    }

    // ── Invariant 1: verifier model never equals worker model ──────────────

    /// Across every worker tier, and whether the suggested verifier is absent,
    /// empty, or identical to the worker, the resolved verifier must differ.
    #[test]
    fn verifier_model_never_equals_worker() {
        let workers = [
            "haiku",
            "sonnet",
            "opus",
            "claude-sonnet-4",
            "mystery-model",
        ];
        for w in workers {
            let canon = canonical(w);
            // No suggestion, empty/blank suggestion, or a suggestion that is the
            // same model (exact string or canonical tier) must all still yield a
            // verifier that differs from the worker.
            let suggestions: [Option<&str>; 5] =
                [None, Some(""), Some("  "), Some(w), Some(canon.as_str())];
            for s in suggestions {
                let v = resolve_verifier_model(w, s);
                assert!(
                    !same_model(&v, w),
                    "verifier {v:?} must differ from worker {w:?} (suggested={s:?})"
                );
            }
        }
    }

    /// A distinct, explicit suggestion is honoured verbatim.
    #[test]
    fn distinct_suggestion_is_honoured() {
        assert_eq!(resolve_verifier_model("sonnet", Some("opus")), "opus");
        assert_eq!(resolve_verifier_model("opus", Some("haiku")), "haiku");
    }

    /// The fallback prefers a stronger tier, or steps down from the top tier.
    #[test]
    fn fallback_picks_distinct_tier() {
        assert_eq!(resolve_verifier_model("haiku", None), "sonnet");
        assert_eq!(resolve_verifier_model("sonnet", None), "opus");
        // Worker already at the top → step down to stay independent.
        assert_eq!(resolve_verifier_model("opus", None), "sonnet");
        // Unknown worker → strongest independent tier.
        assert_eq!(resolve_verifier_model("weird", None), "opus");
    }

    // ── Invariant 2: behavioral criteria never skip the verifier ───────────

    /// A behavioral criteria that ALSO embeds a passing test command must NOT
    /// be skip-eligible: the passing test is evidence, not a substitute.
    #[test]
    fn behavioral_criteria_never_skips_verifier() {
        let dc = "Implement the retry logic correctly; `cargo test -p condukt` passes";
        let c = classify_criteria(dc);
        assert!(c.behavioral, "criteria must be classified behavioral");
        assert!(
            c.mechanical_cmd.is_some(),
            "the embedded command is still extracted (as evidence)"
        );
        assert!(
            !c.skip_eligible,
            "behavioral criteria must NEVER be skip-eligible even with a passing test"
        );
    }

    /// A purely mechanical criteria (observable fact, no judgement words) may
    /// skip the verifier.
    #[test]
    fn purely_mechanical_criteria_is_skip_eligible() {
        let c = classify_criteria("`cargo test -p condukt` exits 0");
        assert!(!c.behavioral);
        assert_eq!(
            c.mechanical_cmd.as_deref(),
            Some(&["cargo", "test", "-p", "condukt"].map(String::from)[..])
        );
        assert!(c.skip_eligible, "a plain passing-test criteria may skip");
    }

    /// No runnable command → not skip-eligible (verifier must run).
    #[test]
    fn non_runnable_criteria_is_not_skip_eligible() {
        let c = classify_criteria("the README documents the new flag");
        assert!(c.mechanical_cmd.is_none());
        assert!(!c.skip_eligible);
    }

    // ── Fail-soft: invariant-violating skip_eligible must not panic ────────

    /// A `skip_eligible` classification whose `mechanical_cmd` is `None` violates
    /// the classifier invariant (e.g. from schema drift / external JSON). The
    /// verdict builder must NOT panic; it must refuse to skip the verifier
    /// (`skip_verifier == false`), carry a `reason`, and not fail the gate.
    #[test]
    fn skip_eligible_without_command_fails_soft() {
        let cls = Classification {
            behavioral: false,
            mechanical_cmd: None,
            skip_eligible: true,
        };
        // The runner must never be called when there is no command.
        let (verdict, gate_failed) = mechanical_skip_verdict(&cls, |_cmd| {
            panic!("runner must not be invoked when there is no mechanical command");
        });
        assert_eq!(
            verdict["skip_verifier"],
            serde_json::json!(false),
            "invariant-violating input must NOT skip the verifier (safe side)"
        );
        assert!(
            verdict.get("reason").and_then(|r| r.as_str()).is_some(),
            "the verdict must carry a machine-readable reason: {verdict}"
        );
        assert!(
            !gate_failed,
            "a missing command must not fail the gate — the verifier still runs"
        );
    }

    /// Valid case: `skip_eligible` with a real command runs it; `skip_verifier`
    /// tracks the command result and a failing command fails the gate.
    #[test]
    fn skip_eligible_with_command_runs_and_tracks_result() {
        let cls = Classification {
            behavioral: false,
            mechanical_cmd: Some(vec!["cargo".to_string(), "test".to_string()]),
            skip_eligible: true,
        };
        // Passing command → skip the verifier, gate not failed.
        let (v_pass, failed_pass) =
            mechanical_skip_verdict(&cls, |cmd| (true, format!("ran {cmd:?}")));
        assert_eq!(v_pass["skip_verifier"], serde_json::json!(true));
        assert_eq!(v_pass["passed"], serde_json::json!(true));
        assert_eq!(v_pass["cmd"], serde_json::json!(["cargo", "test"]));
        assert!(!failed_pass);

        // Failing command → do not skip, gate fails.
        let (v_fail, failed_fail) = mechanical_skip_verdict(&cls, |_cmd| (false, "boom".into()));
        assert_eq!(v_fail["skip_verifier"], serde_json::json!(false));
        assert_eq!(v_fail["passed"], serde_json::json!(false));
        assert!(failed_fail, "a failing mechanical check must fail the gate");
    }

    // ── Structured failure digest (verifier→worker reflux) ────────────────

    /// A representative failing cargo-test output must yield detail BEYOND the
    /// pass/fail boolean: the failing test name and the assertion evidence. A
    /// neutered `distill_failure` returning an empty digest would fail this.
    #[test]
    fn distill_surfaces_test_name_and_assertion_diff() {
        let raw = "\
running 2 tests
test foo::bar ... FAILED
test foo::baz ... ok

failures:

---- foo::bar stdout ----
thread 'foo::bar' panicked at src/lib.rs:42:5:
assertion `left == right` failed
  left: 3
 right: 4

failures:
    foo::bar

test result: FAILED. 1 passed; 1 failed; 0 ignored";
        let d = distill_failure(raw);
        // (a) the failing test name is surfaced.
        assert!(
            d.failing_tests.iter().any(|t| t == "foo::bar"),
            "expected failing test 'foo::bar' in {:?}",
            d.failing_tests
        );
        // (b) assertion evidence is surfaced (the "why" beyond the boolean).
        assert!(
            d.assertion_diffs
                .iter()
                .any(|a| a.contains("assertion `left == right` failed")),
            "expected the assertion line in {:?}",
            d.assertion_diffs
        );
        // left/right evidence is captured too.
        assert!(
            d.assertion_diffs.iter().any(|a| a.starts_with("left:")),
            "expected the left value line in {:?}",
            d.assertion_diffs
        );
        assert!(
            d.assertion_diffs.iter().any(|a| a.starts_with("right:")),
            "expected the right value line in {:?}",
            d.assertion_diffs
        );
        // The panic location is captured as evidence.
        assert!(
            d.assertion_diffs.iter().any(|a| a.contains("panicked at")),
            "expected the panic line in {:?}",
            d.assertion_diffs
        );
        // The tail retains the last lines of output.
        assert!(
            d.output_tail.contains("test result: FAILED"),
            "output_tail must retain the trailing summary: {:?}",
            d.output_tail
        );
        // Names are deduplicated: 'foo::bar' appears in both the result line and
        // the summary block, but must be listed once.
        assert_eq!(
            d.failing_tests.iter().filter(|t| *t == "foo::bar").count(),
            1,
            "failing test names must be deduplicated: {:?}",
            d.failing_tests
        );
    }

    /// Empty input must not panic and must yield empty vecs + empty tail.
    #[test]
    fn distill_empty_input_is_graceful() {
        let d = distill_failure("");
        assert!(d.failing_tests.is_empty());
        assert!(d.assertion_diffs.is_empty());
        assert_eq!(d.output_tail, "");
    }

    /// Garbage / non-cargo input must not panic and yields no false positives,
    /// but still keeps the tail.
    #[test]
    fn distill_garbage_input_is_graceful() {
        let raw = "some unrelated log line\nanother line without markers";
        let d = distill_failure(raw);
        assert!(d.failing_tests.is_empty());
        assert!(d.assertion_diffs.is_empty());
        assert_eq!(d.output_tail, raw);
    }

    /// The mechanical verdict embeds the digest on failure and omits it on pass.
    #[test]
    fn mechanical_verdict_embeds_digest_only_on_failure() {
        let cls = Classification {
            behavioral: false,
            mechanical_cmd: Some(vec!["cargo".to_string(), "test".to_string()]),
            skip_eligible: true,
        };
        // Passing case: no failure_digest field (shape unchanged).
        let (v_pass, _) = mechanical_skip_verdict(&cls, |_c| (true, "all good".into()));
        assert!(
            v_pass.get("failure_digest").is_none(),
            "passing verdict must not carry a failure_digest: {v_pass}"
        );
        // Failing case: failure_digest present and populated.
        let (v_fail, failed) =
            mechanical_skip_verdict(&cls, |_c| (false, "test foo::bar ... FAILED".into()));
        assert!(failed);
        let digest = v_fail
            .get("failure_digest")
            .expect("failing verdict must carry a failure_digest");
        assert_eq!(
            digest["failing_tests"],
            serde_json::json!(["foo::bar"]),
            "digest must surface the failing test: {v_fail}"
        );
        // The raw output is still present alongside the digest.
        assert!(v_fail.get("output").is_some());
    }

    // ── Structured runtime digest (phase-3 runtime FB reflux) ─────────────

    /// A representative failing run — non-zero exit, a panic line, and stderr
    /// content — must surface ALL of (a) the exit code, (b) the panic evidence
    /// line, and (c) the stderr tail. A neutered `distill_runtime` that dropped
    /// exit_code / panics / stderr_tail would genuinely FAIL this.
    #[test]
    fn distill_runtime_surfaces_exit_panic_and_stderr() {
        let stdout = "starting up\ndoing work\n";
        let stderr = "\
thread 'main' panicked at src/main.rs:10:5:
index out of bounds: the len is 0 but the index is 3
note: run with `RUST_BACKTRACE=1` for a backtrace";
        let d = distill_runtime(stdout, stderr, Some(101));
        // (a) the exit code is surfaced verbatim.
        assert_eq!(d.exit_code, Some(101), "exit_code must be surfaced");
        // (b) the panic evidence line is surfaced.
        assert!(
            d.panics.iter().any(|p| p.contains("panicked at")),
            "expected the panic line in {:?}",
            d.panics
        );
        // (c) the stderr tail retains the trailing stderr content.
        assert!(
            d.stderr_tail.contains("index out of bounds"),
            "stderr_tail must retain trailing stderr: {:?}",
            d.stderr_tail
        );
        // stdout tail is captured independently of stderr.
        assert!(
            d.stdout_tail.contains("doing work"),
            "stdout_tail must retain trailing stdout: {:?}",
            d.stdout_tail
        );
    }

    /// Panic evidence is gathered from BOTH streams, deduplicated, with stderr
    /// preferred first-seen. A line present in both streams appears once.
    #[test]
    fn distill_runtime_collects_from_both_streams_deduped() {
        let shared = "Traceback (most recent call last):";
        let stdout = format!("stdout noise\n{shared}\nError: boom from stdout");
        let stderr = format!("{shared}\nException: kaboom");
        let d = distill_runtime(&stdout, &stderr, None);
        // The shared line is deduplicated to a single entry.
        assert_eq!(
            d.panics.iter().filter(|p| *p == shared).count(),
            1,
            "shared evidence must be deduplicated: {:?}",
            d.panics
        );
        // stderr is scanned first, so its shared line wins first-seen order.
        assert_eq!(
            d.panics.first().map(String::as_str),
            Some(shared),
            "stderr evidence must be first-seen: {:?}",
            d.panics
        );
        // Evidence from both streams is present.
        assert!(d.panics.iter().any(|p| p.contains("Exception")));
        assert!(d.panics.iter().any(|p| p.contains("Error:")));
        // No exit code was provided.
        assert_eq!(d.exit_code, None);
    }

    /// Empty input must not panic and must yield an empty digest.
    #[test]
    fn distill_runtime_empty_input_is_graceful() {
        let d = distill_runtime("", "", None);
        assert_eq!(d.exit_code, None);
        assert!(d.panics.is_empty());
        assert_eq!(d.stderr_tail, "");
        assert_eq!(d.stdout_tail, "");
    }

    /// Garbage input — a long newline-free string and a dense symbol run — must
    /// not panic and must yield no false-positive panics, but keep the tails.
    #[test]
    fn distill_runtime_garbage_input_is_graceful() {
        let huge = "x".repeat(50_000);
        let symbols = "�\u{0}\t!@#$%^&*()_+{}|:<>?~`-=[]\\;',./\u{1b}[31m";
        let d = distill_runtime(&huge, symbols, Some(-1));
        assert!(
            d.panics.is_empty(),
            "no marker present → no panics: {:?}",
            d.panics
        );
        // Single-line (no `\n`) input is retained whole as the tail.
        assert_eq!(d.stdout_tail, huge);
        assert_eq!(d.stderr_tail, symbols);
        assert_eq!(d.exit_code, Some(-1));
    }

    // ── Runtime reflux verdict (phase-3 verifier→worker reflux) ───────────

    /// RED existence: a runtime failure — non-zero exit + a panic line + stderr
    /// content — must produce a reflux verdict that carries, BEYOND the pass/fail
    /// boolean, the runtime diagnostics: the exit code, the panic evidence line,
    /// and the stderr tail. Neutering the `runtime_digest` embedding (dropping the
    /// `obj.insert`) makes `.expect("runtime_digest")` panic → genuine FAIL; a
    /// `distill_runtime` that dropped exit_code / panics / stderr_tail also FAILs.
    #[test]
    fn runtime_reflux_verdict_embeds_diagnostics_on_failure() {
        let stdout = "booting\n";
        let stderr = "\
thread 'main' panicked at src/main.rs:10:5:
index out of bounds: the len is 0 but the index is 3
note: run with `RUST_BACKTRACE=1` for a backtrace";
        let v = runtime_reflux_verdict(stdout, stderr, Some(101));
        // The verdict states pass/fail, and this run is a failure.
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "non-zero exit + panic must be a runtime failure: {v}"
        );
        // BEYOND the boolean: the structured runtime digest is embedded.
        let d = v
            .get("runtime_digest")
            .expect("a failing runtime verdict must carry a runtime_digest");
        // (a) the exit code is surfaced.
        assert_eq!(
            d["exit_code"],
            serde_json::json!(101),
            "runtime_digest must surface the exit code: {v}"
        );
        // (b) the panic evidence line is surfaced.
        assert!(
            d["panics"].as_array().is_some_and(|a| a
                .iter()
                .any(|p| p.as_str().is_some_and(|s| s.contains("panicked at")))),
            "runtime_digest must surface the panic line: {v}"
        );
        // (c) the stderr tail retains the trailing stderr content.
        assert!(
            d["stderr_tail"]
                .as_str()
                .is_some_and(|s| s.contains("index out of bounds")),
            "runtime_digest must surface the stderr tail: {v}"
        );
    }

    /// A clean run — exit 0, no panics — passes and omits the digest (the passing
    /// shape stays a bare boolean, mirroring the failure_digest omission on pass).
    #[test]
    fn runtime_reflux_verdict_pass_omits_digest() {
        let v = runtime_reflux_verdict("all good\n", "", Some(0));
        assert_eq!(v["passed"], serde_json::json!(true));
        assert!(
            v.get("runtime_digest").is_none(),
            "a passing runtime verdict must not carry a runtime_digest: {v}"
        );
    }

    /// Panic evidence alone marks a failure even when the exit code is 0 (a
    /// process can panic-catch and still exit 0): the reflux must still fail and
    /// embed the digest so the panic reaches the worker.
    #[test]
    fn runtime_reflux_verdict_fails_on_panic_even_with_zero_exit() {
        let v = runtime_reflux_verdict("", "thread 'worker' panicked at lib.rs:1:1", Some(0));
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "a panic must fail the runtime verdict regardless of exit code: {v}"
        );
        assert!(
            v.get("runtime_digest").is_some(),
            "the panic evidence must be embedded for the worker: {v}"
        );
    }

    /// The reflux carries only observable facts — never a fix instruction. This
    /// pins the LLM/Rust separation: no "how to fix" field leaks into the verdict.
    #[test]
    fn runtime_reflux_verdict_carries_no_fix_decision() {
        let v = runtime_reflux_verdict("", "Error: boom", Some(2));
        let obj = v.as_object().expect("verdict is a JSON object");
        // Only the mechanical keys are present; nothing prescribing a fix.
        for k in obj.keys() {
            assert!(
                matches!(k.as_str(), "kind" | "passed" | "runtime_digest"),
                "unexpected key {k:?} — the verdict must stay fact-only (no fix decision): {v}"
            );
        }
    }

    // ── Real process launch + fail-soft (phase-3 DoD#3) ───────────────────

    /// RED existence: a blastguard-flagged command (recursive rm) must be
    /// refused BEFORE `sh -c` runs. The benign leading segment (`touch sentinel`)
    /// must never execute — the surviving-absent sentinel proves the shell was
    /// not invoked. Neuter oracle: removing the blastguard gate lets the shell
    /// run, so the sentinel is created (this test's `!exists` FAILs) and the
    /// benign rm-on-missing exits 0 (`passed` becomes true → FAILs too).
    #[test]
    fn launch_refuses_destructive_command_without_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("ran.txt");
        let victim = tmp.path().join("victim");
        let payload = format!("touch {} ; rm -rf {}", sentinel.display(), victim.display());
        let v = launch_and_reflux(&payload, 5);
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "a refused command must not count as passed: {v}"
        );
        assert_eq!(v["note"], serde_json::json!("blastguard-denied"));
        let d = v
            .get("runtime_digest")
            .expect("a refusal must carry a runtime_digest");
        assert!(
            d["stderr_tail"]
                .as_str()
                .is_some_and(|s| s.contains("blastguard")),
            "the refusal reason must name the guard: {v}"
        );
        assert!(
            !sentinel.exists(),
            "sh -c must NOT have run — a created sentinel would prove the payload executed"
        );
    }

    /// RED existence: a benign (blastguard-allowed) command that exits non-zero
    /// and writes stderr must reflux a runtime FAILURE whose digest carries the
    /// diagnostics BEYOND the boolean — the exit code and the stderr tail.
    /// Neuter oracle: dropping the `runtime_digest` embed makes `.expect` panic;
    /// dropping the exit-code reflux makes the `exit_code == 3` assert FAIL.
    #[test]
    fn launch_refluxes_runtime_failure_with_diagnostics() {
        let v = launch_and_reflux("echo boom >&2; exit 3", 5);
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "a non-zero exit is a runtime failure: {v}"
        );
        let d = v
            .get("runtime_digest")
            .expect("a runtime failure must carry a runtime_digest");
        assert_eq!(
            d["exit_code"],
            serde_json::json!(3),
            "the exit code must be refluxed: {v}"
        );
        assert!(
            d["stderr_tail"]
                .as_str()
                .is_some_and(|s| s.contains("boom")),
            "the stderr tail must carry the diagnostic beyond the boolean: {v}"
        );
    }

    /// Fail-soft: an absent / unstartable target must NOT panic — it must return
    /// a runtime-failure verdict. Neuter oracle: an `unwrap`/`?` on the child's
    /// exit path would panic here instead of yielding a verdict.
    #[test]
    fn launch_absent_target_fails_soft_without_panic() {
        let v = launch_and_reflux("this_binary_does_not_exist_zzq --nope", 5);
        assert_eq!(v["kind"], serde_json::json!("runtime"));
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "an unstartable target must fail soft to a failure: {v}"
        );
        assert!(
            v.get("runtime_digest").is_some(),
            "a fail-soft verdict still carries a digest: {v}"
        );
    }

    /// Fail-soft: a long-running command hit with a short timeout must be killed
    /// and reported as a timeout WITHOUT panicking (and the test finishes in ~1s,
    /// not ~5s). Neuter oracle: a plain `child.wait()` (no timeout/kill) would
    /// block for the full sleep and return exit 0, so `passed:false` / the
    /// `note == "timeout"` assert FAILs (and the test no longer finishes fast).
    #[test]
    fn launch_timeout_fails_soft_with_note() {
        let v = launch_and_reflux("sleep 5", 1);
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "a timed-out launch must fail soft: {v}"
        );
        assert_eq!(
            v["note"],
            serde_json::json!("timeout"),
            "the timeout note must be set: {v}"
        );
        let d = v
            .get("runtime_digest")
            .expect("a timeout must carry a digest");
        assert_eq!(
            d["exit_code"],
            serde_json::Value::Null,
            "no exit code is known on a timeout: {v}"
        );
    }

    /// A benign command that exits 0 cleanly must pass, and the passing shape
    /// omits the digest (mirroring the pure `runtime_reflux_verdict` pass shape).
    #[test]
    fn launch_benign_command_passes() {
        let v = launch_and_reflux("echo ok", 5);
        assert_eq!(
            v["passed"],
            serde_json::json!(true),
            "a clean exit-0 command must pass: {v}"
        );
        assert!(
            v.get("runtime_digest").is_none(),
            "the passing shape must omit the runtime_digest: {v}"
        );
    }

    /// Japanese behavioral markers are recognised too.
    #[test]
    fn japanese_behavioral_marker_blocks_skip() {
        let dc = "リトライの振る舞いを実装する。`cargo test -p condukt` が通ること";
        let c = classify_criteria(dc);
        assert!(c.behavioral);
        assert!(!c.skip_eligible);
    }

    /// Runtime / health criteria demand a running check, so even when they embed
    /// a passing test command they must NOT skip the verifier. Covers the English
    /// (`runtime`, `health`) and Japanese (`実行時`, `起動`, `稼働`) markers added
    /// for the phase-3 runtime-verification path.
    #[test]
    fn runtime_health_markers_block_skip() {
        for dc in [
            "the server starts and GET /health returns 200; `cargo test -p condukt` passes",
            "no runtime panic under load; `npm test` exits 0",
            "サーバを起動し /health が 200 を返すこと。`cargo test -p condukt` が通る",
            "実行時に例外を出さないこと。`pytest` が通る",
            "本番相当で稼働し続けること。`go test` が通る",
        ] {
            let c = classify_criteria(dc);
            assert!(
                c.behavioral,
                "runtime/health criteria must be behavioral: {dc}"
            );
            assert!(
                !c.skip_eligible,
                "runtime/health criteria must never skip the verifier even with an embedded command: {dc}"
            );
        }
    }

    // ── Health probe with server launch (health-url 付き起動経路) ──────────

    /// (a) Health check succeeds with HTTP 200 from a listening stub.
    /// A TcpListener in an ephemeral port listens for exactly one connection,
    /// responds with HTTP/1.1 200 OK, and the probe returns a pass verdict.
    #[test]
    fn health_probe_200_returns_pass() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        // Start a stub listener on an ephemeral port.
        let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind listener");
        let addr = listener.local_addr().expect("failed to get local addr");
        let port = addr.port();

        // Spawn a thread that accepts one connection and responds with 200 OK.
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Read incoming HTTP request (we don't care about contents).
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                // Send HTTP 200 response.
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
            }
        });

        // Probe the listener with a dummy server command.
        // Use tail -f /dev/null which keeps the process alive without doing anything.
        let health_url = format!("http://127.0.0.1:{}/health", port);
        let v = launch_server_and_probe("tail -f /dev/null", &health_url, 3);

        // The verdict callback should have killed the process, so just verify the result.
        let _ = handle.join();

        // Verify the verdict is a pass.
        assert_eq!(
            v["passed"],
            serde_json::json!(true),
            "health check 200 must result in passed=true: {v}"
        );
    }

    /// (b) Health check fails due to unreachable port (timeout).
    /// Probing a port where nobody is listening should timeout and return fail-soft.
    #[test]
    fn health_probe_timeout_returns_fail_soft() {
        // Pick an unpopulated ephemeral port that no service is listening on.
        // Port 9 (Discard Protocol) typically has no real listener on localhost.
        let health_url = "http://127.0.0.1:9/health";
        let v = launch_server_and_probe("tail -f /dev/null", health_url, 1);

        // Verify fail-soft verdict.
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "unreachable port should result in passed=false: {v}"
        );
        assert_eq!(
            v["note"],
            serde_json::json!("health-timeout"),
            "unreachable port should have note='health-timeout': {v}"
        );
        assert!(
            v.get("runtime_digest").is_some(),
            "fail-soft verdict must include runtime_digest: {v}"
        );
    }

    /// (c) Verify that health-url-less path (launch_and_reflux) still works.
    /// The existing launch_benign_command_passes test should not break.
    #[test]
    fn launch_and_reflux_still_passes_benign_command() {
        let v = launch_and_reflux("echo ok", 5);
        assert_eq!(
            v["passed"],
            serde_json::json!(true),
            "launch_and_reflux for benign command must still pass: {v}"
        );
        assert!(
            v.get("runtime_digest").is_none(),
            "passing verdict must omit runtime_digest: {v}"
        );
    }

    /// (d) Blastguard Deny prevents spawn in health path.
    /// A destructive command (rm -rf) should be refused by blastguard before
    /// spawn, so no process is created and the sentinel file is never created.
    #[test]
    fn health_probe_blastguard_deny_prevents_spawn() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("health_ran.txt");
        let payload = format!("touch {} ; rm -rf /nonexistent", sentinel.display());

        // Use a dummy health URL (won't be reached because spawn is blocked).
        let v = launch_server_and_probe(&payload, "http://127.0.0.1:9/health", 1);

        // Verify blastguard denial.
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "blastguard Deny must result in passed=false: {v}"
        );
        assert_eq!(
            v["note"],
            serde_json::json!("blastguard-denied"),
            "blastguard Deny should have note='blastguard-denied': {v}"
        );
        assert!(
            !sentinel.exists(),
            "sh -c must NOT have run (blastguard must block before spawn): {v}"
        );
    }

    /// (e) Bad URL format returns health-bad-url fail-soft.
    #[test]
    fn health_probe_bad_url_fails_soft() {
        let v = launch_server_and_probe("tail -f /dev/null", "not-a-url", 1);

        // Verify fail-soft verdict.
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "bad URL should result in passed=false: {v}"
        );
        assert_eq!(
            v["note"],
            serde_json::json!("health-bad-url"),
            "bad URL should have note='health-bad-url': {v}"
        );
    }

    // ── Docker-isolated exec backend (phase-3 container launcher) ─────────

    /// (a) UNIT: `docker_run_args` builds the exact expected argv for a sample
    /// (cmd, image, workdir) — pure assert, no spawn.
    #[test]
    fn docker_run_args_builds_expected_argv() {
        // No limits ⇒ byte-identical to the legacy network-only argv.
        let args = docker_run_args(
            "echo hi",
            "alpine:latest",
            "/work/dir",
            &SandboxLimits::default(),
        );
        assert_eq!(
            args,
            vec![
                "run".to_string(),
                "--rm".to_string(),
                "--network=none".to_string(),
                "-v".to_string(),
                "/work/dir:/work/dir".to_string(),
                "-w".to_string(),
                "/work/dir".to_string(),
                "alpine:latest".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string(),
            ],
            "docker_run_args must build the exact expected argv: {args:?}"
        );
    }

    /// (a2) UNIT: an empty `SandboxLimits` is reported empty and yields argv
    /// identical to the no-limits legacy form (backward-compat invariant).
    #[test]
    fn docker_run_args_default_limits_match_legacy() {
        let lim = SandboxLimits::default();
        let with = docker_run_args("x", "img", "/w", &lim);
        // Same as a hand-built legacy argv (no hardening flags anywhere).
        assert!(!with.iter().any(|a| a == "--memory"
            || a == "--cpus"
            || a == "--pids-limit"
            || a == "--read-only"));
        assert_eq!(with.first().map(String::as_str), Some("run"));
        assert_eq!(with.get(2).map(String::as_str), Some("--network=none"));
    }

    /// (a3) UNIT: `--memory` only is inserted between `--network=none` and `-v`.
    #[test]
    fn docker_run_args_memory_only() {
        let lim = SandboxLimits {
            memory: Some("512m".to_string()),
            ..Default::default()
        };
        let args = docker_run_args("cargo test", "rust:1", "/w", &lim);
        let mi = args
            .iter()
            .position(|a| a == "--memory")
            .expect("--memory present");
        assert_eq!(args[mi + 1], "512m");
        // still network-isolated, mount preserved
        assert!(args.contains(&"--network=none".to_string()));
        assert!(args.contains(&"/w:/w".to_string()));
        assert!(!args
            .iter()
            .any(|a| a == "--cpus" || a == "--pids-limit" || a == "--read-only"));
    }

    /// (a4) UNIT: `--cpus` only.
    #[test]
    fn docker_run_args_cpus_only() {
        let lim = SandboxLimits {
            cpus: Some("1.5".to_string()),
            ..Default::default()
        };
        let args = docker_run_args("x", "img", "/w", &lim);
        let ci = args
            .iter()
            .position(|a| a == "--cpus")
            .expect("--cpus present");
        assert_eq!(args[ci + 1], "1.5");
        assert!(!args.iter().any(|a| a == "--memory" || a == "--pids-limit"));
    }

    /// (a5) UNIT: `--pids-limit` only (numeric rendered as a string).
    #[test]
    fn docker_run_args_pids_only() {
        let lim = SandboxLimits {
            pids_limit: Some(256),
            ..Default::default()
        };
        let args = docker_run_args("x", "img", "/w", &lim);
        let pi = args
            .iter()
            .position(|a| a == "--pids-limit")
            .expect("--pids-limit present");
        assert_eq!(args[pi + 1], "256");
    }

    /// (a6) UNIT: `--read-only` flag with no value; mount stays read-write.
    #[test]
    fn docker_run_args_read_only_flag() {
        let lim = SandboxLimits {
            read_only: true,
            ..Default::default()
        };
        let args = docker_run_args("x", "img", "/w", &lim);
        assert!(args.contains(&"--read-only".to_string()));
        // the workdir bind mount is still present and rw (no :ro suffix)
        assert!(args.contains(&"/w:/w".to_string()));
    }

    /// (a7) UNIT: all limits together, in the documented order between
    /// `--network=none` and `-v`.
    #[test]
    fn docker_run_args_all_limits_ordered() {
        let lim = SandboxLimits {
            memory: Some("256m".to_string()),
            cpus: Some("2".to_string()),
            pids_limit: Some(128),
            read_only: true,
        };
        let args = docker_run_args("run it", "alpine", "/w", &lim);
        let net = args.iter().position(|a| a == "--network=none").unwrap();
        let vmount = args.iter().position(|a| a == "-v").unwrap();
        for flag in ["--read-only", "--memory", "--cpus", "--pids-limit"] {
            let idx = args
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{flag} present"));
            assert!(
                idx > net && idx < vmount,
                "{flag} must sit between --network=none and -v"
            );
        }
        // command still last three tokens
        assert_eq!(
            &args[args.len() - 3..],
            &["sh".to_string(), "-c".to_string(), "run it".to_string()]
        );
    }

    /// (b) FAIL-SOFT: a blastguard-flagged command must be refused BEFORE even
    /// probing docker availability — the benign leading segment must never
    /// execute (proven by the surviving-absent sentinel), mirroring
    /// `launch_refuses_destructive_command_without_spawning` for the host path.
    #[test]
    fn launch_in_container_refuses_destructive_command_without_spawning() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("ran.txt");
        let victim = tmp.path().join("victim");
        let payload = format!("touch {} ; rm -rf {}", sentinel.display(), victim.display());
        let workdir = tmp.path().display().to_string();
        let v = launch_in_container(
            &payload,
            5,
            "alpine:latest",
            &workdir,
            &SandboxLimits::default(),
        );
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "a refused command must not count as passed: {v}"
        );
        assert_eq!(v["note"], serde_json::json!("blastguard-denied"));
        assert!(
            !sentinel.exists(),
            "the payload must NOT have run — sentinel would prove it executed"
        );
    }

    /// (b) FAIL-SOFT: when docker is unavailable (binary missing, daemon down,
    /// or permission denied — the case in this worker's env), `launch_in_container`
    /// must return the `docker_unavailable` fail-soft verdict WITHOUT running the
    /// command, rather than panicking or silently falling back to the host shell.
    #[test]
    fn launch_in_container_fails_soft_when_docker_unavailable() {
        if docker_available() {
            // Nothing to assert here in an environment where docker really is
            // reachable — the availability-gated test below covers that path.
            return;
        }
        let v = launch_in_container(
            "echo hi",
            5,
            "alpine:latest",
            "/tmp",
            &SandboxLimits::default(),
        );
        assert_eq!(
            v["passed"],
            serde_json::json!(false),
            "docker-unavailable must fail soft: {v}"
        );
        assert_eq!(
            v["note"],
            serde_json::json!("docker_unavailable"),
            "docker-unavailable must carry the docker_unavailable note: {v}"
        );
    }

    /// (c) AVAILABILITY-GATED INTEGRATION: exercises the real container path
    /// when docker is reachable, else falls back to asserting the fail-soft
    /// verdict — so plain `cargo test` is GREEN whether or not docker is
    /// reachable in the host running this test.
    #[test]
    fn launch_in_container_runs_real_container_when_available() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path().display().to_string();
        if docker_available() {
            let v = launch_in_container(
                "true",
                30,
                "alpine:latest",
                &workdir,
                &SandboxLimits::default(),
            );
            assert_eq!(
                v["passed"],
                serde_json::json!(true),
                "a successful container run must pass: {v}"
            );

            let v2 = launch_in_container(
                "exit 3",
                30,
                "alpine:latest",
                &workdir,
                &SandboxLimits::default(),
            );
            assert_eq!(
                v2["passed"],
                serde_json::json!(false),
                "a non-zero container exit must fail: {v2}"
            );
            let d = v2
                .get("runtime_digest")
                .expect("a runtime failure must carry a runtime_digest");
            assert_eq!(
                d["exit_code"],
                serde_json::json!(3),
                "the container exit code must be refluxed: {v2}"
            );
        } else {
            let v = launch_in_container(
                "true",
                5,
                "alpine:latest",
                &workdir,
                &SandboxLimits::default(),
            );
            assert_eq!(
                v["passed"],
                serde_json::json!(false),
                "without docker reachable, this must fail soft: {v}"
            );
            assert_eq!(
                v["note"],
                serde_json::json!("docker_unavailable"),
                "without docker reachable, the note must be docker_unavailable: {v}"
            );
        }
    }
}

/// Spy-driven unit tests for the deterministic RUN-POLICY gate. The injected
/// launcher records whether it was called (via an `AtomicBool`), so these prove
/// the EscalateDocker-only routing WITHOUT any real docker.
#[cfg(test)]
mod run_policy_gate_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Run the gate with a spy launcher; returns `(verdict_json, launcher_called)`.
    fn spy_gate(cv: &str, dv: &str, cr: &str) -> (serde_json::Value, bool) {
        let called = AtomicBool::new(false);
        let v = run_policy_gate(cv, dv, cr, || {
            called.store(true, Ordering::SeqCst);
            serde_json::json!({ "kind": "runtime", "passed": true, "note": "spy-launch" })
        });
        (v, called.load(Ordering::SeqCst))
    }

    /// EscalateDocker (cheap_verify=fail, divergence=high) invokes the launcher
    /// and marks `container_launched:true` with the nested launch verdict.
    #[test]
    fn escalate_docker_invokes_container_launcher() {
        let (v, called) = spy_gate("fail", "high", "low");
        assert!(called, "EscalateDocker MUST invoke the container launcher");
        assert_eq!(v["verdict"], serde_json::json!("escalate_docker"));
        assert_eq!(v["container_launched"], serde_json::json!(true));
        assert_eq!(
            v["launch"]["note"],
            serde_json::json!("spy-launch"),
            "the injected launcher's verdict must be embedded: {v}"
        );
    }

    /// The second EscalateDocker path (cheap_verify=pass, divergence=high) also
    /// invokes the launcher.
    #[test]
    fn escalate_docker_pass_high_also_invokes_launcher() {
        let (v, called) = spy_gate("pass", "high", "low");
        assert!(called, "pass+high divergence -> EscalateDocker -> launch");
        assert_eq!(v["verdict"], serde_json::json!("escalate_docker"));
        assert_eq!(v["container_launched"], serde_json::json!(true));
    }

    /// VerifyOnly (pass/low/medium) does NOT invoke the launcher.
    #[test]
    fn verify_only_does_not_invoke_launcher() {
        let (v, called) = spy_gate("pass", "low", "medium");
        assert!(!called, "VerifyOnly must NOT invoke the launcher");
        assert_eq!(v["verdict"], serde_json::json!("verify_only"));
        assert_eq!(v["container_launched"], serde_json::json!(false));
        assert!(
            v.get("launch").is_none(),
            "no container path -> no nested launch verdict: {v}"
        );
    }

    /// EscalateShip (pass/low/low) does NOT invoke the launcher.
    #[test]
    fn escalate_ship_does_not_invoke_launcher() {
        let (v, called) = spy_gate("pass", "low", "low");
        assert!(!called, "EscalateShip must NOT invoke the launcher");
        assert_eq!(v["verdict"], serde_json::json!("escalate_ship"));
        assert_eq!(v["container_launched"], serde_json::json!(false));
    }

    /// AskHuman (fail/low/low) does NOT invoke the launcher.
    #[test]
    fn ask_human_does_not_invoke_launcher() {
        let (v, called) = spy_gate("fail", "low", "low");
        assert!(!called, "AskHuman must NOT invoke the launcher");
        assert_eq!(v["verdict"], serde_json::json!("ask_human"));
        assert_eq!(v["container_launched"], serde_json::json!(false));
    }

    /// Garbage inputs fail-soft (never panic) and, per the matrix, an
    /// unrecognized cheap_verify -> AskHuman -> launcher NOT called.
    #[test]
    fn garbage_inputs_fail_soft_without_launch() {
        let (v, called) = spy_gate("bogus", "\u{0}\t", "???");
        assert!(!called, "unknown cheap_verify -> AskHuman -> no launch");
        assert_eq!(v["verdict"], serde_json::json!("ask_human"));
        assert_eq!(v["container_launched"], serde_json::json!(false));
    }

    // ── declared deterministic checks ────────────────────────────────────

    use crate::model::Check;

    fn check(cmd: &str, expect_exit: Option<i64>, expect_substring: Option<&str>) -> Check {
        Check {
            cmd: cmd.to_string(),
            expect_exit,
            expect_substring: expect_substring.map(str::to_string),
        }
    }

    /// `check_passed` truth table: exit-only matching against the default (0)
    /// and an explicit non-zero expectation.
    #[test]
    fn check_passed_exit_only() {
        // default expect_exit = 0
        assert!(check_passed(&check("x", None, None), 0, ""));
        assert!(!check_passed(&check("x", None, None), 1, ""));
        // explicit non-zero expectation
        assert!(check_passed(&check("x", Some(2), None), 2, ""));
        assert!(!check_passed(&check("x", Some(2), None), 0, ""));
    }

    /// `check_passed` substring condition (in addition to a matching exit).
    #[test]
    fn check_passed_substring() {
        assert!(check_passed(
            &check("x", None, Some("ok")),
            0,
            "all ok here"
        ));
        assert!(!check_passed(&check("x", None, Some("ok")), 0, "nope"));
        // both conditions: exit matches but substring absent -> fail
        assert!(!check_passed(
            &check("x", Some(0), Some("ok")),
            0,
            "missing"
        ));
        // both conditions satisfied -> pass
        assert!(check_passed(&check("x", Some(0), Some("ok")), 0, "ok!"));
    }

    /// `checks_verdict` aggregate: all-true -> true, any-false -> false,
    /// empty -> vacuous true.
    #[test]
    fn checks_verdict_aggregate() {
        assert!(checks_verdict(&[true, true, true]));
        assert!(!checks_verdict(&[true, false, true]));
        assert!(checks_verdict(&[]));
    }

    /// Executor smoke tests against trivial coreutils commands.
    #[test]
    fn run_check_executor_smoke() {
        assert!(run_check(&check("true", None, None), None).passed);
        assert!(!run_check(&check("false", None, None), None).passed);
        assert!(run_check(&check("echo hello", None, Some("hello")), None).passed);
        assert!(!run_check(&check("echo hello", None, Some("nope")), None).passed);
    }

    /// `run_checks` aggregates and reports each check in declaration order.
    #[test]
    fn run_checks_aggregates_report() {
        let report = run_checks(
            &[check("true", None, None), check("false", None, None)],
            None,
        );
        assert!(!report.all_passed);
        assert_eq!(report.results.len(), 2);
        assert!(report.results[0].passed);
        assert!(!report.results[1].passed);

        let all_ok = run_checks(&[check("true", None, None)], None);
        assert!(all_ok.all_passed);
    }
}
