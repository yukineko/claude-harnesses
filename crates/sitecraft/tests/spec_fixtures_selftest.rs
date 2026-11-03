//! Self-tests of the spec-test fixtures (fake Blender, fake `bpy`, fake
//! Playwright, image / GLB builders). These are NOT spec tests: they must
//! pass at every revision, and they exist so that a RED spec test can be
//! trusted to be RED because sitecraft lacks the behaviour, not because a
//! fixture is broken. They run the sitecraft binary only for behaviour that
//! already exists before this spec (serve start/stop, the gate on a
//! purpose-less record).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::process::Command;
use std::time::Duration;

use serde_json::Value;
use spec_common::*;

fn glb_ok(b: &[u8]) -> bool {
    b.len() >= 12
        && &b[0..4] == b"glTF"
        && u32::from_le_bytes(b[4..8].try_into().unwrap()) == 2
        && u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize == b.len()
}

#[test]
fn glb_builders_match_the_d4_condition() {
    assert!(glb_ok(&glb_valid()));
    assert!(!glb_ok(&glb_bad_magic()));
    assert!(!glb_ok(&glb_with_version(1)));
    assert!(!glb_ok(&glb_bad_length()));
    assert!(!glb_ok(&[]));
}

#[test]
fn png_builders_decode_to_the_intended_pixels() {
    let bytes = png_with_drawn(100, [0, 0, 0, 255]);
    let dec = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut buf).unwrap();
    assert_eq!((info.width, info.height), (100, 100));
    let black = buf[..info.buffer_size()]
        .chunks(4)
        .filter(|p| p[0] == 0 && p[1] == 0 && p[2] == 0)
        .count();
    assert_eq!(black, 100, "exactly 1% drawn");
    // Corners are white.
    for (x, y) in [(0usize, 0usize), (99, 0), (0, 99), (99, 99)] {
        let i = (y * 100 + x) * 4;
        assert_eq!(&buf[i..i + 3], &[255, 255, 255]);
    }
    assert!(png::Decoder::new(std::io::Cursor::new(png_raw_dims(0, 10)))
        .read_info()
        .into_err_or_zero());
}

trait ErrOrZero {
    fn into_err_or_zero(self) -> bool;
}

impl<R: std::io::BufRead + std::io::Seek> ErrOrZero for Result<png::Reader<R>, png::DecodingError> {
    fn into_err_or_zero(self) -> bool {
        match self {
            Err(_) => true,
            Ok(r) => r.info().width == 0 || r.info().height == 0,
        }
    }
}

#[test]
fn fake_blender_runs_a_bpy_script_and_exports_a_valid_glb() {
    let p = Project::new();
    let blender = p.fake_blender();
    let script = p.base.join("probe.py");
    write(
        &script,
        "import bpy, sys\nbpy.ops.wm.read_factory_settings(use_empty=False)\nbpy.context.scene.render.engine = 'BLENDER_EEVEE'\nfor o in bpy.data.objects:\n    pass\nbpy.ops.export_scene.gltf(filepath=sys.argv[-1], export_format='GLB')\n",
    );
    let out_glb = p.state.join("probe-tmp/x.glb");
    let log = p.base.join("blender-log.jsonl");
    let mut c = Command::new(&blender);
    c.args(["--background", "--factory-startup", "--python"])
        .arg(&script)
        .arg("--")
        .arg(&out_glb)
        .current_dir(&p.root)
        .env("FAKE_BLENDER_LOG", &log);
    let o = run_deadline(c, None, Duration::from_secs(30));
    expect_code(&o, 0, "fake blender");
    assert!(
        glb_ok(&std::fs::read(&out_glb).unwrap()),
        "fake bpy must write a valid GLB"
    );
    let line: Value = serde_json::from_str(
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(line["argv"][0], "--background");
    assert_eq!(
        std::path::PathBuf::from(line["cwd"].as_str().unwrap()),
        p.root.canonicalize().unwrap()
    );

    let mut c = Command::new(&blender);
    c.args(["--background", "--python"])
        .arg(&script)
        .arg("--")
        .arg(&out_glb)
        .env("FAKE_BLENDER_EXIT", "1");
    expect_code(
        &run_deadline(c, None, Duration::from_secs(30)),
        1,
        "FAKE_BLENDER_EXIT=1",
    );

    let bad = p.state.join("probe-tmp/bad.glb");
    let mut c = Command::new(&blender);
    c.args(["--background", "--python"])
        .arg(&script)
        .arg("--")
        .arg(&bad)
        .env("FAKE_BPY_GLB", "bad-magic");
    expect_code(
        &run_deadline(c, None, Duration::from_secs(30)),
        0,
        "bad-magic export",
    );
    assert!(!glb_ok(&std::fs::read(&bad).unwrap()));
}

#[test]
fn fake_blender_passes_args_after_the_double_dash_to_the_script() {
    let p = Project::new();
    let blender = p.fake_blender();
    let script = p.base.join("args.py");
    write(
        &script,
        "import sys\na = sys.argv[sys.argv.index('--') + 1:]\nopen(a[a.index('--glb') + 1], 'w').write('g')\nopen(a[a.index('--render') + 1], 'w').write('r')\n",
    );
    let mut c = Command::new(&blender);
    c.args(["--background", "--factory-startup", "--python"])
        .arg(&script)
        .args(["--", "--glb"])
        .arg(p.base.join("o.glb"))
        .arg("--render")
        .arg(p.base.join("o.png"));
    expect_code(
        &run_deadline(c, None, Duration::from_secs(30)),
        0,
        "args script",
    );
    assert_eq!(std::fs::read_to_string(p.base.join("o.glb")).unwrap(), "g");
    assert_eq!(std::fs::read_to_string(p.base.join("o.png")).unwrap(), "r");
}

/// A plausible client of the fake Playwright: resolves it from the root
/// like `check pages`, probes WebGL, waits for a selector, waits 2000 ms,
/// screenshots it, and prints what it saw.
const CLIENT: &str = r#"import { createRequire } from 'node:module';
import path from 'node:path';
const pw = createRequire(path.join(process.cwd(), '__x__.cjs'))('playwright');
const out = {};
try {
  const browser = await pw.chromium.launch({ headless: true });
  const page = await browser.newPage();
  out.webgl = await page.evaluate(() => {
    const c = document.createElement('canvas');
    return c.getContext('webgl2') ? 'webgl2' : c.getContext('webgl') ? 'webgl' : 'none';
  });
  await page.goto('http://127.0.0.1:1/viewer.html', { waitUntil: 'load' });
  try {
    await page.locator('canvas').first().waitFor({ state: 'visible', timeout: 30000 });
    await page.waitForTimeout(2000);
    const buf = await page.locator('canvas').first().screenshot({ type: 'png' });
    out.bytes = buf.length;
  } catch (e) {
    out.wait_error = e.name;
  }
  await browser.close();
} catch (e) {
  out.launch_error = String(e.message);
}
console.log(JSON.stringify(out));
"#;

fn run_client(p: &Project, scenario: Value) -> Value {
    let client = p.base.join("client.mjs");
    write(&client, CLIENT);
    let mut c = Command::new(which("node"));
    c.arg(&client)
        .current_dir(&p.root)
        .env("FAKE_PLAYWRIGHT_SCENARIO", p.scenario(&scenario))
        .env("FAKE_PLAYWRIGHT_LOG", p.base.join("pw-log.jsonl"));
    let o = run_deadline(c, None, Duration::from_secs(30));
    expect_code(&o, 0, "node client");
    serde_json::from_str(o.stdout.trim()).unwrap_or_else(|e| panic!("{e}: {}", o.describe()))
}

#[test]
fn fake_playwright_serves_the_documented_surface() {
    let p = Project::new();
    p.install_fake_playwright();
    let shot = p.base.join("shot.png");
    let png = png_with_drawn(150, [0, 0, 0, 255]);
    write(&shot, &png);
    let v = run_client(
        &p,
        serde_json::json!({"webgl": "webgl2", "visible": true, "screenshot": shot.display().to_string()}),
    );
    assert_eq!(v["webgl"], "webgl2", "{v}");
    assert_eq!(v["bytes"], png.len(), "{v}");
    let log: Vec<Value> = std::fs::read_to_string(p.base.join("pw-log.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let vis = log.iter().find(|e| e["op"] == "visible").unwrap()["t"]
        .as_i64()
        .unwrap();
    let sh = log.iter().find(|e| e["op"] == "screenshot").unwrap()["t"]
        .as_i64()
        .unwrap();
    assert!(
        sh - vis >= 1900,
        "waitForTimeout must really wait: {}",
        sh - vis
    );

    let v = run_client(&p, serde_json::json!({"webgl": "webgl"}));
    assert_eq!(v["webgl"], "webgl", "{v}");
    let v = run_client(&p, serde_json::json!({"webgl": "none", "visible": false}));
    assert_eq!(v["webgl"], "none", "{v}");
    assert_eq!(v["wait_error"], "TimeoutError", "{v}");
    let v = run_client(
        &p,
        serde_json::json!({"launch_error": "Executable doesn't exist at /x"}),
    );
    assert!(
        v["launch_error"]
            .as_str()
            .unwrap()
            .contains("Executable doesn't exist"),
        "{v}"
    );
}

/// Reference reading of the D4 pixel rule, used only to prove the R4 image
/// fixtures mean what the R4 tests claim. `None` = not decodable.
fn d4_clean(bytes: &[u8]) -> Option<bool> {
    let mut r = png::Decoder::new(std::io::Cursor::new(bytes.to_vec()))
        .read_info()
        .ok()?;
    let mut buf = vec![0; r.output_buffer_size()?];
    let info = r.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    if w == 0 || h == 0 {
        return Some(false);
    }
    assert_eq!(info.color_type, png::ColorType::Rgba, "fixtures are RGBA8");
    let px = |x: usize, y: usize| {
        let i = (y * w + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2]]
    };
    let corners = [px(0, 0), px(w - 1, 0), px(0, h - 1), px(w - 1, h - 1)];
    let count = |c: [u8; 3]| corners.iter().filter(|k| **k == c).count();
    let best = corners.iter().map(|c| count(*c)).max().unwrap();
    let mut cands: Vec<[u8; 3]> = corners
        .iter()
        .copied()
        .filter(|c| count(*c) == best)
        .collect();
    cands.dedup();
    let ratio = |bg: [u8; 3]| {
        let mut drawn = 0usize;
        for y in 0..h {
            for x in 0..w {
                let p = px(x, y);
                if (0..3).any(|i| (i32::from(p[i]) - i32::from(bg[i])).abs() > 16) {
                    drawn += 1;
                }
            }
        }
        drawn as f64 / (w * h) as f64
    };
    let min = cands
        .iter()
        .map(|c| ratio(*c))
        .fold(f64::INFINITY, f64::min);
    Some(min >= 0.01)
}

#[test]
fn r4_image_fixtures_mean_what_the_r4_tests_claim() {
    assert_eq!(d4_clean(&png_solid(64, 64, [255, 255, 255])), Some(false));
    assert_eq!(d4_clean(&png_with_drawn(100, [0, 0, 0, 255])), Some(true));
    assert_eq!(d4_clean(&png_with_drawn(99, [0, 0, 0, 255])), Some(false));
    assert_eq!(
        d4_clean(&png_with_drawn(150, [239, 239, 239, 255])),
        Some(false)
    );
    assert_eq!(
        d4_clean(&png_with_drawn(150, [238, 238, 238, 255])),
        Some(true)
    );
    assert_eq!(
        d4_clean(&png_with_drawn(150, [255, 255, 238, 255])),
        Some(true)
    );
    assert_eq!(
        d4_clean(&png_with_drawn(150, [255, 255, 255, 0])),
        Some(false)
    );
    let tie = png_rgba(100, 100, |x, y| {
        if x == 0 && (y == 0 || y == 99) {
            [0, 0, 0, 255]
        } else {
            [255, 255, 255, 255]
        }
    });
    assert_eq!(
        d4_clean(&tie),
        Some(false),
        "tie → smallest ratio → violation"
    );
    let four = png_rgba(100, 100, |x, y| match (x, y) {
        (99, 0) => [255, 0, 0, 255],
        (0, 99) => [0, 255, 0, 255],
        (99, 99) => [0, 0, 255, 255],
        _ => [128, 128, 128, 255],
    });
    assert_eq!(
        d4_clean(&four),
        Some(false),
        "4 distinct → smallest ratio → violation"
    );
    assert_ne!(d4_clean(&png_raw_dims(0, 10)), Some(true));
}

/// The dev-server stub and `GateProject` baseline work with behaviour that
/// already exists (serve start/stop, the gate on a purpose-less record).
#[test]
fn dev_server_stub_and_gate_baseline_work_at_head_surface() {
    let p = Project::new();
    let port = p.write_sitecraft_toml("");
    p.trust_ok();
    let _g = ServeGuard(&p);
    expect_code(&p.run(&["serve", "start"]), 0, "serve start with the stub");
    assert!(port_is_listening(port), "the stub must listen on {port}");
    expect_code(&p.run(&["serve", "stop"]), 0, "serve stop");

    let g = GateProject::new("");
    g.p.write_decisions(&areas_all_irrelevant());
    g.p.trust_ok();
    let o = g.gate("selftest", &[]);
    assert_allow(
        &o,
        "baseline gate on a purpose-less complete record (current surface)",
    );
    assert!(
        g.p.marker("npm-ran").exists(),
        "check security ran the npm stub"
    );
    assert!(
        !g.p.marker("node-ran").exists(),
        "no web edits: pages must not run"
    );
}
