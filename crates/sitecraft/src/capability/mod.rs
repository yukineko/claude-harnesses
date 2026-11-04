//! Capabilities beyond the web app (rulings D2–D5): `sitecraft capability
//! probe <name>` (D3) and `sitecraft check capability <name>` (D4, with the
//! D5 result record). The schema lives in [`crate::decide::capability`]; the
//! evidence rules in [`pixels`]; the input hash, result records and the Stop
//! gate's freshness check in [`results`].
//!
//! # Probe (D3) — Ask material, never completion evidence (U2)
//!
//! Prints exactly one JSON line `{"capability","result","reason"}` with
//! `result` = `feasible` (exit 0) / `infeasible` (1) / `undetermined` (2).
//! Time limit [`PROBE_LIMIT`] → `undetermined`, reason `timeout`. Writes
//! neither the result records nor decisions.toml; the Blender probe's
//! scratch files live under `<state>/probe-tmp/` and are removed afterwards.
//!
//! # Check (D4)
//!
//! Exit 0 clean / 1 violation / 2 undetermined, time limit [`CHECK_LIMIT`]
//! (reason `timeout`). The input hash is computed before the run and again
//! after it; inputs that changed during the run are exit 2 (the run's
//! evidence would not pin them). Every run of a registered capability writes
//! its result record; a record that cannot be written makes the run exit 2.
//!
//! The `builtin:threejs-render` page is opened on the address the dev server
//! answered on (`serve start`'s recorded `url`): `http://127.0.0.1:<port>`,
//! or `http://[::1]:<port>` for a server that listens on the IPv6 loopback
//! only.

pub mod pixels;
pub mod results;

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::check::pages;
use crate::config::{self, Loaded};
use crate::decide::capability::{self, Capability, Verify};
use crate::decide::record;
use crate::serve;
use crate::trust::{self, Trust};
use crate::{Determination, Verdict, ENV_BLENDER, EXIT_UNDETERMINED};
use pixels::Pixels;
use results::WebInputs;

/// Time limit of one probe (rulings D3 / D11).
pub const PROBE_LIMIT: Duration = Duration::from_secs(120);
/// Time limit of one `check capability` (rulings D4 / D11).
pub const CHECK_LIMIT: Duration = Duration::from_secs(300);
/// Where Blender is looked for after `$SITECRAFT_BLENDER` and `PATH`.
pub const MAC_BLENDER: &str = "/Applications/Blender.app/Contents/MacOS/Blender";
/// The bundled three.js runner (probe and check).
pub const THREEJS_SCRIPT: &str = "capability-threejs.mjs";
/// Schema tag the three.js runner emits.
pub const THREEJS_SCHEMA: &str = "sitecraft-capability-threejs/1";
/// Sub-directory of the state dir for the Blender probe's scratch files.
pub const PROBE_TMP: &str = "probe-tmp";

const FIX_PLAYWRIGHT: &str = "install Playwright in the web-app package (the `npm --prefix` directory of `command`, else the project root): `npm i -D playwright && npx playwright install chromium`";

// ---------------------------------------------------------------------------
// shared
// ---------------------------------------------------------------------------

/// An absolute point in time every subprocess of one run must finish by.
#[derive(Clone, Copy)]
struct Deadline(Instant);

impl Deadline {
    fn after(d: Duration) -> Self {
        Deadline(Instant::now() + d)
    }
    fn remaining(self) -> Duration {
        self.0.saturating_duration_since(Instant::now())
    }
}

/// Last few non-empty lines of a captured stream, as a reason suffix.
fn tail(label: &str, bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return String::new();
    }
    let start = lines.len().saturating_sub(8);
    format!("\n{label}:\n  {}", lines[start..].join("\n  "))
}

/// Where Blender was found.
enum Blender {
    Found(PathBuf),
    /// Not on any of the three places (probe: infeasible).
    Absent(String),
    /// `$SITECRAFT_BLENDER` names something unusable (probe: undetermined).
    Unusable(String),
}

/// The D3 lookup: `$SITECRAFT_BLENDER` when non-empty (only that), else
/// `blender` on `PATH`, else [`MAC_BLENDER`].
fn find_blender() -> Blender {
    if let Some(v) = std::env::var_os(ENV_BLENDER).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(&v);
        return if results::is_executable_file(&p) {
            Blender::Found(p)
        } else {
            Blender::Unusable(format!(
                "{ENV_BLENDER}={} is not an executable file; point it at the Blender executable or unset it",
                p.display()
            ))
        };
    }
    if let Some(p) = on_path("blender") {
        return Blender::Found(p);
    }
    let mac = PathBuf::from(MAC_BLENDER);
    if results::is_executable_file(&mac) {
        return Blender::Found(mac);
    }
    Blender::Absent(format!(
        "no Blender found ({ENV_BLENDER} unset, no `blender` on PATH, no {MAC_BLENDER}); install Blender or set {ENV_BLENDER}"
    ))
}

/// `prog` resolved on `PATH` (an executable regular file), if any.
fn on_path(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(prog))
        .find(|p| results::is_executable_file(p))
}

/// `argv[0]` of a `command` capability as spawned: containing `/` → that
/// path (root-relative unless absolute); else the bare name (PATH lookup).
fn program_path(root: &Path, argv0: &str) -> PathBuf {
    if argv0.contains('/') {
        let p = Path::new(argv0);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    } else {
        PathBuf::from(argv0)
    }
}

fn piped(cmd: &mut Command) {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, so a timeout kills everything the run started.
        .process_group(0);
}

/// Load the record and find `name`: `Err` = exit 2 reason.
fn lookup(root: &Path, name: &str) -> Result<(record::Decisions, toml::Value, Capability), String> {
    let d = match record::load(root) {
        Determination::Known(Some(d)) => d,
        Determination::Known(None) => {
            return Err(format!(
                "{} does not exist, so capability `{name}` is not registered; register it with `sitecraft decide capability --name {name} …`",
                record::path_for(root).display()
            ))
        }
        Determination::Undetermined(why) => return Err(why.as_str().to_string()),
    };
    let Some(raw) = d.capabilities.get(name).cloned() else {
        let known: Vec<&String> = d.capabilities.keys().collect();
        return Err(format!(
            "capability `{name}` is not registered in {} (registered: {known:?}); register it with `sitecraft decide capability --name {name} …`",
            record::path_for(root).display()
        ));
    };
    let cap = capability::parse(name, &raw)?;
    Ok((d, raw, cap))
}

/// Outcome of the bundled three.js runner.
enum ThreeOut {
    /// Probe, status ok: the WebGL context kind (`None` = neither).
    Webgl(Option<String>),
    /// Check, status ok: the screenshot PNG.
    Png(Vec<u8>),
    /// Check: the selector never became visible (violation).
    NotVisible(String),
    /// Playwright or Chromium could not start, a timeout, or any other
    /// failure (undetermined). `timeout` is `true` for the time limit.
    Undetermined { why: String, timeout: bool },
}

/// Run the bundled three.js runner with `input` in `cwd` within `limit`.
fn run_three(cwd: &Path, input: &Value, limit: Duration) -> ThreeOut {
    let undet = |why: String| ThreeOut::Undetermined {
        why,
        timeout: false,
    };
    let script = match pages::locate_asset(THREEJS_SCRIPT) {
        Ok(s) => s,
        Err(e) => return undet(e),
    };
    let node = crate::program_from_env(crate::ENV_NODE, "node");
    let mut cmd = Command::new(&node);
    cmd.arg(&script).arg(input.to_string()).current_dir(cwd);
    piped(&mut cmd);
    let out = match pages::run_with_deadline(cmd, limit) {
        Ok(o) => o,
        Err(e) => {
            return undet(format!(
                "cannot run node ({}): {e}; install Node.js or set {} to the node executable",
                node.to_string_lossy(),
                crate::ENV_NODE
            ))
        }
    };
    let stderr = tail("node stderr", &out.stderr);
    let Some(status) = out.status else {
        return ThreeOut::Undetermined {
            why: format!(
                "timeout: the three.js runner did not finish within {}s and was killed{stderr}",
                limit.as_secs()
            ),
            timeout: true,
        };
    };
    let v: Value = match std::str::from_utf8(&out.stdout)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(t.trim()).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => {
            return undet(format!(
                "the three.js runner ({status}) printed no usable JSON ({e}){stderr}"
            ))
        }
    };
    if v.get("schema").and_then(Value::as_str) != Some(THREEJS_SCHEMA) {
        return undet(format!(
            "the three.js runner output is not {THREEJS_SCHEMA}; reinstall the sitecraft plugin{stderr}"
        ));
    }
    let err = v
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("no error text")
        .to_string();
    let st = v.get("status").and_then(Value::as_str).unwrap_or("");
    let concluded = status.success();
    match st {
        "ok" if concluded => {
            if let Some(b64) = v.get("png_base64").and_then(Value::as_str) {
                return match base64_decode(b64) {
                    Ok(png) => ThreeOut::Png(png),
                    Err(e) => undet(format!("the screenshot is not valid base64: {e}")),
                };
            }
            match v.get("webgl") {
                Some(Value::String(k)) if k == "webgl2" || k == "webgl" => {
                    ThreeOut::Webgl(Some(k.clone()))
                }
                Some(Value::Null) => ThreeOut::Webgl(None),
                _ => undet(format!(
                    "the three.js runner said ok without a screenshot or a webgl answer: {v}"
                )),
            }
        }
        "not-visible" if concluded => ThreeOut::NotVisible(err),
        "playwright-missing" => undet(format!("Playwright cannot be loaded: {err}; {FIX_PLAYWRIGHT}{stderr}")),
        "launch-failed" => undet(format!(
            "Chromium cannot be launched: {err}; install it from the web-app package: `npx playwright install chromium`{stderr}"
        )),
        "error" if err == "timeout" => {
            crate::history::note_timeout();
            ThreeOut::Undetermined {
                why: format!("timeout: the three.js runner hit its time limit{stderr}"),
                timeout: true,
            }
        }
        other => undet(format!(
            "the three.js runner ended with status {other:?} ({status}): {err}{stderr}"
        )),
    }
}

/// Standard base64 (with or without padding) → bytes.
fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let data: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let data = match data.iter().position(|b| *b == b'=') {
        Some(i) => {
            if data[i..].iter().any(|b| *b != b'=') || data.len() % 4 != 0 {
                return Err("misplaced padding".to_string());
            }
            &data[..i]
        }
        None => &data[..],
    };
    if data.len() % 4 == 1 {
        return Err("truncated input".to_string());
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    for chunk in data.chunks(4) {
        let mut acc = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = val(*c).ok_or_else(|| format!("invalid character {:?}", char::from(*c)))?;
            acc |= v << (18 - 6 * i);
        }
        let bytes = acc.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Ok(out)
}

/// The directory node runs in for `root`: the `npm --prefix` package dir of
/// a loadable `sitecraft.toml`, else the root (no config). An unusable
/// config is `Err`.
fn node_dir_for(root: &Path) -> Result<PathBuf, String> {
    match config::load(root) {
        Loaded::Absent => Ok(root.to_path_buf()),
        Loaded::Config(cfg, _) => pages::package_dir(&cfg.command, root),
        Loaded::Undetermined(why) => Err(why),
    }
}

// ---------------------------------------------------------------------------
// probe (D3)
// ---------------------------------------------------------------------------

/// A probe answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// It worked on this machine (exit 0). Not completion evidence (U2).
    Feasible(String),
    /// It ran to a conclusion and does not work here (exit 1).
    Infeasible(String),
    /// Could not tell (exit 2). `"timeout"` for the time limit.
    Undetermined(String),
}

impl Probe {
    fn parts(&self) -> (&'static str, &str, i32) {
        match self {
            Probe::Feasible(r) => ("feasible", r, 0),
            Probe::Infeasible(r) => ("infeasible", r, 1),
            Probe::Undetermined(r) => ("undetermined", r, EXIT_UNDETERMINED),
        }
    }
}

/// `sitecraft capability probe <name>`: print the one JSON line and return
/// the exit code (module docs).
pub fn cmd_probe(root: &Path, name: &str) -> i32 {
    let p = probe(root, name);
    let (result, reason, code) = p.parts();
    println!(
        "{}",
        serde_json::json!({ "capability": name, "result": result, "reason": reason })
    );
    if code != 0 {
        eprintln!("sitecraft capability probe {name}: {result} - {reason}");
    }
    code
}

/// The probe of `name` (no output).
#[must_use]
pub fn probe(root: &Path, name: &str) -> Probe {
    let deadline = Deadline::after(PROBE_LIMIT);
    let (_, _, cap) = match lookup(root, name) {
        Ok(x) => x,
        Err(e) => return Probe::Undetermined(e),
    };
    let Some(verify) = cap.verify else {
        return Probe::Undetermined(format!(
            "capability `{name}` has no verify table; record it with `sitecraft decide capability`"
        ));
    };
    match verify {
        Verify::Blender { .. } => probe_blender(root, deadline),
        Verify::Threejs { .. } => probe_threejs(root, deadline),
        Verify::Command { argv, .. } => probe_command(root, &argv[0]),
    }
}

/// The minimal Blender script: export the default scene to `glb`.
fn probe_script(glb: &Path) -> String {
    let lit = serde_json::Value::String(glb.to_string_lossy().into_owned()).to_string();
    format!(
        "# sitecraft capability probe: export the factory-default scene as a GLB.\nimport bpy\nbpy.ops.export_scene.gltf(filepath={lit}, export_format=\"GLB\")\n"
    )
}

fn probe_blender(root: &Path, deadline: Deadline) -> Probe {
    let blender = match find_blender() {
        Blender::Found(p) => p,
        Blender::Absent(why) => return Probe::Infeasible(why),
        Blender::Unusable(why) => return Probe::Undetermined(why),
    };
    let state = match crate::state_dir() {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => return Probe::Undetermined(why.as_str().to_string()),
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = state
        .join(PROBE_TMP)
        .join(format!("{}-{nanos}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Probe::Undetermined(format!("cannot create {}: {e}", dir.display()));
    }
    let r = probe_blender_in(root, &blender, &dir, deadline);
    // Scratch files only; a failed cleanup is reported but does not change
    // the answer (the result records and decisions.toml are never written).
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        eprintln!(
            "sitecraft capability probe: could not remove {}: {e}",
            dir.display()
        );
    }
    // The shared parent too, unless another probe is using it (not empty).
    let parent = state.join(PROBE_TMP);
    match std::fs::remove_dir(&parent) {
        Ok(()) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(e) => eprintln!(
            "sitecraft capability probe: could not remove {}: {e}",
            parent.display()
        ),
    }
    r
}

fn probe_blender_in(root: &Path, blender: &Path, dir: &Path, deadline: Deadline) -> Probe {
    let glb = dir.join("probe.glb");
    let script = dir.join("probe.py");
    if let Err(e) = std::fs::write(&script, probe_script(&glb)) {
        return Probe::Undetermined(format!("cannot write {}: {e}", script.display()));
    }
    let mut cmd = Command::new(blender);
    cmd.arg("--background")
        .arg("--factory-startup")
        .arg("--python")
        .arg(&script);
    if root.is_dir() {
        cmd.current_dir(root);
    }
    piped(&mut cmd);
    let out = match pages::run_with_deadline(cmd, deadline.remaining()) {
        Ok(o) => o,
        Err(e) => return Probe::Undetermined(format!("cannot spawn {}: {e}", blender.display())),
    };
    let Some(status) = out.status else {
        return Probe::Undetermined("timeout".to_string());
    };
    if !status.success() {
        return Probe::Infeasible(format!(
            "{} ended with {status}{}",
            blender.display(),
            tail("blender stderr", &out.stderr)
        ));
    }
    match std::fs::read(&glb) {
        Ok(bytes) => match pixels::judge_glb(&bytes) {
            Ok(()) => Probe::Feasible(format!(
                "{} exported a valid GLB headless",
                blender.display()
            )),
            Err(why) => Probe::Infeasible(format!("the exported {}: {why}", glb.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Probe::Infeasible(format!(
            "{} exited 0 but wrote no GLB at {}",
            blender.display(),
            glb.display()
        )),
        Err(e) => Probe::Undetermined(format!("cannot read {}: {e}", glb.display())),
    }
}

fn probe_threejs(root: &Path, deadline: Deadline) -> Probe {
    let dir = match node_dir_for(root) {
        Ok(d) => d,
        Err(e) => return Probe::Undetermined(e),
    };
    let limit = deadline.remaining();
    let input = serde_json::json!({ "mode": "probe", "timeout_ms": millis(limit) });
    match run_three(&dir, &input, limit) {
        ThreeOut::Webgl(Some(k)) => {
            Probe::Feasible(format!("headless Chromium gives a {k} context"))
        }
        ThreeOut::Webgl(None) => Probe::Infeasible(
            "headless Chromium gives neither a webgl2 nor a webgl context".to_string(),
        ),
        ThreeOut::Undetermined { timeout: true, .. } => Probe::Undetermined("timeout".to_string()),
        ThreeOut::Undetermined { why, .. } => Probe::Undetermined(why),
        ThreeOut::Png(_) | ThreeOut::NotVisible(_) => {
            Probe::Undetermined("the three.js runner answered a check, not a probe".to_string())
        }
    }
}

fn probe_command(root: &Path, argv0: &str) -> Probe {
    if argv0.contains('/') {
        let p = program_path(root, argv0);
        match std::fs::metadata(&p) {
            Ok(_) if results::is_executable_file(&p) => {
                Probe::Feasible(format!("{} is an executable file", p.display()))
            }
            Ok(_) => Probe::Infeasible(format!("{} is not an executable file", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Probe::Infeasible(format!("{} does not exist", p.display()))
            }
            Err(e) => Probe::Undetermined(format!("cannot inspect {}: {e}", p.display())),
        }
    } else {
        match on_path(argv0) {
            Some(p) => Probe::Feasible(format!("`{argv0}` resolves to {} on PATH", p.display())),
            None => Probe::Infeasible(format!("`{argv0}` is not an executable on PATH")),
        }
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX).max(1)
}

// ---------------------------------------------------------------------------
// check capability (D4 / D5)
// ---------------------------------------------------------------------------

/// `sitecraft check capability <name>`: run, write the result record, print
/// the verdict, append the `capability:<name>` check-history line
/// ([`crate::history::recorded`]), return the exit code (module docs).
pub fn cmd_check(root: &Path, name: &str) -> i32 {
    let label = format!("sitecraft check capability {name}");
    let mut out = crate::history::Output::printing();
    let state = match crate::state_dir() {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => {
            // No state dir: neither the result record nor the history line
            // can be written.
            let v = why.into_verdict();
            out.report(&label, &v);
            return crate::exit_code(&v);
        }
    };
    let v = crate::history::recorded(
        Ok(&state),
        root,
        &format!("capability:{name}"),
        &label,
        &mut out,
        |o| check_and_record(&state, root, name, &label, o),
    );
    crate::exit_code(&v)
}

/// The body of [`cmd_check`]: look up, run, write the result record and
/// print the verdict into `out`. Returns the reported verdict (an unwritable
/// result record is `Undetermined`).
fn check_and_record(
    state: &Path,
    root: &Path,
    name: &str,
    label: &str,
    out: &mut crate::history::Output,
) -> Verdict {
    let (d, raw, cap) = match lookup(root, name) {
        Ok(x) => x,
        Err(e) => {
            let v = Verdict::undetermined(e);
            out.report(label, &v);
            return v;
        }
    };
    // A refused run has no input hash: its record stores `input_hash: null`,
    // which the Stop gate reads as a Violation (never as fresh).
    let (verdict, hash) = match check(state, root, name, &d, &raw, &cap) {
        Run::Refused(v) => (v, None),
        Run::Ran {
            verdict,
            input_hash,
        } => (verdict, Some(input_hash)),
    };
    let code = crate::exit_code(&verdict);
    out.report(label, &verdict);
    match results::write_record(state, root, name, code, hash.as_deref()) {
        Ok(_) => verdict,
        Err(e) => {
            let u = Verdict::undetermined(format!(
                "the result record cannot be written ({e}), so the Stop gate could never see this run; make the state dir writable and rerun"
            ));
            out.report(label, &u);
            u
        }
    }
}

/// Outcome of one `check capability` attempt.
#[derive(Debug)]
enum Run {
    /// Nothing ran (not relevant, no verify, tool missing, untrusted,
    /// unhashable inputs, …). The verdict is never `Clean`; there is no input
    /// hash, so the result record stores `null` (a Violation at the gate).
    Refused(Verdict),
    /// The verify ran; `input_hash` was taken before the run.
    Ran {
        /// The run's verdict (including an inputs-changed / re-hash failure).
        verdict: Verdict,
        /// The input hash taken before the run.
        input_hash: String,
    },
}

/// Run the D4 check of a registered capability.
fn check(
    state: &Path,
    root: &Path,
    name: &str,
    d: &record::Decisions,
    raw: &toml::Value,
    cap: &Capability,
) -> Run {
    let deadline = Deadline::after(CHECK_LIMIT);
    if !cap.is_relevant() {
        return Run::Refused(Verdict::undetermined(format!(
            "capability `{name}` is not judged relevant; only a relevant capability is checked (record it with `sitecraft decide capability --name {name} --relevance relevant …`)"
        )));
    }
    let Some(verify) = &cap.verify else {
        return Run::Refused(Verdict::undetermined(format!(
            "capability `{name}` has no verify table; record it with `sitecraft decide capability --name {name} … --verify-kind <kind>`"
        )));
    };
    match verify {
        Verify::Blender {
            script,
            glb,
            render,
            ..
        } => {
            let hash = |root: &Path| results::input_hash(root, name, raw, verify, None);
            check_blender(root, script, glb, render, deadline, &hash)
        }
        Verify::Command { argv, .. } => {
            let caps = match d.parsed_capabilities() {
                Ok(c) => c,
                Err(e) => return Run::Refused(Verdict::undetermined(e)),
            };
            let hash = |root: &Path| results::input_hash(root, name, raw, verify, None);
            check_command(state, root, argv, &caps, deadline, &hash)
        }
        Verify::Threejs { page, selector, .. } => {
            check_threejs(state, root, name, raw, verify, page, selector, deadline)
        }
    }
}

/// Run `body` between two input hashes: an unhashable input is exit 2
/// without running; inputs that changed during the run are exit 2.
fn hashed(
    root: &Path,
    hash: &dyn Fn(&Path) -> Result<String, String>,
    body: impl FnOnce() -> Verdict,
) -> Run {
    let before = match hash(root) {
        Ok(h) => h,
        Err(e) => {
            return Run::Refused(Verdict::undetermined(format!(
                "cannot compute the input hash: {e}"
            )))
        }
    };
    let v = body();
    let verdict = match hash(root) {
        Err(e) => Verdict::worst_of([
            v,
            Verdict::undetermined(format!(
                "cannot recompute the input hash after the run: {e}"
            )),
        ]),
        Ok(after) if after != before => Verdict::worst_of([
            v,
            Verdict::undetermined(
                "the capability's inputs changed during the run, so the run does not pin them; make sure the run does not write into its own inputs (script / verify.inputs / web_globs), then rerun",
            ),
        ]),
        Ok(_) => v,
    };
    Run::Ran {
        verdict,
        input_hash: before,
    }
}

/// Read an output file: missing → `Err(violation)`, unreadable → `Err(undetermined)`.
fn read_output(p: &Path, what: &str) -> Result<Vec<u8>, Verdict> {
    match std::fs::read(p) {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Verdict::violation(format!(
            "the {what} {} was not written",
            p.display()
        ))),
        Err(e) => Err(Verdict::undetermined(format!(
            "cannot read the {what} {}: {e}",
            p.display()
        ))),
    }
}

/// [`Verdict::worst_of`], except that a resulting `Violation` carries every
/// undetermined reason too.
fn combine(verdicts: Vec<Verdict>) -> Verdict {
    let undetermined: Vec<String> = verdicts
        .iter()
        .filter_map(|v| match v {
            Verdict::Undetermined(u) => Some(u.as_str().to_string()),
            _ => None,
        })
        .collect();
    match Verdict::worst_of(verdicts) {
        Verdict::Violation(r) if !undetermined.is_empty() => Verdict::violation(format!(
            "{}; additionally undetermined: {}",
            r.as_str(),
            undetermined.join("; ")
        )),
        other => other,
    }
}

fn judge_image(bytes: &[u8], what: &str) -> Verdict {
    match pixels::judge_png(bytes) {
        Pixels::Drawn { .. } => Verdict::from_findings(Vec::new()),
        Pixels::NotDrawn(why) => Verdict::violation(format!("the {what}: {why}")),
    }
}

fn remove_stale(p: &Path) -> Result<(), String> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot delete the old output {}: {e}", p.display())),
    }
}

fn check_blender(
    root: &Path,
    script: &str,
    glb: &str,
    render: &str,
    deadline: Deadline,
    hash: &dyn Fn(&Path) -> Result<String, String>,
) -> Run {
    let blender = match find_blender() {
        Blender::Found(p) => p,
        Blender::Absent(why) | Blender::Unusable(why) => {
            return Run::Refused(Verdict::undetermined(why))
        }
    };
    let script_path = root.join(script);
    if !script_path.is_file() {
        return Run::Refused(Verdict::undetermined(format!(
            "the capability script {} does not exist; write it (or fix verify.script)",
            script_path.display()
        )));
    }
    let canon_root = match std::fs::canonicalize(root) {
        Ok(c) => c,
        Err(e) => {
            return Run::Refused(Verdict::undetermined(format!(
                "cannot canonicalize {}: {e}",
                root.display()
            )))
        }
    };
    let glb_abs = canon_root.join(glb);
    let render_abs = canon_root.join(render);
    hashed(root, hash, || {
        for p in [&glb_abs, &render_abs] {
            if let Err(e) = remove_stale(p) {
                return Verdict::undetermined(e);
            }
            if let Some(parent) = p.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return Verdict::undetermined(format!(
                        "cannot create {}: {e}",
                        parent.display()
                    ));
                }
            }
        }
        let mut cmd = Command::new(&blender);
        cmd.arg("--background")
            .arg("--factory-startup")
            .arg("--python")
            .arg(canon_root.join(script))
            .arg("--")
            .arg("--glb")
            .arg(&glb_abs)
            .arg("--render")
            .arg(&render_abs)
            .current_dir(&canon_root);
        piped(&mut cmd);
        let out = match pages::run_with_deadline(cmd, deadline.remaining()) {
            Ok(o) => o,
            Err(e) => {
                return Verdict::undetermined(format!("cannot spawn {}: {e}", blender.display()))
            }
        };
        let Some(status) = out.status else {
            return Verdict::undetermined(format!(
                "timeout: blender did not finish within {}s and was killed",
                CHECK_LIMIT.as_secs()
            ));
        };
        if !status.success() {
            return Verdict::violation(format!(
                "{} ended with {status}{}",
                blender.display(),
                tail("blender stderr", &out.stderr)
            ));
        }
        let g = match read_output(&glb_abs, "GLB") {
            Ok(b) => match pixels::judge_glb(&b) {
                Ok(()) => Verdict::from_findings(Vec::new()),
                Err(why) => Verdict::violation(format!("the GLB {}: {why}", glb_abs.display())),
            },
            Err(v) => v,
        };
        let r = match read_output(&render_abs, "render") {
            Ok(b) => judge_image(&b, &format!("render {}", render_abs.display())),
            Err(v) => v,
        };
        combine(vec![g, r])
    })
}

fn check_command(
    state: &Path,
    root: &Path,
    argv: &[String],
    caps: &BTreeMap<String, Capability>,
    deadline: Deadline,
    hash: &dyn Fn(&Path) -> Result<String, String>,
) -> Run {
    let Some(digest) = capability::command_digest(caps) else {
        return Run::Refused(Verdict::undetermined(
            "no command capability found for the D2-trust digest",
        ));
    };
    match trust::check_capabilities(state, root, &digest) {
        Trust::Trusted => {}
        Trust::Untrusted(why) => {
            return Run::Refused(Verdict::undetermined(format!(
                "refusing to run the command: {why}"
            )))
        }
        Trust::Undetermined(why) => {
            return Run::Refused(Verdict::undetermined(format!(
                "cannot verify the D2-trust of the command (nothing was run): {why}; fix the trust store, then a human runs `sitecraft trust`"
            )))
        }
    }
    hashed(root, hash, || {
        let prog = program_path(root, &argv[0]);
        let mut cmd = Command::new(&prog);
        cmd.args(&argv[1..]).current_dir(root);
        piped(&mut cmd);
        let out = match pages::run_with_deadline(cmd, deadline.remaining()) {
            Ok(o) => o,
            Err(e) => {
                return Verdict::undetermined(format!(
                    "cannot spawn {}: {e}; fix verify.argv (then `sitecraft trust`)",
                    prog.display()
                ))
            }
        };
        let streams = format!(
            "{}{}",
            tail("stdout", &out.stdout),
            tail("stderr", &out.stderr)
        );
        match out.status {
            None => Verdict::undetermined(format!(
                "timeout: the command did not finish within {}s and was killed{streams}",
                CHECK_LIMIT.as_secs()
            )),
            Some(s) => match s.code() {
                Some(0) => Verdict::from_findings(Vec::new()),
                Some(2) => {
                    Verdict::undetermined(format!("the command exited 2 (undetermined){streams}"))
                }
                Some(c) => Verdict::violation(format!("the command exited {c}{streams}")),
                None => {
                    Verdict::violation(format!("the command was ended by a signal ({s}){streams}"))
                }
            },
        }
    })
}

/// Stops the dev server it guards when dropped (early return or panic).
struct StartedServer<'a> {
    state: &'a Path,
    root: &'a Path,
    armed: bool,
}

impl StartedServer<'_> {
    fn stop(mut self) -> Verdict {
        self.armed = false;
        match serve::stop(self.state, self.root) {
            Determination::Known(_) => Verdict::from_findings(Vec::new()),
            Determination::Undetermined(why) => Verdict::undetermined(format!(
                "the dev server this check started could not be stopped: {}",
                why.as_str()
            )),
        }
    }
}

impl Drop for StartedServer<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Determination::Undetermined(why) = serve::stop(self.state, self.root) {
                eprintln!(
                    "sitecraft check capability: the dev server could not be stopped: {}",
                    why.as_str()
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_threejs(
    state: &Path,
    root: &Path,
    name: &str,
    raw: &toml::Value,
    verify: &Verify,
    page: &str,
    selector: &str,
    deadline: Deadline,
) -> Run {
    let cfg_path = config::path_for(root);
    let (cfg, bytes) = match config::load(root) {
        Loaded::Config(c, b) => (c, b),
        Loaded::Absent => {
            return Run::Refused(Verdict::undetermined(format!(
                "builtin:threejs-render needs a trusted {} (none at {}); write it and have a human run `sitecraft trust`",
                config::FILE_NAME,
                root.display()
            )))
        }
        Loaded::Undetermined(why) => return Run::Refused(Verdict::undetermined(why)),
    };
    match trust::check_bytes(state, root, &cfg_path, &bytes) {
        Trust::Trusted => {}
        Trust::Untrusted(why) => {
            return Run::Refused(Verdict::undetermined(format!(
                "untrusted config, nothing was run: {why}"
            )))
        }
        Trust::Undetermined(why) => {
            return Run::Refused(Verdict::undetermined(format!(
                "cannot verify trust of {}: {why}",
                cfg_path.display()
            )))
        }
    }
    let web_globs = cfg.web_globs.clone();
    let hash = |root: &Path| {
        results::input_hash(
            root,
            name,
            raw,
            verify,
            Some(WebInputs {
                toml_bytes: &bytes,
                web_globs: &web_globs,
            }),
        )
    };
    let node_dir = match pages::package_dir(&cfg.command, root) {
        Ok(d) => d,
        Err(e) => return Run::Refused(Verdict::undetermined(e)),
    };
    hashed(root, &hash, || {
        let (url, guard) = match serve::status(state, root) {
            Determination::Known(serve::Status::Running(st)) => (st.url, None),
            Determination::Known(serve::Status::Stopped) => match serve::start(state, root) {
                Determination::Known(st) => (
                    st.url,
                    Some(StartedServer {
                        state,
                        root,
                        armed: true,
                    }),
                ),
                Determination::Undetermined(why) => {
                    return Verdict::undetermined(format!(
                        "the dev server could not be started: {}",
                        why.as_str()
                    ))
                }
            },
            Determination::Undetermined(why) => {
                return Verdict::undetermined(format!(
                    "the dev server state is unclear: {}",
                    why.as_str()
                ))
            }
        };
        let limit = deadline.remaining();
        let v = if limit.is_zero() {
            crate::history::note_timeout();
            Verdict::undetermined(format!(
                "timeout: the {}s limit passed while starting the dev server",
                CHECK_LIMIT.as_secs()
            ))
        } else {
            let target = format!("{}{page}", url.trim_end_matches('/'));
            let input = serde_json::json!({
                "mode": "check",
                "url": target,
                "selector": selector,
                "timeout_ms": millis(limit),
            });
            match run_three(&node_dir, &input, limit) {
                ThreeOut::Png(png) => {
                    judge_image(&png, &format!("screenshot of `{selector}` on {target}"))
                }
                ThreeOut::NotVisible(why) => Verdict::violation(why),
                ThreeOut::Undetermined { why, .. } => Verdict::undetermined(why),
                ThreeOut::Webgl(_) => {
                    Verdict::undetermined("the three.js runner answered a probe, not a check")
                }
            }
        };
        match guard {
            Some(g) => combine(vec![v, g.stop()]),
            None => v,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("").unwrap(), b"");
        assert!(base64_decode("a").is_err());
        assert!(base64_decode("a$==").is_err());
    }

    #[test]
    fn probe_script_embeds_the_path() {
        let s = probe_script(Path::new("/tmp/x \"y\".glb"));
        assert!(s.contains(r#"filepath="/tmp/x \"y\".glb""#), "{s}");
    }
}
