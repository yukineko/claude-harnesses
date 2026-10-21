//! CLI contract tests for curate. Each test drives the real binary and asserts
//! the specific observable outcome.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("curate-pc-{tag}-{}-{ns}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn curate(args: &[&str], home: &Path, cwd: Option<&Path>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_curate"));
    c.args(args).env("HOME", home);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.output().expect("binary runs")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Write a store from (ts, title, done_criteria) rows; returns its path.
fn store(dir: &Path, rows: &[(u64, &str, &str)]) -> PathBuf {
    let p = dir.join("store.jsonl");
    let mut s = String::new();
    for (ts, t, dc) in rows {
        s.push_str(&serde_json::json!({"ts": ts, "title": t, "done_criteria": dc}).to_string());
        s.push('\n');
    }
    std::fs::write(&p, s).unwrap();
    p
}

fn dataset_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Promote `title` from a one-entry store; return the single golden.
fn promote_one(tag: &str, title: &str, dc: &str, extra: &[&str]) -> Value {
    let t = Tmp::new(tag);
    let st = store(t.path(), &[(1, title, dc)]);
    let root = t.path().join("root");
    let mut args = vec![
        "promote",
        title,
        "--store",
        st.to_str().unwrap(),
        "--root",
        root.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    let o = curate(&args, t.path(), None);
    assert!(o.status.success(), "stderr: {}", err(&o));
    let mut g = dataset_lines(&root.join("evals/curated/promoted.jsonl"));
    assert_eq!(g.len(), 1);
    g.remove(0)
}

#[test]
fn claim_cargo_test_scoped_golden() {
    let g = promote_one("scoped", "T", "cargo test -p evalkit is green", &[]);
    assert_eq!(
        g["cmd"],
        serde_json::json!(["cargo", "test", "-p", "evalkit"])
    );
    assert_eq!(g["assert"]["exit"], serde_json::json!(0));
    assert!(g.get("draft").is_none());
}

#[test]
fn claim_non_mechanical_draft_describe() {
    let g = promote_one("draft", "T", "auth handles token refresh", &[]);
    assert_eq!(g["draft"], serde_json::json!(true));
    assert!(g.get("cmd").is_none());
    assert_eq!(
        g["describe"],
        "T — TODO assert done_criteria: auth handles token refresh"
    );
}

#[test]
fn claim_go_test_recognised() {
    let g = promote_one("gotest", "T", "go test ./... passes", &[]);
    assert_eq!(g["cmd"], serde_json::json!(["go", "test"]));
}

#[test]
fn claim_candidates_limit_default_20() {
    let t = Tmp::new("cand");
    // 25 entries; even ts -> mechanical, odd ts -> draft. Titles carry the ts.
    let rows: Vec<(u64, String, String)> = (1..=25u64)
        .map(|i| {
            let dc = if i % 2 == 0 {
                "`cargo test` passes"
            } else {
                "prose only"
            };
            (i, format!("entry{i:02}"), dc.to_string())
        })
        .collect();
    let refs: Vec<(u64, &str, &str)> = rows
        .iter()
        .map(|(a, b, c)| (*a, b.as_str(), c.as_str()))
        .collect();
    let st = store(t.path(), &refs);
    let s = st.to_str().unwrap();

    let o = curate(&["candidates", "--store", s], t.path(), None);
    assert!(o.status.success());
    let stdout = out(&o);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 20, "{stdout}");
    for (n, line) in lines.iter().enumerate() {
        let i = 25 - n as u64; // newest first
        let want = if i % 2 == 0 { "mech" } else { "draft" };
        assert!(line.starts_with(want), "line {n}: {line}");
        assert!(line.contains(&format!("entry{i:02}")), "line {n}: {line}");
    }

    // `--limit` is not a flag of `candidates`; the count flag is `--k`.
    let o = curate(
        &["candidates", "--store", s, "--limit", "3"],
        t.path(),
        None,
    );
    assert!(!o.status.success(), "--limit unexpectedly accepted");
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("--limit"), "{}", err(&o));

    let o = curate(&["candidates", "--store", s, "--k", "3"], t.path(), None);
    assert!(o.status.success());
    assert_eq!(out(&o).lines().count(), 3);
}

#[test]
fn claim_promote_draft_flag() {
    let g = promote_one("forcedraft", "T", "`cargo test` passes", &["--draft"]);
    assert_eq!(g["draft"], serde_json::json!(true));
    assert!(g.get("cmd").is_none());
}

#[test]
fn claim_default_store_path() {
    let t = Tmp::new("home");
    let dir = t.path().join(".fugu-router");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("playbooks.jsonl"),
        "{\"ts\":1,\"title\":\"home-default-entry\",\"done_criteria\":\"x\"}\n",
    )
    .unwrap();
    let o = curate(&["candidates"], t.path(), None);
    assert!(o.status.success());
    assert!(out(&o).contains("home-default-entry"), "{}", out(&o));

    // `promote` without --store also reads the HOME default store.
    let root = t.path().join("root");
    let o = curate(
        &["promote", "--latest", "--root", root.to_str().unwrap()],
        t.path(),
        None,
    );
    assert!(o.status.success(), "{}", err(&o));
    let g = dataset_lines(&root.join("evals/curated/promoted.jsonl"));
    assert_eq!(g.len(), 1);
    assert!(g[0]["id"]
        .as_str()
        .unwrap()
        .starts_with("home-default-entry-"));
}

#[test]
fn claim_candidates_store_reads_file() {
    let t = Tmp::new("storeflag");
    let st = store(t.path(), &[(1, "explicit-store-entry", "x")]);
    // HOME has no store, so a hit can only come from --store.
    let o = curate(
        &["candidates", "--store", st.to_str().unwrap()],
        t.path(),
        None,
    );
    assert!(o.status.success());
    assert!(out(&o).contains("explicit-store-entry"), "{}", out(&o));
}

#[test]
fn claim_promote_selector_cli() {
    let t = Tmp::new("selector");
    let st = store(
        t.path(),
        &[
            (10, "add login", "x"),
            (30, "add LOGIN v2", "x"),
            (20, "add login v3", "x"),
            (40, "other", "x"),
        ],
    );
    let root = t.path().join("root");
    let o = curate(
        &[
            "promote",
            "LOGIN",
            "--store",
            st.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
        ],
        t.path(),
        None,
    );
    assert!(o.status.success(), "{}", err(&o));
    let g = dataset_lines(&root.join("evals/curated/promoted.jsonl"));
    assert_eq!(g.len(), 1);
    assert!(g[0]["id"].as_str().unwrap().starts_with("add-login-v2-"));
    assert!(g[0]["describe"]
        .as_str()
        .unwrap()
        .starts_with("add LOGIN v2"));
}

#[test]
fn claim_promote_latest_cli() {
    let t = Tmp::new("latest");
    let st = store(t.path(), &[(10, "a", "x"), (30, "b", "x"), (20, "c", "x")]);
    let root = t.path().join("root");
    let o = curate(
        &[
            "promote",
            "--latest",
            "--store",
            st.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
        ],
        t.path(),
        None,
    );
    assert!(o.status.success(), "{}", err(&o));
    let g = dataset_lines(&root.join("evals/curated/promoted.jsonl"));
    assert_eq!(g.len(), 1);
    assert!(g[0]["id"].as_str().unwrap().starts_with("b-"), "{}", g[0]);
    assert!(g[0]["describe"].as_str().unwrap().starts_with("b "));
}

#[test]
fn claim_dataset_default_promoted() {
    let t = Tmp::new("dsdefault");
    let st = store(t.path(), &[(1, "t", "x")]);
    let root = t.path().join("root");
    let o = curate(
        &[
            "promote",
            "t",
            "--store",
            st.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
        ],
        t.path(),
        None,
    );
    assert!(o.status.success());
    assert!(root.join("evals/curated/promoted.jsonl").is_file());
}

#[test]
fn claim_dedup_by_case_id() {
    let t = Tmp::new("dedup");
    let st = store(t.path(), &[(1, "same title", "x")]);
    let root = t.path().join("root");
    let args = [
        "promote",
        "same title",
        "--store",
        st.to_str().unwrap(),
        "--root",
        root.to_str().unwrap(),
    ];
    let o1 = curate(&args, t.path(), None);
    assert!(o1.status.success());
    let o2 = curate(&args, t.path(), None);
    assert_eq!(o2.status.code(), Some(0));
    assert!(err(&o2).contains("already promoted"), "{}", err(&o2));
    let n = std::fs::read_to_string(root.join("evals/curated/promoted.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(n, 1);

    // Same title, different done_criteria: the key is the title-derived id.
    let d2 = t.path().join("second");
    std::fs::create_dir_all(&d2).unwrap();
    let st2 = store(&d2, &[(2, "same title", "`cargo test` passes")]);
    let o3 = curate(
        &[
            "promote",
            "same title",
            "--store",
            st2.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
        ],
        t.path(),
        None,
    );
    assert_eq!(o3.status.code(), Some(0));
    assert!(err(&o3).contains("already promoted"), "{}", err(&o3));
    assert_eq!(
        dataset_lines(&root.join("evals/curated/promoted.jsonl")).len(),
        1
    );
}

#[test]
fn claim_promote_appends() {
    let t = Tmp::new("append");
    let st = store(t.path(), &[(1, "first one", "x"), (2, "second one", "x")]);
    let root = t.path().join("root");
    for title in ["first one", "second one"] {
        let o = curate(
            &[
                "promote",
                title,
                "--store",
                st.to_str().unwrap(),
                "--root",
                root.to_str().unwrap(),
            ],
            t.path(),
            None,
        );
        assert!(o.status.success(), "{}", err(&o));
    }
    let g = dataset_lines(&root.join("evals/curated/promoted.jsonl"));
    assert_eq!(g.len(), 2);
    assert!(g[0]["id"].as_str().unwrap().starts_with("first-one-"));
    assert!(g[1]["id"].as_str().unwrap().starts_with("second-one-"));
}

#[test]
fn claim_evals_dir_flag() {
    let t = Tmp::new("evalsdir");
    let st = store(t.path(), &[(1, "t", "x")]);
    let root = t.path().join("root");
    let o = curate(
        &[
            "promote",
            "t",
            "--store",
            st.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
            "--evals-dir",
            "foo",
        ],
        t.path(),
        None,
    );
    assert!(o.status.success(), "{}", err(&o));
    assert!(root.join("foo/curated/promoted.jsonl").is_file());
    assert!(!root.join("evals").exists());
}

#[test]
fn claim_root_defaults_to_cwd() {
    let t = Tmp::new("cwd");
    let st = store(t.path(), &[(1, "t", "x")]);
    let cwd = t.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let o = curate(
        &["promote", "t", "--store", st.to_str().unwrap()],
        t.path(),
        Some(&cwd),
    );
    assert!(o.status.success(), "{}", err(&o));
    assert!(cwd.join("evals/curated/promoted.jsonl").is_file());
}
