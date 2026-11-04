//! Spec `sitecraft-capabilities` R10 — `api_paths` in `sitecraft.toml` and
//! the Vite + Hono SKILL step (rulings D10).
//!
//! # Surface assumed (rulings D10)
//!
//! * `sitecraft.toml` accepts the optional top-level key `api_paths`
//!   (array of strings starting with `/`, default empty). An element not
//!   starting with `/` makes the config unusable (`check pages` exit 2,
//!   nothing spawned). Trust stays over the whole file (changing
//!   `api_paths` after `sitecraft trust` makes `serve start` exit 2 until
//!   trusted again).
//! * `sitecraft --root <root> check pages` (pages part via the `SITECRAFT_NODE`
//!   stub reporting a clean run) additionally sends `GET <path>` to
//!   `http://127.0.0.1:<port>` for each `api_paths` entry, without following
//!   redirects: any non-2xx (a 302 included) or a connection failure → exit
//!   1; one path taking more than 10 s → exit 2. All 2xx → exit 0.
//!   The in-test listener records the request line (`GET <path> HTTP/1.x`).
//! * SKILL.md has the Vite + Hono (`@hono/vite-dev-server`) one-dev-command
//!   step naming `vite.config.ts` and `server/app.ts`.
//!
//! `api_path_without_leading_slash_is_unusable` carries a same-root positive
//! control (the `/`-prefixed form is clean), because at HEAD the unknown
//! key alone already gives exit 2.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod spec_common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spec_common::*;

/// Accepts on `port`; answers each request by path:
/// `/api/hang` never answers, `/api/redirect` 302, `/api/missing` 404,
/// `/api/error` 500, anything else 200. Returns the recorded request lines.
fn api_server(port: u16) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let l = TcpListener::bind(("127.0.0.1", port)).expect("bind api listener");
    let rec = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let rec = Arc::clone(&rec);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                }
                rec.lock().unwrap().push(line.trim().to_string());
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let resp = match path.as_str() {
                    "/api/hang" => {
                        std::thread::sleep(Duration::from_secs(120));
                        return;
                    }
                    "/api/redirect" => {
                        "HTTP/1.1 302 Found\r\nLocation: /api/ok\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    }
                    "/api/missing" => {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    }
                    "/api/error" => {
                        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    }
                    _ => "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                };
                let _ = s.write_all(resp.as_bytes());
            });
        }
    });
    seen
}

/// A project whose `sitecraft.toml` has `api_paths` and whose node stub
/// reports a clean single-page run. Returns (project, port, node stub).
fn project(api_paths: &str) -> (Project, u16, std::path::PathBuf) {
    let p = Project::new();
    let port = p.write_sitecraft_toml(&format!("api_paths = {api_paths}\n"));
    let node = p.base.join("bin/node-stub");
    write_exec(
        &node,
        &format!(
            "#!/bin/sh\nmkdir -p '{m}'\n: > '{m}/node-ran'\necho '{{\"schema\":\"sitecraft-check-pages/1\",\"status\":\"ok\",\"pages_checked\":1,\"findings\":[]}}'\nexit 0\n",
            m = p.base.join("markers").display()
        ),
    );
    (p, port, node)
}

fn check_pages(p: &Project, node: &std::path::Path) -> Out {
    p.run_env(&["check", "pages"], &[("SITECRAFT_NODE", os(node))])
}

#[test]
fn api_paths_all_2xx_is_clean() {
    let (p, port, node) = project("[\"/api/health\", \"/api/items?limit=1\"]");
    let seen = api_server(port);
    let o = check_pages(&p, &node);
    expect_code(&o, 0, "check pages with 2xx api_paths");
    let seen = seen.lock().unwrap().clone();
    for path in ["/api/health", "/api/items?limit=1"] {
        assert!(
            seen.iter()
                .any(|l| l.starts_with(&format!("GET {path} HTTP/1."))),
            "expected `GET {path}` on 127.0.0.1:{port}; saw {seen:?}"
        );
    }
}

#[test]
fn api_path_without_leading_slash_is_unusable() {
    let (p, port, node) = project("[\"api/health\"]");
    let o = check_pages(&p, &node);
    expect_code(&o, 2, "api_paths element without a leading /");
    assert!(
        !p.base.join("markers/node-ran").exists(),
        "nothing may be checked with an unusable config"
    );
    // Positive control in the same root: the `/`-prefixed form is usable, so
    // the exit 2 above is about the missing `/`, not about the key itself.
    let cfg = std::fs::read_to_string(p.path("sitecraft.toml")).unwrap();
    p.write(
        "sitecraft.toml",
        cfg.replace("[\"api/health\"]", "[\"/api/health\"]"),
    );
    let _seen = api_server(port);
    expect_code(
        &check_pages(&p, &node),
        0,
        "positive control: api_paths = [\"/api/health\"]",
    );
}

#[test]
fn non_2xx_api_paths_are_violations() {
    for path in ["/api/missing", "/api/error", "/api/redirect"] {
        let (p, port, node) = project(&format!("[\"/api/ok\", \"{path}\"]"));
        let seen = api_server(port);
        let o = check_pages(&p, &node);
        expect_code(&o, 1, &format!("check pages with {path}"));
        let seen = seen.lock().unwrap().clone();
        assert!(
            seen.iter()
                .any(|l| l.starts_with(&format!("GET {path} HTTP/1."))),
            "{path} must be requested: {seen:?}"
        );
        // The redirect target is /api/ok, which is also listed once: a
        // followed redirect would request it twice.
        let ok_hits = seen
            .iter()
            .filter(|l| l.starts_with("GET /api/ok HTTP/1."))
            .count();
        assert_eq!(
            ok_hits, 1,
            "{path}: redirects must not be followed: {seen:?}"
        );
    }
}

#[test]
fn unreachable_api_server_is_a_violation() {
    // Nothing listens on the configured port.
    let (p, _port, node) = project("[\"/api/health\"]");
    let o = check_pages(&p, &node);
    expect_code(&o, 1, "check pages with no API listener");
}

#[test]
fn api_path_slower_than_10_seconds_is_exit_2() {
    let (p, port, node) = project("[\"/api/hang\"]");
    let seen = api_server(port);
    let o = check_pages(&p, &node);
    expect_code(&o, 2, "check pages with an API path that never answers");
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter().any(|l| l.starts_with("GET /api/hang HTTP/1.")),
        "the exit 2 must come from the unanswered GET /api/hang: {seen:?}\n{}",
        o.describe()
    );
    assert!(
        o.elapsed >= Duration::from_secs(9) && o.elapsed < Duration::from_secs(40),
        "the per-path limit is 10 s, took {:?}",
        o.elapsed
    );
}

#[test]
fn api_paths_stay_under_whole_file_trust() {
    let (p, _port, _node) = project("[\"/api/health\"]");
    let _g = ServeGuard(&p);
    p.trust_ok();
    expect_code(
        &p.run(&["serve", "start"]),
        0,
        "serve start with trusted api_paths",
    );
    expect_code(&p.run(&["serve", "stop"]), 0, "serve stop");
    let cfg = std::fs::read_to_string(p.path("sitecraft.toml")).unwrap();
    p.write(
        "sitecraft.toml",
        cfg.replace(
            "api_paths = [\"/api/health\"]",
            "api_paths = [\"/api/other\"]",
        ),
    );
    let o = p.run(&["serve", "start"]);
    expect_code(&o, 2, "serve start after api_paths changed");
    assert!(o.all().contains("sitecraft trust"), "{}", o.describe());
}

#[test]
fn skill_has_the_vite_hono_one_command_step() {
    let s = skill_text();
    for needle in ["@hono/vite-dev-server", "vite.config.ts", "server/app.ts"] {
        assert!(s.contains(needle), "SKILL.md must mention `{needle}`");
    }
    assert_paragraph_with(
        &s,
        &["Vite", "Hono"],
        "R10: one step describing Vite + Hono together",
    );
}
