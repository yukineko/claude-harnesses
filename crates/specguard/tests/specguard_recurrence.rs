// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(dead_code)] // fixture helpers shared with backlog_89544915.rs; not every one is used here
                     // RECURRENCE (claim from the 89544915 implementer, tested black-box): a
                     // structural finding `specguard:<kind>:<key>` has no episode id. After
                     // `reconcile-findings` closes it `resolved`, if the same gap returns, a fresh
                     // `specguard audit` must surface a NEW OPEN finding in `overwatch review-queue
                     // --json`. record-audit solved this with episode ids; specguard must too.
                     // Independent test (author != implementer).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fx {
    repo: PathBuf,
    home: PathBuf,
    // keep the tempdir alive
    _tmp: tempfile::TempDir,
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

fn write(repo: &Path, rel: &str, body: &str) {
    let p = repo.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

const MAP_BOTH_UNTESTED: &str = r#"last_synced = ""

[entries.alpha]
key = "alpha"
kind = "feature"
spec_doc = "docs/a.md"
impl_files = ["src/a.rs"]
test_files = []

[entries.beta]
key = "beta"
kind = "feature"
spec_doc = "docs/b.md"
impl_files = ["src/b.rs"]
test_files = []
"#;

/// alpha now has a test file (fixed); beta is still untested.
const MAP_ALPHA_FIXED: &str = r#"last_synced = ""

[entries.alpha]
key = "alpha"
kind = "feature"
spec_doc = "docs/a.md"
impl_files = ["src/a.rs"]
test_files = ["tests/a.rs"]

[entries.beta]
key = "beta"
kind = "feature"
spec_doc = "docs/b.md"
impl_files = ["src/b.rs"]
test_files = []
"#;

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t.t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        write(&repo, "README.md", "seed\n");
        write(
            &repo,
            "specguard.toml",
            "[project]\nname = \"Demo\"\nroot = \".\"\n\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n\n[[area]]\nname = \"src\"\nglobs = [\"src/**\"]\ncanon = [\"docs/spec.md\"]\n",
        );
        write(&repo, "src/a.rs", "pub fn a() {}\n");
        write(&repo, "src/b.rs", "pub fn b() {}\n");
        write(&repo, "tests/a.rs", "#[test]\nfn a() {}\n");
        write(&repo, "docs/a.md", "# a\n");
        write(&repo, "docs/b.md", "# b\n");
        write(&repo, ".specguard/spec-map.toml", MAP_BOTH_UNTESTED);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "seed"]);
        Fx {
            repo,
            home,
            _tmp: tmp,
        }
    }

    fn sg(&self, sub: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_specguard"))
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("SPECGUARD_BASELINE_REF")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(["--config", "specguard.toml", "--date", "2026-01-01"])
            .args(sub)
            .output()
            .expect("spawn specguard")
    }

    fn set_map(&self, body: &str) {
        write(&self.repo, ".specguard/spec-map.toml", body);
    }

    fn findings_file(&self) -> PathBuf {
        find_file(&self.home.join(".overwatch"), "review_findings.jsonl")
            .expect("`specguard audit` must have recorded review findings")
    }

    fn finding_ids(&self) -> Vec<String> {
        std::fs::read_to_string(self.findings_file())
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                v["finding_id"].as_str().unwrap().to_string()
            })
            .collect()
    }

    fn id_for(&self, kind: &str, key: &str) -> String {
        self.finding_ids()
            .into_iter()
            .find(|i| i.starts_with(&format!("specguard:{kind}:")) && i.contains(key))
            .unwrap_or_else(|| {
                panic!(
                    "audit must record specguard:{kind}:..{key}.. ; have {:?}",
                    self.finding_ids()
                )
            })
    }

    fn dispositions(&self) -> Vec<Value> {
        let p = self.findings_file().with_file_name("dispositions.jsonl");
        match std::fs::read_to_string(p) {
            Ok(t) => t
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn disposition_of(&self, id: &str) -> Option<Value> {
        self.dispositions()
            .into_iter()
            .find(|d| d["finding_id"] == id)
    }

    /// Run `specguard audit` so the real producer records the findings.
    fn audit(&self) {
        let out = self.sg(&["audit"]);
        assert!(out.status.code().is_some(), "audit must run: {out:?}");
    }

    fn reconcile(&self) -> (Output, Value) {
        let out = self.sg(&["reconcile-findings", "--json"]);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!("`specguard reconcile-findings --json` must print JSON ({e}); out={out:?}")
        });
        (out, v)
    }
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

fn ids(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("`{key}` array in {v}"))
        .iter()
        .map(|x| {
            x.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| x["finding_id"].as_str().unwrap().to_string())
        })
        .collect()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn overwatch_bin() -> PathBuf {
    static BUILT: std::sync::Once = std::sync::Once::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    BUILT.call_once(|| {
        let st = Command::new(env!("CARGO"))
            .current_dir(&root)
            .args(["build", "-q", "-p", "overwatch", "--bin", "overwatch"])
            .status()
            .unwrap();
        assert!(st.success(), "cargo build -p overwatch");
    });
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("target"));
    target.join("debug/overwatch")
}

impl Fx {
    fn queue(&self) -> Value {
        let out = Command::new(overwatch_bin())
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(["review-queue", "--json"])
            .output()
            .expect("spawn overwatch");
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("review-queue --json must print JSON ({e}); {out:?}"))
    }
}

/// Count queue rows whose serialized form mentions `needle`.
fn rows_mentioning(v: &Value, needle: &str) -> usize {
    fn walk(v: &Value, needle: &str, n: &mut usize) {
        match v {
            Value::Array(a) => {
                for x in a {
                    let s = x.to_string();
                    if x.is_object() && s.contains(needle) {
                        *n += 1;
                    } else {
                        walk(x, needle, n);
                    }
                }
            }
            Value::Object(o) => {
                for x in o.values() {
                    walk(x, needle, n);
                }
            }
            _ => {}
        }
    }
    let mut n = 0;
    walk(v, needle, &mut n);
    n
}

#[test]
fn recurrence_after_resolved_is_a_new_open_finding_in_review_queue() {
    let fx = Fx::new();
    fx.audit();
    let alpha = fx.id_for("untested", "alpha");

    // Precondition 1: audit surfaces alpha as an open queue row.
    let q0 = fx.queue();
    assert!(
        rows_mentioning(&q0, "alpha") >= 1,
        "alpha must be visible after first audit: {q0}"
    );

    // Fix, reconcile => resolved; precondition 2: it left the queue.
    fx.set_map(MAP_ALPHA_FIXED);
    let (out, v) = fx.reconcile();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(ids(&v, "resolved"), vec![alpha.clone()], "{v}");
    let q1 = fx.queue();
    assert_eq!(
        rows_mentioning(&q1, "alpha"),
        0,
        "resolved alpha must be out of the queue (else the test proves nothing): {q1}"
    );

    // Reintroduce the same gap; audit again.
    fx.set_map(MAP_BOTH_UNTESTED);
    fx.audit();

    // The recurrence must be a NEW open finding visible in the queue.
    let q2 = fx.queue();
    assert!(
        rows_mentioning(&q2, "alpha") >= 1,
        "RECURRENCE HIDDEN: alpha's untested gap came back after being resolved, \
         but review-queue shows no row for it. ids in store={:?}; queue={q2}",
        fx.finding_ids()
    );
}
