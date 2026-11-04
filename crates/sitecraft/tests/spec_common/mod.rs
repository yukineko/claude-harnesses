//! Shared harness for the `spec_r*.rs` tests of spec `sitecraft-capabilities`
//! (`specs/sitecraft-capabilities.toml`, canon
//! `docs/specs/sitecraft-capabilities-rulings.md`).
//!
//! Written by an independent test author (CLAUDE.md §2(a)). Implementers make
//! these tests pass without editing them.
//!
//! # Process conventions (every spec test)
//!
//! * The binary is `env!("CARGO_BIN_EXE_sitecraft")`, run with
//!   `--root <root>` (except `sitecraft init`), `SITECRAFT_STATE_DIR=<state>`,
//!   `HOME=<home>`, `CLAUDE_PLUGIN_ROOT=<crate dir>` (so bundled assets under
//!   `crates/sitecraft/assets/` resolve), and with `SITECRAFT_BLENDER`,
//!   `SITECRAFT_NODE`, `SITECRAFT_NPM`, `SITECRAFT_MARKETPLACE`, `GIT_DIR`,
//!   `GIT_WORK_TREE`, `GIT_INDEX_FILE` removed unless a test sets them.
//! * The process **cwd is the tempdir base, not the project root**, so
//!   anything the rulings say runs "with cwd = root" (D4) or resolves "from the
//!   project root" (D3) must use `--root` explicitly.
//! * Every run has a hard deadline (killed + test failure when exceeded).
//! * `<root-key>` = lowercase sha256 hex of the canonical project root path
//!   string (D5). Results: `<state>/capability-results/<root-key>/<name>.json`
//!   (D5). History: `<state>/check-history/<root-key>.jsonl` (D11).
//!
//! # Tool prerequisites
//!
//! `git`, `python3` (fake Blender, dev-server stub) and `node` (only for the
//! three.js tests, which run sitecraft's own bundled script against the fake
//! Playwright in `tests/fixtures/spec/fake_playwright/`) on `PATH`, like the
//! existing sitecraft tests. No real Blender, Playwright, Chromium or network.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Catalog area ids (pinned by the decide tests).
pub const AREAS: [&str; 12] = [
    "auth",
    "users-roles",
    "admin",
    "storage",
    "data-analytics",
    "web-analytics",
    "processing",
    "workflow",
    "notification",
    "ai",
    "formats",
    "exposure",
];

/// Default deadline of one sitecraft run.
pub const RUN_DEADLINE: Duration = Duration::from_secs(120);

pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn spec_fixtures() -> PathBuf {
    crate_dir().join("tests/fixtures/spec")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Result of one process run.
#[derive(Debug)]
pub struct Out {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

impl Out {
    pub fn all(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
    pub fn describe(&self) -> String {
        format!(
            "exit={:?} elapsed={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.code, self.elapsed, self.stdout, self.stderr
        )
    }
    /// Stdout lines that parse as JSON objects.
    pub fn json_objects(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
            .filter(Value::is_object)
            .collect()
    }
}

#[track_caller]
pub fn expect_code(o: &Out, want: i32, what: &str) {
    assert_eq!(
        o.code,
        Some(want),
        "{what}: expected exit {want}\n{}",
        o.describe()
    );
}

#[track_caller]
pub fn expect_failure(o: &Out, what: &str) {
    assert!(
        matches!(o.code, Some(c) if c != 0),
        "{what}: expected a failing exit (non-zero)\n{}",
        o.describe()
    );
}

/// Spawn `cmd` (stdin = `stdin_text` or null), wait at most `deadline`.
pub fn run_deadline(mut cmd: Command, stdin_text: Option<&str>, deadline: Duration) -> Out {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin_text.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let start = Instant::now();
    let mut child = cmd.spawn().expect("spawn process");
    if let Some(t) = stdin_text {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(t.as_bytes()).expect("write stdin");
    }
    let mut so = child.stdout.take().expect("stdout");
    let mut se = child.stderr.take().expect("stderr");
    let t_out = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = so.read_to_end(&mut v);
        v
    });
    let t_err = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = se.read_to_end(&mut v);
        v
    });
    let status = loop {
        if let Some(st) = child.try_wait().expect("try_wait") {
            break st;
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("process did not finish within {deadline:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let elapsed = start.elapsed();
    Out {
        code: status.code(),
        stdout: String::from_utf8_lossy(&t_out.join().expect("stdout reader")).into_owned(),
        stderr: String::from_utf8_lossy(&t_err.join().expect("stderr reader")).into_owned(),
        elapsed,
    }
}

pub fn write(path: &Path, content: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write file");
}

pub fn write_exec(path: &Path, content: &str) {
    write(path, content);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod 755");
}

pub fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Absolute path of `prog` found on the test process's `PATH`.
pub fn which(prog: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is set");
    for dir in std::env::split_paths(&path) {
        let p = dir.join(prog);
        if let Ok(m) = std::fs::metadata(&p) {
            if m.is_file() && m.permissions().mode() & 0o111 != 0 {
                return p;
            }
        }
    }
    panic!("test prerequisite `{prog}` is not on PATH");
}

pub fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    l.local_addr().expect("local addr").port()
}

pub fn port_is_listening(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().expect("addr"),
        Duration::from_millis(500),
    )
    .is_ok()
}

pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------- decisions

/// `[areas.<a>] relevance = "irrelevant"` for every area: every catalog
/// decision recorded, nothing pending.
pub fn areas_all_irrelevant() -> String {
    AREAS
        .iter()
        .map(|a| format!("[areas.{a}]\nrelevance = \"irrelevant\"\n\n"))
        .collect()
}

/// `[purpose] text = <text>` (D1).
pub fn purpose(text: &str) -> String {
    format!("[purpose]\ntext = {}\n\n", toml_str(text))
}

/// A complete record: purpose + every area irrelevant.
pub fn complete_record() -> String {
    format!(
        "{}{}",
        purpose("Spec test purpose."),
        areas_all_irrelevant()
    )
}

/// A TOML basic string.
pub fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

pub fn toml_arr(items: &[&str]) -> String {
    let v: Vec<String> = items.iter().map(|s| toml_str(s)).collect();
    format!("[{}]", v.join(", "))
}

/// `[capabilities.<name>]` for `builtin:blender-headless` (D2).
pub fn cap_blender(name: &str, script: &str, glb: &str, render: &str, inputs: &[&str]) -> String {
    let mut s = format!(
        "[capabilities.{name}]\nrelevance = \"relevant\"\nhow = \"headless blender script\"\ndone_when = \"a GLB and a drawn render exist\"\n\n[capabilities.{name}.verify]\nkind = \"builtin:blender-headless\"\nscript = {}\nglb = {}\nrender = {}\n",
        toml_str(script),
        toml_str(glb),
        toml_str(render)
    );
    if !inputs.is_empty() {
        s.push_str(&format!("inputs = {}\n", toml_arr(inputs)));
    }
    s.push('\n');
    s
}

/// `[capabilities.<name>]` for `builtin:threejs-render` (D2).
pub fn cap_threejs(name: &str, page: &str, selector: Option<&str>) -> String {
    let mut s = format!(
        "[capabilities.{name}]\nrelevance = \"relevant\"\nhow = \"three.js viewer page\"\ndone_when = \"the canvas shows the model\"\n\n[capabilities.{name}.verify]\nkind = \"builtin:threejs-render\"\npage = {}\n",
        toml_str(page)
    );
    if let Some(sel) = selector {
        s.push_str(&format!("selector = {}\n", toml_str(sel)));
    }
    s.push('\n');
    s
}

/// `[capabilities.<name>]` for `command` (D2).
pub fn cap_command(name: &str, argv: &[&str], inputs: &[&str]) -> String {
    format!(
        "[capabilities.{name}]\nrelevance = \"relevant\"\nhow = \"a converter script\"\ndone_when = \"the converter exits 0\"\n\n[capabilities.{name}.verify]\nkind = \"command\"\nargv = {}\ninputs = {}\n\n",
        toml_arr(argv),
        toml_arr(inputs)
    )
}

// ---------------------------------------------------------------- project

/// A temp project: `<base>/site` is the root, `<base>/state` the state dir,
/// `<base>/home` HOME. All paths are canonical (macOS /var → /private/var).
pub struct Project {
    _tmp: tempfile::TempDir,
    pub base: PathBuf,
    pub root: PathBuf,
    pub state: PathBuf,
    pub home: PathBuf,
}

impl Project {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonicalize tempdir");
        let root = base.join("site");
        let state = base.join("state");
        let home = base.join("home");
        for d in [&root, &state, &home] {
            std::fs::create_dir_all(d).expect("mkdir");
        }
        Project {
            _tmp: tmp,
            base,
            root,
            state,
            home,
        }
    }

    /// Same, with `git init` + identity in the root.
    pub fn new_git() -> Self {
        let p = Project::new();
        git(&p.root, &["init", "-q"]);
        git(&p.root, &["config", "user.email", "t@example.com"]);
        git(&p.root, &["config", "user.name", "t"]);
        p
    }

    /// The sitecraft command with the conventions in the module docs. `args`
    /// follow `--root <root>`.
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = self.bare_cmd();
        c.arg("--root").arg(&self.root).args(args);
        c
    }

    /// The sitecraft command without `--root`.
    pub fn bare_cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_sitecraft"));
        c.current_dir(&self.base)
            .env("SITECRAFT_STATE_DIR", &self.state)
            .env("HOME", &self.home)
            .env("CLAUDE_PLUGIN_ROOT", crate_dir())
            .env_remove("SITECRAFT_BLENDER")
            .env_remove("SITECRAFT_NODE")
            .env_remove("SITECRAFT_NPM")
            .env_remove("SITECRAFT_MARKETPLACE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        c
    }

    pub fn run(&self, args: &[&str]) -> Out {
        run_deadline(self.cmd(args), None, RUN_DEADLINE)
    }

    pub fn run_env(&self, args: &[&str], envs: &[(&str, OsString)]) -> Out {
        self.run_env_with_deadline(args, envs, RUN_DEADLINE)
    }

    /// Like [`Project::run_env`] with an explicit harness deadline, for the
    /// `#[ignore]`d tests that must outlive [`RUN_DEADLINE`] to observe a
    /// sitecraft time limit (120 s probe, 300 s check). Choose a deadline
    /// above the limit under test and above that test's own `elapsed` bound.
    pub fn run_env_with_deadline(
        &self,
        args: &[&str],
        envs: &[(&str, OsString)],
        deadline: Duration,
    ) -> Out {
        let mut c = self.cmd(args);
        for (k, v) in envs {
            c.env(k, v);
        }
        run_deadline(c, None, deadline)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn write(&self, rel: &str, content: impl AsRef<[u8]>) {
        write(&self.path(rel), content);
    }

    pub fn decisions_path(&self) -> PathBuf {
        self.root.join("sitecraft").join("decisions.toml")
    }

    pub fn write_decisions(&self, body: &str) {
        write(&self.decisions_path(), body);
    }

    pub fn decisions_bytes(&self) -> Option<Vec<u8>> {
        std::fs::read(self.decisions_path()).ok()
    }

    pub fn decisions(&self) -> toml::Value {
        let body = std::fs::read_to_string(self.decisions_path()).expect("read decisions.toml");
        toml::from_str(&body).expect("decisions.toml parses as TOML")
    }

    /// `[[history]]` rows of decisions.toml (empty when absent).
    pub fn history(&self) -> Vec<toml::Value> {
        self.decisions()
            .get("history")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default()
    }

    /// `sitecraft trust` (the human step); returns its output.
    pub fn trust(&self) -> Out {
        self.run(&["trust"])
    }

    pub fn trust_ok(&self) {
        let o = self.trust();
        expect_code(&o, 0, "setup: sitecraft trust");
    }

    pub fn trust_store(&self) -> serde_json::Map<String, Value> {
        let text = std::fs::read_to_string(self.state.join("trust.json")).expect("read trust.json");
        let v: Value = serde_json::from_str(&text).expect("trust.json is JSON");
        v.as_object().expect("trust.json is an object").clone()
    }

    pub fn canonical_root(&self) -> String {
        self.root
            .canonicalize()
            .expect("canonical root")
            .to_string_lossy()
            .into_owned()
    }

    /// D5 `<root-key>`.
    pub fn root_key(&self) -> String {
        sha256_hex(self.canonical_root().as_bytes())
    }

    pub fn results_dir(&self) -> PathBuf {
        self.state.join("capability-results").join(self.root_key())
    }

    pub fn results_file(&self, name: &str) -> PathBuf {
        self.results_dir().join(format!("{name}.json"))
    }

    pub fn result(&self, name: &str) -> Value {
        let p = self.results_file(name);
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| {
            panic!(
                "capability result {} is missing or unreadable: {e}",
                p.display()
            )
        });
        serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{} is not JSON ({e}): {text}", p.display()))
    }

    pub fn history_file(&self) -> PathBuf {
        self.state
            .join("check-history")
            .join(format!("{}.jsonl", self.root_key()))
    }

    /// Parsed lines of the D11 history file (panics when absent).
    pub fn check_history(&self) -> Vec<Value> {
        let p = self.history_file();
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| {
            panic!(
                "check history {} is missing or unreadable: {e}",
                p.display()
            )
        });
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str(l)
                    .unwrap_or_else(|e| panic!("history line is not JSON ({e}): {l}"))
            })
            .collect()
    }

    /// Number of history lines (0 when the file is absent).
    pub fn check_history_len(&self) -> usize {
        match std::fs::read_to_string(self.history_file()) {
            Ok(t) => t.lines().filter(|l| !l.trim().is_empty()).count(),
            Err(_) => 0,
        }
    }

    /// Copy the fake Playwright into `<root>/node_modules/playwright/`.
    pub fn install_fake_playwright(&self) {
        let src = spec_fixtures().join("fake_playwright/index.js");
        let dst = self.root.join("node_modules/playwright/index.js");
        write(&dst, std::fs::read(src).expect("read fake playwright"));
    }

    /// Write a fake-Playwright scenario file and return its path.
    pub fn scenario(&self, v: &Value) -> PathBuf {
        let p = self.base.join(format!(
            "scenario-{}.json",
            sha256_hex(v.to_string().as_bytes())
        ));
        write(&p, v.to_string());
        p
    }

    /// A `/bin/sh` wrapper running the fake Blender; returns its path.
    pub fn fake_blender(&self) -> PathBuf {
        let p = self.base.join("bin/fake-blender");
        write_exec(&p, &fake_blender_wrapper());
        p
    }

    pub fn marker(&self, name: &str) -> PathBuf {
        self.base.join("markers").join(name)
    }

    /// A `sitecraft.toml` whose dev server is `python3 -m http.server` on a
    /// free port (marker file first). Returns the port.
    pub fn write_sitecraft_toml(&self, extra: &str) -> u16 {
        let port = free_port();
        let dev = self.base.join("bin/dev-server.sh");
        write_exec(
            &dev,
            &format!(
                "#!/bin/sh\nmkdir -p '{mdir}'\n: > '{marker}'\nexec '{py}' -m http.server {port} --bind 127.0.0.1 --directory '{root}'\n",
                mdir = self.base.join("markers").display(),
                marker = self.marker("dev-server-ran").display(),
                py = which("python3").display(),
                root = self.root.display()
            ),
        );
        self.write(
            "sitecraft.toml",
            format!(
                "command = [{dev}]\nport = {port}\nready_path = \"/\"\nready_timeout_secs = 30\npages = [\"/\"]\nbuild_dir = \"dist\"\nweb_globs = [\"src/**\", \"index.html\"]\n{extra}",
                dev = toml_str(&dev.display().to_string()),
            ),
        );
        port
    }
}

impl Default for Project {
    fn default() -> Self {
        Project::new()
    }
}

/// Stops the dev server of `root` on drop (best effort; the test outcome is
/// decided by its assertions, this only avoids leaking a server).
pub struct ServeGuard<'a>(pub &'a Project);

impl Drop for ServeGuard<'_> {
    fn drop(&mut self) {
        let _ = run_deadline(
            self.0.cmd(&["serve", "stop"]),
            None,
            Duration::from_secs(30),
        );
    }
}

pub fn fake_blender_wrapper() -> String {
    format!(
        "#!/bin/sh\nexport PYTHONDONTWRITEBYTECODE=1\nexec '{py}' -B '{script}' \"$@\"\n",
        py = which("python3").display(),
        script = spec_fixtures().join("fake_blender.py").display()
    )
}

// ---------------------------------------------------------------- images

/// RGBA8 PNG from a pixel function.
pub fn png_rgba(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&f(x, y));
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().expect("png header");
        wr.write_image_data(&data).expect("png data");
    }
    out
}

/// Single colour image (D4: violation).
pub fn png_solid(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    png_rgba(w, h, |_, _| [rgb[0], rgb[1], rgb[2], 255])
}

/// 100x100 white image with the first `n` pixels (row-major, starting at
/// (10,10) inside a 10-wide block, never touching a corner) set to `ink`.
pub fn png_with_drawn(n: u32, ink: [u8; 4]) -> Vec<u8> {
    png_rgba(100, 100, |x, y| {
        let inside = (10..20).contains(&x) && (10..30).contains(&y);
        if inside && (y - 10) * 10 + (x - 10) < n {
            ink
        } else {
            [255, 255, 255, 255]
        }
    })
}

/// A PNG whose IHDR declares `w` x `h` (may be 0), hand-built (encoders
/// refuse a zero dimension).
pub fn png_raw_dims(w: u32, h: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    // zlib stream of zero bytes: empty stored block.
    chunk(
        &mut out,
        b"IDAT",
        &[
            0x78, 0x01, 0x01, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
        ],
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

// ---------------------------------------------------------------- GLB

/// A well-formed glTF 2 binary (D4: header length == file size).
pub fn glb_valid() -> Vec<u8> {
    let mut body = br#"{"asset":{"version":"2.0"},"scenes":[{"nodes":[]}],"scene":0}"#.to_vec();
    while body.len() % 4 != 0 {
        body.push(b' ');
    }
    let total = (12 + 8 + body.len()) as u32;
    let mut out = b"glTF".to_vec();
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn glb_with_version(v: u32) -> Vec<u8> {
    let mut g = glb_valid();
    g[4..8].copy_from_slice(&v.to_le_bytes());
    g
}

pub fn glb_bad_magic() -> Vec<u8> {
    let mut g = glb_valid();
    g[0..4].copy_from_slice(b"gLTX");
    g
}

pub fn glb_bad_length() -> Vec<u8> {
    let mut g = glb_valid();
    let wrong = (g.len() as u32) + 4;
    g[8..12].copy_from_slice(&wrong.to_le_bytes());
    g
}

// ---------------------------------------------------------------- SKILL.md

pub fn skill_text() -> String {
    std::fs::read_to_string(crate_dir().join("skills/sitecraft/SKILL.md")).expect("read SKILL.md")
}

/// Blank-line separated blocks of `text`.
pub fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.trim().is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
        } else {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Some paragraph of `text` contains every needle.
pub fn has_paragraph_with(text: &str, needles: &[&str]) -> bool {
    paragraphs(text)
        .iter()
        .any(|p| needles.iter().all(|n| p.contains(n)))
}

#[track_caller]
pub fn assert_paragraph_with(text: &str, needles: &[&str], what: &str) {
    assert!(
        has_paragraph_with(text, needles),
        "{what}: no paragraph of SKILL.md contains all of {needles:?}"
    );
}

/// The markdown section starting at the first line that starts with
/// `heading` up to (not including) the next heading of the same or higher
/// level. Empty when absent.
pub fn section(text: &str, heading: &str) -> String {
    let level = heading.chars().take_while(|c| *c == '#').count();
    let mut out = String::new();
    let mut inside = false;
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
        }
        if !in_code && !inside && line.starts_with(heading) {
            inside = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if inside && !in_code && line.starts_with('#') {
            let l = line.chars().take_while(|c| *c == '#').count();
            if l <= level && line.chars().nth(l) == Some(' ') {
                break;
            }
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------- stop gate

/// A git project set up like `tests/gate_paths.rs`: trusted-able
/// `sitecraft.toml` (python dev server), clean sources/build output, an
/// `npm audit` stub reporting zero vulnerabilities, a `node` stub that only
/// leaves a marker, a transcript without web edits, a complete decision
/// record (with purpose). Trust is NOT recorded here.
pub struct GateProject {
    pub p: Project,
    pub port: u16,
    pub transcript: PathBuf,
    pub npm_stub: PathBuf,
    pub node_stub: PathBuf,
}

impl GateProject {
    pub fn new(extra_decisions: &str) -> Self {
        let p = Project::new_git();
        let port = p.write_sitecraft_toml("");
        p.write_decisions(&format!("{}{extra_decisions}", complete_record()));
        p.write("index.html", "<!doctype html><title>t</title>\n");
        p.write("src/main.js", "export const answer = 42;\n");
        p.write("dist/app.js", "console.log('ok');\n");
        p.write(
            "package.json",
            "{\"name\":\"t\",\"version\":\"0.0.0\",\"private\":true}\n",
        );
        p.write(
            "package-lock.json",
            "{\"name\":\"t\",\"version\":\"0.0.0\",\"lockfileVersion\":3,\"requires\":true,\"packages\":{}}\n",
        );
        git(&p.root, &["add", "-A"]);
        git(&p.root, &["commit", "-q", "--no-verify", "-m", "init"]);
        let npm_stub = p.base.join("bin/npm");
        write_exec(
            &npm_stub,
            &format!(
                "#!/bin/sh\nmkdir -p '{m}'\n: > '{m}/npm-ran'\ncat <<'EOF'\n{{\"auditReportVersion\":2,\"vulnerabilities\":{{}},\"metadata\":{{\"vulnerabilities\":{{\"info\":0,\"low\":0,\"moderate\":0,\"high\":0,\"critical\":0,\"total\":0}},\"dependencies\":{{\"prod\":1,\"dev\":0,\"optional\":0,\"peer\":0,\"peerOptional\":0,\"total\":1}}}}}}\nEOF\nexit 0\n",
                m = p.base.join("markers").display()
            ),
        );
        let node_stub = p.base.join("bin/node-stub");
        write_exec(
            &node_stub,
            &format!(
                "#!/bin/sh\nmkdir -p '{m}'\n: > '{m}/node-ran'\nexit 2\n",
                m = p.base.join("markers").display()
            ),
        );
        let transcript = p.base.join("transcript.jsonl");
        let line = serde_json::json!({
            "type": "user",
            "sessionId": "s",
            "message": {"role": "user", "content": "update the readme"}
        });
        write(&transcript, format!("{line}\n"));
        GateProject {
            p,
            port,
            transcript,
            npm_stub,
            node_stub,
        }
    }

    pub fn payload(&self, session: &str) -> String {
        serde_json::json!({
            "session_id": session,
            "transcript_path": self.transcript.display().to_string(),
            "cwd": self.p.root.display().to_string(),
            "hook_event_name": "Stop",
            "stop_hook_active": false,
        })
        .to_string()
    }

    /// Run `sitecraft gate` with the Stop payload; extra env applied last.
    pub fn gate(&self, session: &str, envs: &[(&str, OsString)]) -> Out {
        let mut c = self.p.bare_cmd();
        c.arg("gate")
            .env("SITECRAFT_NODE", &self.node_stub)
            .env("SITECRAFT_NPM", &self.npm_stub)
            .env("CLAUDE_CODE_SESSION_ID", session);
        for (k, v) in envs {
            c.env(k, v);
        }
        run_deadline(c, Some(&self.payload(session)), Duration::from_secs(180))
    }

    /// Run a sitecraft subcommand with the stubs in place.
    pub fn run_env(&self, args: &[&str], envs: &[(&str, OsString)]) -> Out {
        let mut c = self.p.cmd(args);
        c.env("SITECRAFT_NODE", &self.node_stub)
            .env("SITECRAFT_NPM", &self.npm_stub);
        for (k, v) in envs {
            c.env(k, v);
        }
        run_deadline(c, None, RUN_DEADLINE)
    }
}

/// The block reason of a gate run, if it blocked.
pub fn block_reason(o: &Out) -> Option<String> {
    o.json_objects().into_iter().find_map(|v| {
        if v.get("decision").and_then(|d| d.as_str()) == Some("block") {
            v.get("reason").and_then(|r| r.as_str()).map(str::to_string)
        } else {
            None
        }
    })
}

#[track_caller]
pub fn assert_block(o: &Out, what: &str) -> String {
    assert_eq!(
        o.code,
        Some(0),
        "{what}: gate must exit 0\n{}",
        o.describe()
    );
    block_reason(o).unwrap_or_else(|| panic!("{what}: expected a block\n{}", o.describe()))
}

#[track_caller]
pub fn assert_allow(o: &Out, what: &str) {
    assert_eq!(
        o.code,
        Some(0),
        "{what}: gate must exit 0\n{}",
        o.describe()
    );
    assert!(
        block_reason(o).is_none(),
        "{what}: expected ALLOW, got a block\n{}",
        o.describe()
    );
}

pub fn os(s: impl AsRef<std::ffi::OsStr>) -> OsString {
    s.as_ref().to_os_string()
}
