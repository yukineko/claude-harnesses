//! Spec `sitecraft-capabilities` R4 — `sitecraft check capability <name>`
//! (rulings D4, U2).
//!
//! # CLI surface assumed (rulings D4)
//!
//! * `sitecraft --root <root> check capability <name>`: exit 0 clean /
//!   1 violation / 2 undetermined. Unregistered or `relevance = "irrelevant"`
//!   → 2. Time limit 300 s → 2 with `timeout` in the output.
//! * Pixel rule (both builtin kinds): background = most frequent RGB of the 4
//!   corners (alpha ignored); not unique → the candidate giving the smallest
//!   drawn ratio. Drawn pixel = some R/G/B channel differs from the
//!   background by more than 16. Ratio >= 1% clean, < 1% violation; width or
//!   height 0 violation.
//! * GLB condition: `glTF` magic, u32 LE version 2, u32 LE length == file
//!   size; 0 bytes or any mismatch → violation.
//! * `builtin:blender-headless`: deletes `glb` and `render` first; blender
//!   from the D3 lookup (`SITECRAFT_BLENDER` here; not found → 2); runs with
//!   cwd = root `<blender> --background --factory-startup --python <script>
//!   -- --glb <abs> --render <abs>`. Non-zero exit, missing output,
//!   unreadable-as-PNG, bad GLB, pixel violation → 1. Missing script → 2.
//!   Read IO failure (permission) → 2.
//! * `builtin:threejs-render`: needs a trusted `sitecraft.toml` (absent or
//!   untrusted → 2). Starts the dev server when it is not running (like
//!   `serve start`) and stops only a server it started. Opens
//!   `http://127.0.0.1:<port><page>` in Playwright Chromium (resolved from
//!   the root, node = `$SITECRAFT_NODE`), waits for `selector` (default
//!   `canvas`) to be visible through `page.waitForSelector` or
//!   `locator.waitFor` (fake: not visible → TimeoutError → 1), waits 2000 ms,
//!   screenshots the element and applies the pixel rule. Playwright /
//!   Chromium cannot start → 2. Fake surface:
//!   `tests/fixtures/spec/fake_playwright/index.js`.
//! * `command`: no D2-trust → 2 without running. Trusted: cwd = root, run
//!   argv; command exit 0 → 0, 2 → 2, other non-zero or signal → 1, spawn
//!   failure → 2.
//!
//! `#[ignore]`d: `check_times_out_after_300_seconds` (needs > 300 s).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;
use spec_common::*;

const MAKE_PY: &str = r#"import os, shutil, sys
a = sys.argv[sys.argv.index("--") + 1:]
glb = a[a.index("--glb") + 1]
render = a[a.index("--render") + 1]
for src, dst in ((os.environ.get("SPEC_SRC_GLB"), glb), (os.environ.get("SPEC_SRC_PNG"), render)):
    if src:
        d = os.path.dirname(dst)
        if d:
            os.makedirs(d, exist_ok=True)
        shutil.copyfile(src, dst)
for var, path in (("SPEC_CHMOD_RENDER", render), ("SPEC_CHMOD_GLB", glb)):
    mode = os.environ.get(var)
    if mode:
        os.chmod(path, int(mode, 8))
"#;

struct Blender {
    p: Project,
}

impl Blender {
    fn new() -> Self {
        let p = Project::new();
        p.write("scripts/make.py", MAKE_PY);
        p.write_decisions(&format!(
            "{}{}",
            complete_record(),
            cap_blender(
                "blender",
                "scripts/make.py",
                "out/model.glb",
                "out/render.png",
                &[]
            )
        ));
        Blender { p }
    }

    fn src(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.p.base.join("src").join(name);
        write(&path, bytes);
        path
    }

    /// Run `check capability blender`, the script copying `glb` / `png`
    /// (None = write nothing) to the outputs.
    fn check(
        &self,
        glb: Option<&[u8]>,
        png: Option<&[u8]>,
        extra: &[(&'static str, OsString)],
    ) -> Out {
        self.check_with_deadline(glb, png, extra, RUN_DEADLINE)
    }

    fn check_with_deadline(
        &self,
        glb: Option<&[u8]>,
        png: Option<&[u8]>,
        extra: &[(&'static str, OsString)],
        deadline: std::time::Duration,
    ) -> Out {
        let mut env: Vec<(&str, OsString)> = vec![
            ("SITECRAFT_BLENDER", os(self.p.fake_blender())),
            (
                "FAKE_BLENDER_LOG",
                os(self.p.base.join("blender-log.jsonl")),
            ),
        ];
        if let Some(g) = glb {
            env.push(("SPEC_SRC_GLB", os(self.src("in.glb", g))));
        }
        if let Some(i) = png {
            env.push(("SPEC_SRC_PNG", os(self.src("in.png", i))));
        }
        env.extend(extra.iter().cloned());
        self.p
            .run_env_with_deadline(&["check", "capability", "blender"], &env, deadline)
    }
}

fn drawn_png() -> Vec<u8> {
    png_with_drawn(150, [0, 0, 0, 255])
}

#[test]
fn unregistered_and_irrelevant_capabilities_are_exit_2() {
    let b = Blender::new();
    let o = b.p.run_env(
        &["check", "capability", "nope"],
        &[("SITECRAFT_BLENDER", os(b.p.fake_blender()))],
    );
    expect_code(&o, 2, "check capability of an unregistered name");
    // Positive control: the registered capability is clean in the same root.
    expect_code(
        &b.check(Some(&glb_valid()), Some(&drawn_png()), &[]),
        0,
        "positive control: registered blender capability",
    );
    let irrelevant = cap_blender(
        "blender",
        "scripts/make.py",
        "out/model.glb",
        "out/render.png",
        &[],
    )
    .replace("relevance = \"relevant\"", "relevance = \"irrelevant\"");
    b.p.write_decisions(&format!("{}{irrelevant}", complete_record()));
    expect_code(
        &b.check(Some(&glb_valid()), Some(&drawn_png()), &[]),
        2,
        "check capability of an irrelevant capability",
    );
}

#[test]
#[ignore = "takes > 300 s: the check capability time limit is 300 s (rulings D4)"]
fn check_times_out_after_300_seconds() {
    let b = Blender::new();
    // Harness deadline 400 s: above the 300 s limit under test and above the
    // `< 390` bound asserted below (the default 120 s would kill sitecraft first).
    let o = b.check_with_deadline(
        Some(&glb_valid()),
        Some(&drawn_png()),
        &[("FAKE_BLENDER_SLEEP", os("400"))],
        std::time::Duration::from_secs(400),
    );
    expect_code(&o, 2, "check capability past 300 s");
    assert!(o.all().contains("timeout"), "{}", o.describe());
    assert!(
        o.elapsed.as_secs() < 390,
        "must stop at 300 s: {:?}",
        o.elapsed
    );
}

// ---------------------------------------------------------------- pixel rule

fn expect_pixels(png: &[u8], want: i32, what: &str) {
    let b = Blender::new();
    let o = b.check(Some(&glb_valid()), Some(png), &[]);
    expect_code(&o, want, what);
}

#[test]
fn single_colour_render_is_a_violation() {
    expect_pixels(
        &png_solid(64, 64, [255, 255, 255]),
        1,
        "single-colour white render",
    );
    expect_pixels(
        &png_solid(64, 64, [12, 80, 200]),
        1,
        "single-colour blue render",
    );
}

#[test]
fn render_drawn_at_one_percent_or_more_is_clean() {
    expect_pixels(
        &png_with_drawn(100, [0, 0, 0, 255]),
        0,
        "exactly 1% drawn (100 of 10000)",
    );
    expect_pixels(&png_with_drawn(200, [0, 0, 0, 255]), 0, "2% drawn");
}

#[test]
fn render_drawn_below_one_percent_is_a_violation() {
    expect_pixels(&png_with_drawn(99, [0, 0, 0, 255]), 1, "99 of 10000 drawn");
}

#[test]
fn channel_difference_must_exceed_16() {
    expect_pixels(
        &png_with_drawn(150, [239, 239, 239, 255]),
        1,
        "difference exactly 16 on every channel is not drawn",
    );
    expect_pixels(
        &png_with_drawn(150, [238, 238, 238, 255]),
        0,
        "difference 17 is drawn",
    );
    expect_pixels(
        &png_with_drawn(150, [255, 255, 238, 255]),
        0,
        "difference 17 on one channel (B) is drawn",
    );
}

#[test]
fn alpha_is_ignored() {
    expect_pixels(
        &png_with_drawn(150, [255, 255, 255, 0]),
        1,
        "pixels differing only in alpha are not drawn",
    );
    expect_pixels(
        &png_rgba(64, 64, |x, y| [40, 40, 40, ((x * 7 + y * 3) % 256) as u8]),
        1,
        "one RGB colour with varying alpha is single-colour",
    );
}

#[test]
fn tied_corner_colours_pick_the_candidate_with_the_smallest_drawn_ratio() {
    // Corners: TL black, BL black, TR white, BR white (2 / 2). Everything
    // else white. Background white → 2 drawn pixels (violation);
    // background black → 9998 drawn (clean). The restricted side wins.
    let png = png_rgba(100, 100, |x, y| {
        if x == 0 && (y == 0 || y == 99) {
            [0, 0, 0, 255]
        } else {
            [255, 255, 255, 255]
        }
    });
    expect_pixels(
        &png,
        1,
        "2/2 corner tie resolves to the smallest drawn ratio",
    );
}

#[test]
fn four_distinct_corner_colours_pick_the_candidate_with_the_smallest_drawn_ratio() {
    // Body and TL are grey; TR red, BL green, BR blue. Background grey → 3
    // drawn pixels (violation); any other candidate → ~100% (clean).
    let png = png_rgba(100, 100, |x, y| match (x, y) {
        (99, 0) => [255, 0, 0, 255],
        (0, 99) => [0, 255, 0, 255],
        (99, 99) => [0, 0, 255, 255],
        _ => [128, 128, 128, 255],
    });
    expect_pixels(
        &png,
        1,
        "4 distinct corners resolve to the smallest drawn ratio",
    );
}

#[test]
fn zero_sized_or_unreadable_png_is_a_violation() {
    expect_pixels(&png_raw_dims(0, 10), 1, "PNG declaring width 0");
    expect_pixels(&png_raw_dims(10, 0), 1, "PNG declaring height 0");
    expect_pixels(b"this is not a png", 1, "render that is not a PNG");
}

// ---------------------------------------------------------------- GLB

#[test]
fn glb_condition_failures_are_violations() {
    for (what, glb) in [
        ("0-byte GLB", Vec::new()),
        ("bad magic", glb_bad_magic()),
        ("version 1", glb_with_version(1)),
        ("length != file size", glb_bad_length()),
    ] {
        let b = Blender::new();
        let o = b.check(Some(&glb), Some(&drawn_png()), &[]);
        expect_code(&o, 1, what);
    }
    // Positive control: a valid GLB with the same render is clean.
    let b = Blender::new();
    expect_code(
        &b.check(Some(&glb_valid()), Some(&drawn_png()), &[]),
        0,
        "valid GLB + drawn render",
    );
}

// ---------------------------------------------------------------- blender run

#[test]
fn blender_is_run_with_the_documented_argv_and_cwd() {
    let b = Blender::new();
    let o = b.check(Some(&glb_valid()), Some(&drawn_png()), &[]);
    expect_code(&o, 0, "check capability blender");
    let log = std::fs::read_to_string(b.p.base.join("blender-log.jsonl")).expect("blender ran");
    let calls: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(calls.len(), 1, "blender must run once: {calls:?}");
    let argv: Vec<String> = calls[0]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    assert_eq!(argv.len(), 9, "argv: {argv:?}");
    assert_eq!(
        argv[..3],
        ["--background", "--factory-startup", "--python"],
        "{argv:?}"
    );
    let script = Path::new(&argv[3]);
    let script = if script.is_absolute() {
        script.to_path_buf()
    } else {
        b.p.root.join(script)
    };
    assert_eq!(
        script.canonicalize().unwrap(),
        b.p.path("scripts/make.py").canonicalize().unwrap(),
        "--python must name the capability's script: {argv:?}"
    );
    assert_eq!(argv[4], "--", "{argv:?}");
    assert_eq!(argv[5], "--glb", "{argv:?}");
    assert_eq!(argv[7], "--render", "{argv:?}");
    for (i, rel) in [(6, "out/model.glb"), (8, "out/render.png")] {
        let a = Path::new(&argv[i]);
        assert!(a.is_absolute(), "argv[{i}] must be absolute: {argv:?}");
        assert_eq!(
            a.canonicalize().unwrap(),
            b.p.path(rel).canonicalize().unwrap(),
            "argv[{i}] must be the absolute {rel}"
        );
    }
    let cwd = PathBuf::from(calls[0]["cwd"].as_str().unwrap());
    assert_eq!(
        cwd.canonicalize().unwrap(),
        b.p.root.canonicalize().unwrap(),
        "blender must run with cwd = root"
    );
}

#[test]
fn stale_outputs_are_deleted_before_the_run() {
    let b = Blender::new();
    b.p.write("out/model.glb", glb_valid());
    b.p.write("out/render.png", drawn_png());
    let o = b.check(None, None, &[]);
    expect_code(&o, 1, "script wrote nothing; old outputs must not pass");
    assert!(
        !b.p.path("out/model.glb").exists() && !b.p.path("out/render.png").exists(),
        "the old glb / render must have been deleted"
    );
}

#[test]
fn missing_render_is_a_violation() {
    let b = Blender::new();
    expect_code(
        &b.check(Some(&glb_valid()), None, &[]),
        1,
        "GLB written, no render",
    );
    let b = Blender::new();
    expect_code(
        &b.check(None, Some(&drawn_png()), &[]),
        1,
        "render written, no GLB",
    );
}

#[test]
fn blender_nonzero_exit_is_a_violation() {
    let b = Blender::new();
    b.p.write("out/model.glb", glb_valid());
    b.p.write("out/render.png", drawn_png());
    let o = b.check(
        Some(&glb_valid()),
        Some(&drawn_png()),
        &[("FAKE_BLENDER_EXIT", os("1"))],
    );
    expect_code(&o, 1, "blender exiting 1");
}

#[test]
fn blender_not_found_is_exit_2() {
    let b = Blender::new();
    // Positive control in the same root: with a blender the check is clean,
    // so the exit 2 below is about the lookup, not about argument parsing.
    expect_code(
        &b.check(Some(&glb_valid()), Some(&drawn_png()), &[]),
        0,
        "positive control: blender found",
    );
    let o = b.p.run_env(
        &["check", "capability", "blender"],
        &[("SITECRAFT_BLENDER", os(b.p.base.join("no-such-blender")))],
    );
    expect_code(&o, 2, "SITECRAFT_BLENDER naming a missing file");
}

#[test]
fn missing_script_is_exit_2() {
    let b = Blender::new();
    expect_code(
        &b.check(Some(&glb_valid()), Some(&drawn_png()), &[]),
        0,
        "positive control: script present",
    );
    std::fs::remove_file(b.p.path("scripts/make.py")).unwrap();
    let o = b.check(Some(&glb_valid()), Some(&drawn_png()), &[]);
    expect_code(&o, 2, "capability script does not exist");
}

#[test]
fn unreadable_outputs_are_exit_2() {
    let b = Blender::new();
    expect_code(
        &b.check(Some(&glb_valid()), Some(&drawn_png()), &[]),
        0,
        "positive control: readable outputs",
    );
    let o = b.check(
        Some(&glb_valid()),
        Some(&drawn_png()),
        &[("SPEC_CHMOD_RENDER", os("000"))],
    );
    expect_code(&o, 2, "render exists but cannot be read (mode 000)");
    let b = Blender::new();
    let o = b.check(
        Some(&glb_valid()),
        Some(&drawn_png()),
        &[("SPEC_CHMOD_GLB", os("000"))],
    );
    expect_code(&o, 2, "GLB exists but cannot be read (mode 000)");
}

// ---------------------------------------------------------------- three.js

struct Three {
    p: Project,
    port: u16,
}

impl Three {
    fn new(selector: Option<&str>) -> Self {
        let p = Project::new();
        let port = p.write_sitecraft_toml("");
        p.write_decisions(&format!(
            "{}{}",
            complete_record(),
            cap_threejs("threejs", "/viewer.html", selector)
        ));
        p.install_fake_playwright();
        Three { p, port }
    }

    fn log_path(&self) -> PathBuf {
        self.p.base.join("pw-log.jsonl")
    }

    fn log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.log_path())
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn check(&self, scenario: Value) -> Out {
        let sc = self.p.scenario(&scenario);
        self.p.run_env(
            &["check", "capability", "threejs"],
            &[
                ("SITECRAFT_NODE", os(which("node"))),
                ("FAKE_PLAYWRIGHT_SCENARIO", os(sc)),
                ("FAKE_PLAYWRIGHT_LOG", os(self.log_path())),
            ],
        )
    }

    fn shot(&self, png: &[u8]) -> String {
        let path = self.p.base.join(format!("shot-{}.png", sha256_hex(png)));
        write(&path, png);
        path.display().to_string()
    }

    fn serve_status(&self) -> String {
        let o = self.p.run(&["serve", "status"]);
        expect_code(&o, 0, "serve status");
        let v = o.json_objects().pop().expect("serve status JSON");
        v["status"].as_str().unwrap_or("").to_string()
    }
}

#[test]
fn threejs_without_sitecraft_toml_is_exit_2() {
    let t = Three::new(None);
    // Positive control in the same root: trusted sitecraft.toml → clean, so
    // the exit 2 below comes from the missing file.
    t.p.trust_ok();
    {
        let _g = ServeGuard(&t.p);
        let o = t.check(serde_json::json!({"visible": true, "screenshot": t.shot(&drawn_png())}));
        expect_code(&o, 0, "positive control: trusted sitecraft.toml");
    }
    let _ = std::fs::remove_file(t.log_path());
    std::fs::remove_file(t.p.path("sitecraft.toml")).unwrap();
    let o = t.check(serde_json::json!({"visible": true, "screenshot": t.shot(&drawn_png())}));
    expect_code(&o, 2, "threejs-render without sitecraft.toml");
    assert!(
        t.log().is_empty(),
        "Chromium must not be launched: {:?}",
        t.log()
    );
}

#[test]
fn threejs_with_untrusted_sitecraft_toml_is_exit_2() {
    let t = Three::new(None);
    let _g = ServeGuard(&t.p);
    let o = t.check(serde_json::json!({"visible": true, "screenshot": t.shot(&drawn_png())}));
    expect_code(&o, 2, "threejs-render with an untrusted sitecraft.toml");
    assert!(
        !t.p.marker("dev-server-ran").exists(),
        "an untrusted dev server command must not run"
    );
    // Positive control: trusting makes the same check clean.
    t.p.trust_ok();
    let o = t.check(serde_json::json!({"visible": true, "screenshot": t.shot(&drawn_png())}));
    expect_code(&o, 0, "threejs-render after trust");
}

#[test]
fn threejs_drawn_canvas_is_clean_and_the_server_it_started_is_stopped() {
    let t = Three::new(None);
    t.p.trust_ok();
    let _g = ServeGuard(&t.p);
    let o = t.check(serde_json::json!({"visible": true, "screenshot": t.shot(&drawn_png())}));
    expect_code(&o, 0, "threejs-render with a drawn canvas");
    assert!(
        t.p.marker("dev-server-ran").exists(),
        "the dev server was not running, so the check must start it"
    );
    assert_eq!(
        t.serve_status(),
        "stopped",
        "the server it started must be stopped"
    );
    assert!(
        !port_is_listening(t.port),
        "port {} still listening",
        t.port
    );
    let log = t.log();
    let want_url = format!("http://127.0.0.1:{}/viewer.html", t.port);
    assert!(
        log.iter()
            .any(|e| e["op"] == "goto" && e["url"] == want_url.as_str()),
        "must open {want_url}: {log:?}"
    );
    let visible = log
        .iter()
        .find(|e| e["op"] == "visible" && e["selector"] == "canvas")
        .unwrap_or_else(|| panic!("must wait for the default selector `canvas`: {log:?}"));
    let shot = log
        .iter()
        .find(|e| e["op"] == "screenshot")
        .unwrap_or_else(|| panic!("must take a screenshot: {log:?}"));
    let gap = shot["t"].as_i64().unwrap() - visible["t"].as_i64().unwrap();
    assert!(
        gap >= 1900,
        "the screenshot must come 2000 ms after the selector is visible (gap {gap} ms)"
    );
}

#[test]
fn threejs_single_colour_canvas_is_a_violation() {
    let t = Three::new(None);
    t.p.trust_ok();
    let _g = ServeGuard(&t.p);
    let o = t.check(serde_json::json!({
        "visible": true,
        "screenshot": t.shot(&png_solid(300, 150, [0, 0, 0]))
    }));
    expect_code(&o, 1, "threejs-render with a single-colour canvas");
}

#[test]
fn threejs_selector_never_visible_is_a_violation() {
    let t = Three::new(None);
    t.p.trust_ok();
    let _g = ServeGuard(&t.p);
    let o = t.check(serde_json::json!({"visible": false, "screenshot": t.shot(&drawn_png())}));
    expect_code(&o, 1, "threejs-render whose canvas never becomes visible");
    // R4: the visibility wait is bounded at 30 s — the wait's own `timeout`
    // option, else the page default timeout set before it.
    let log = t.log();
    let wait = log
        .iter()
        .position(|e| e["op"] == "waitForSelector" && e["selector"] == "canvas")
        .unwrap_or_else(|| panic!("must wait for the selector `canvas`: {log:?}"));
    let effective = log[wait]["opts"]["timeout"].as_i64().or_else(|| {
        log[..wait]
            .iter()
            .rev()
            .find(|e| e["op"] == "setDefaultTimeout")
            .and_then(|e| e["ms"].as_i64())
    });
    assert_eq!(
        effective,
        Some(30_000),
        "the selector wait must be limited to 30 s: {log:?}"
    );
}

#[test]
fn threejs_custom_selector_is_used() {
    let t = Three::new(Some("#scene"));
    t.p.trust_ok();
    let _g = ServeGuard(&t.p);
    let o = t.check(serde_json::json!({"visible": true, "screenshot": t.shot(&drawn_png())}));
    expect_code(&o, 0, "threejs-render with selector #scene");
    assert!(
        t.log()
            .iter()
            .any(|e| e["op"] == "visible" && e["selector"] == "#scene"),
        "must wait for `#scene`: {:?}",
        t.log()
    );
}

#[test]
fn threejs_chromium_or_playwright_unavailable_is_exit_2() {
    let t = Three::new(None);
    t.p.trust_ok();
    let _g = ServeGuard(&t.p);
    // Positive control in the same root: a launchable Chromium is clean.
    let o = t.check(serde_json::json!({"visible": true, "screenshot": t.shot(&drawn_png())}));
    expect_code(&o, 0, "positive control: Chromium launches");
    let o = t.check(serde_json::json!({
        "launch_error": "browserType.launch: Executable doesn't exist at /nowhere/chrome"
    }));
    expect_code(&o, 2, "Chromium cannot be launched");

    let t = Three::new(None);
    std::fs::remove_dir_all(t.p.path("node_modules")).unwrap();
    t.p.trust_ok();
    let _g2 = ServeGuard(&t.p);
    let o = t.check(serde_json::json!({"visible": true, "screenshot": t.shot(&drawn_png())}));
    expect_code(&o, 2, "Playwright cannot be resolved from the root");
}

#[test]
fn threejs_leaves_an_already_running_server_running() {
    let t = Three::new(None);
    t.p.trust_ok();
    let _g = ServeGuard(&t.p);
    expect_code(&t.p.run(&["serve", "start"]), 0, "serve start");
    let o = t.check(serde_json::json!({"visible": true, "screenshot": t.shot(&drawn_png())}));
    expect_code(&o, 0, "threejs-render against a running server");
    assert_eq!(
        t.serve_status(),
        "running",
        "a server the check did not start must keep running"
    );
}

// ---------------------------------------------------------------- command

/// A project whose `tool.sh` runs `body` after writing its cwd to a marker.
fn command_project(body: &str) -> Project {
    let p = Project::new();
    p.write_sitecraft_toml("");
    let marker = p.marker("tool-ran");
    write_exec(
        &p.path("tool.sh"),
        &format!(
            "#!/bin/sh\nmkdir -p '{}'\npwd -P > '{}'\n{body}\n",
            marker.parent().unwrap().display(),
            marker.display()
        ),
    );
    let argv0 = p.path("tool.sh").display().to_string();
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_command("conv", &[&argv0], &["tool.sh"])
    ));
    p
}

#[test]
fn command_without_capability_trust_is_not_run() {
    let p = Project::new();
    p.write_sitecraft_toml("");
    // Trust only sitecraft.toml (no capabilities yet), then add the command.
    p.write_decisions(&complete_record());
    p.trust_ok();
    let marker = p.marker("tool-ran");
    write_exec(
        &p.path("tool.sh"),
        &format!(
            "#!/bin/sh\nmkdir -p '{}'\n: > '{}'\nexit 0\n",
            marker.parent().unwrap().display(),
            marker.display()
        ),
    );
    let argv0 = p.path("tool.sh").display().to_string();
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_command("conv", &[&argv0], &["tool.sh"])
    ));
    let o = p.run(&["check", "capability", "conv"]);
    expect_code(&o, 2, "command capability without D2-trust");
    assert!(!marker.exists(), "an untrusted command must not run");
    // Positive control: after trust it runs and is clean.
    p.trust_ok();
    expect_code(&p.run(&["check", "capability", "conv"]), 0, "after trust");
    assert!(marker.exists(), "the trusted command must run");
}

#[test]
fn command_exit_codes_map_to_the_verdict() {
    for (body, want) in [
        ("exit 0", 0),
        ("exit 2", 2),
        ("exit 1", 1),
        ("exit 7", 1),
        ("kill -KILL $$", 1),
    ] {
        let p = command_project(body);
        p.trust_ok();
        let o = p.run(&["check", "capability", "conv"]);
        expect_code(&o, want, &format!("command `{body}`"));
        let cwd = std::fs::read_to_string(p.marker("tool-ran")).expect("the command ran");
        assert_eq!(
            PathBuf::from(cwd.trim()),
            p.root.canonicalize().unwrap(),
            "the command must run with cwd = root"
        );
    }
}

#[test]
fn command_spawn_failure_is_exit_2() {
    let p = command_project("exit 0");
    // Positive control in the same root: the spawnable argv is clean.
    p.trust_ok();
    expect_code(
        &p.run(&["check", "capability", "conv"]),
        0,
        "positive control: spawnable argv",
    );
    let missing = p.base.join("no-such-tool").display().to_string();
    p.write_decisions(&format!(
        "{}{}",
        complete_record(),
        cap_command("conv", &[&missing], &["tool.sh"])
    ));
    p.trust_ok();
    let o = p.run(&["check", "capability", "conv"]);
    expect_code(&o, 2, "argv[0] that cannot be spawned");
}
