//! CLI contract tests for deepwiki. Each test drives the real binary (or
//! inspects git packaging state) in isolated temp dirs with a temp HOME.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

struct Out {
    code: i32,
    stdout: String,
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn dw(cwd: &Path, home: &Path, args: &[&str]) -> Out {
    let out = Command::new(env!("CARGO_BIN_EXE_deepwiki"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("binary runs");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn commit_all(dir: &Path, home: &Path, msg: &str) {
    git(dir, home, &["add", "-A"]);
    git(dir, home, &["commit", "-q", "-m", msg]);
}

fn git_repo(home: &Path) -> TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), home, &["init", "-q"]);
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    commit_all(d.path(), home, "A");
    d
}

fn head(dir: &Path, home: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn claim_status_fresh_stale() {
    let home = tempfile::tempdir().unwrap();
    let repo = git_repo(home.path());
    let r = repo.path();
    assert_eq!(dw(r, home.path(), &["stamp", "overview.md"]).code, 0);
    let fresh = dw(r, home.path(), &["status"]);
    assert_eq!(fresh.code, 0);
    assert!(fresh.stdout.contains("✅ fresh"), "got: {}", fresh.stdout);
    assert!(!fresh.stdout.contains("⚠ stale"), "got: {}", fresh.stdout);

    std::fs::write(r.join("src/lib.rs"), "pub fn a() {}\npub fn b() {}\n").unwrap();
    std::fs::create_dir_all(r.join("docs")).unwrap();
    std::fs::write(r.join("docs/notes.md"), "notes\n").unwrap();
    commit_all(r, home.path(), "B");
    let stale = dw(r, home.path(), &["status"]);
    assert!(stale.stdout.contains("⚠ stale"), "got: {}", stale.stdout);
    assert!(!stale.stdout.contains("✅ fresh"), "got: {}", stale.stdout);
    assert!(
        stale.stdout.lines().any(|l| l.trim() == "src/lib.rs"),
        "got: {}",
        stale.stdout
    );
    assert!(
        !stale.stdout.lines().any(|l| l.trim() == "docs/notes.md"),
        "non-source file listed: {}",
        stale.stdout
    );
}

#[test]
fn claim_scan_map() {
    let home = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    std::fs::create_dir_all(r.join("src")).unwrap();
    std::fs::write(r.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(r.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    std::fs::write(r.join("README.md"), "# x\n").unwrap();

    let md = dw(r, home.path(), &["scan"]);
    assert_eq!(md.code, 0);
    let s = &md.stdout;
    let section = |name: &str| -> String {
        let marker = format!("## {name}\n");
        let start = s
            .find(&marker)
            .unwrap_or_else(|| panic!("missing section {name}: {s}"));
        let rest = &s[start + marker.len()..];
        rest.split("\n## ").next().unwrap().to_string()
    };
    assert!(section("languages").contains("- Rust: 1 files"), "{s}");
    assert!(section("top-level layout").contains("[dir ] src"), "{s}");
    assert!(section("entry points").contains("- src/main.rs"), "{s}");
    assert!(section("key files").contains("- Cargo.toml"), "{s}");

    let js = dw(r, home.path(), &["scan", "--json"]);
    assert_eq!(js.code, 0);
    let v: serde_json::Value = serde_json::from_str(&js.stdout).expect("parseable JSON");
    assert_eq!(v["languages"]["Rust"]["files"], 1);
    assert_eq!(v["entry_points"][0], "src/main.rs");
    assert_eq!(v["key_files"][0], "Cargo.toml");
    assert!(v["top_level"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["name"] == "src" && e["is_dir"] == true));
}

#[test]
fn claim_stamp_records() {
    let home = tempfile::tempdir().unwrap();
    let repo = git_repo(home.path());
    let r = repo.path();
    let o = dw(r, home.path(), &["stamp", "overview.md", "arch.md"]);
    assert_eq!(o.code, 0);
    let text = std::fs::read_to_string(r.join(".deepwiki/_meta.toml")).expect("_meta.toml");
    let v: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(v["sha"].as_str().unwrap(), head(r, home.path()));
    let pages: Vec<&str> = v["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(pages, ["overview.md", "arch.md"]);
}

#[test]
fn claim_root_flag() {
    let home = tempfile::tempdir().unwrap();
    let a = git_repo(home.path());
    let b = tempfile::tempdir().unwrap();
    std::fs::write(b.path().join("only_in_b.rs"), "fn x() {}\n").unwrap();
    std::fs::write(a.path().join("only_in_a.rs"), "fn x() {}\n").unwrap();
    let (pa, pb) = (a.path().to_str().unwrap(), b.path());

    // scan
    let with = dw(pb, home.path(), &["scan", "--root", pa]);
    assert!(with.stdout.contains("only_in_a.rs"), "{}", with.stdout);
    assert!(!with.stdout.contains("only_in_b.rs"), "{}", with.stdout);
    let without = dw(pb, home.path(), &["scan"]);
    assert!(
        without.stdout.contains("only_in_b.rs"),
        "{}",
        without.stdout
    );
    assert!(!without.stdout.contains("only_in_a.rs"));

    // init
    assert_eq!(dw(pb, home.path(), &["init", "--root", pa]).code, 0);
    assert!(a.path().join(".deepwiki").is_dir());
    assert!(!b.path().join(".deepwiki").exists());
    assert_eq!(dw(pb, home.path(), &["init"]).code, 0);
    assert!(b.path().join(".deepwiki").is_dir());
    std::fs::remove_dir_all(a.path().join(".deepwiki")).unwrap();
    std::fs::remove_dir_all(b.path().join(".deepwiki")).unwrap();

    // stamp
    assert_eq!(
        dw(pb, home.path(), &["stamp", "x.md", "--root", pa]).code,
        0
    );
    assert!(a.path().join(".deepwiki/_meta.toml").is_file());
    assert!(!b.path().join(".deepwiki").exists());
    assert_eq!(dw(pb, home.path(), &["stamp", "y.md"]).code, 0);
    assert!(b.path().join(".deepwiki/_meta.toml").is_file());

    // status: A is stamped (fresh), B has a stamp with empty sha (not a git repo)
    let sa = dw(pb, home.path(), &["status", "--root", pa]);
    assert!(sa.stdout.contains("✅ fresh"), "{}", sa.stdout);
    let sb = dw(pb, home.path(), &["status"]);
    assert!(sb.stdout.contains("not a git repo"), "{}", sb.stdout);
}

#[test]
fn claim_launcher_tracked_bins_ignored() {
    let m = manifest();
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&m)
            .args(args)
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git runs")
    };
    let ls = run(&["ls-files", "-s", "bin"]);
    assert!(ls.status.success());
    let ls = String::from_utf8_lossy(&ls.stdout).into_owned();
    let lines: Vec<&str> = ls.lines().collect();
    assert_eq!(lines.len(), 1, "tracked bin entries: {ls}");
    assert!(lines[0].starts_with("100755 "), "{ls}");
    assert!(lines[0].ends_with("\tbin/deepwiki"), "{ls}");

    for tag in [
        "darwin-arm64",
        "darwin-x86_64",
        "linux-arm64",
        "linux-x86_64",
    ] {
        let path = format!("bin/deepwiki-{tag}");
        let o = run(&["check-ignore", "-q", &path]);
        assert_eq!(o.status.code(), Some(0), "{path} not ignored");
        let v = run(&["check-ignore", "-v", &path]);
        let line = String::from_utf8_lossy(&v.stdout).into_owned();
        let (src, _) = line.split_once('\t').expect("-v output");
        let mut parts = src.splitn(3, ':');
        assert_eq!(parts.next(), Some(".gitignore"), "{line}");
        parts.next();
        assert_eq!(
            parts.next(),
            Some(format!("crates/*/bin/*-{tag}").as_str()),
            "{line}"
        );
    }
    // negative control: the tracked launcher is not ignored
    assert_eq!(
        run(&["check-ignore", "-q", "bin/deepwiki"]).status.code(),
        Some(1)
    );
}

/// Tracked paths (as `git ls-files` prints them, relative to `repo`) under
/// `bin_rel`. A failing git fails the test; it never yields an empty list.
fn tracked_bin_entries(repo: &Path, bin_rel: &str) -> Vec<String> {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-files", "--", bin_rel])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git ls-files {bin_rel} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Violations of the claim "the launcher is the only tracked file in bin".
fn bin_claim_violations(entries: &[String], launcher: &str) -> Vec<String> {
    let mut v = Vec::new();
    if !entries.iter().any(|e| e == launcher) {
        v.push(format!("launcher {launcher} not tracked: {entries:?}"));
    }
    for e in entries {
        if e != launcher {
            v.push(format!("unexpected tracked entry {e}"));
        }
        if e.rsplit('/').next().unwrap_or("").starts_with("deepwiki-") {
            v.push(format!("platform binary tracked: {e}"));
        }
    }
    v
}

fn fixture_bin_repo(track_darwin: bool, track_launcher: bool) -> TempDir {
    let home = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    git(d.path(), home.path(), &["init", "-q"]);
    std::fs::create_dir_all(d.path().join("bin")).unwrap();
    std::fs::write(d.path().join("bin/deepwiki"), "#!/bin/sh\n").unwrap();
    std::fs::write(d.path().join("bin/deepwiki-darwin-arm64"), "BIN").unwrap();
    if track_launcher {
        git(d.path(), home.path(), &["add", "-f", "bin/deepwiki"]);
    }
    if track_darwin {
        git(
            d.path(),
            home.path(),
            &["add", "-f", "bin/deepwiki-darwin-arm64"],
        );
    }
    d
}

#[test]
fn claim_platform_bins_untracked() {
    let launcher = "bin/deepwiki";

    // negative controls: the helper must see a tracked platform binary and
    // an empty tracked list, and the predicate must reject both.
    let bad = fixture_bin_repo(true, true);
    let entries = tracked_bin_entries(bad.path(), "bin");
    assert!(
        entries.iter().any(|e| e == "bin/deepwiki-darwin-arm64"),
        "helper cannot see tracked binary: {entries:?}"
    );
    assert!(!bin_claim_violations(&entries, launcher).is_empty());
    let none = fixture_bin_repo(false, false);
    let entries = tracked_bin_entries(none.path(), "bin");
    assert!(entries.is_empty(), "{entries:?}");
    assert!(!bin_claim_violations(&entries, launcher).is_empty());
    let ok = fixture_bin_repo(false, true);
    let entries = tracked_bin_entries(ok.path(), "bin");
    assert_eq!(
        bin_claim_violations(&entries, launcher),
        Vec::<String>::new()
    );

    // the real crate
    let real = manifest();
    let entries = tracked_bin_entries(&real, "bin");
    assert_eq!(
        bin_claim_violations(&entries, launcher),
        Vec::<String>::new(),
        "tracked: {entries:?}"
    );
    assert_eq!(entries, vec![launcher.to_string()]);
}
