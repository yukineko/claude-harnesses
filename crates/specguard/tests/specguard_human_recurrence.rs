// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(dead_code)] // fixture copied from specguard_recurrence.rs; not every helper is used here
                     // HUMAN-VERDICT RECURRENCE (user ruling 2026-10-03). For a structural finding
                     // `specguard:<kind>:<key>[:<epoch>]` whose latest episode was closed by a HUMAN verdict
                     // (confirmed / dismissed / false-positive via `overwatch record-disposition`): if a later
                     // `specguard audit` OBSERVES the gap gone, and an even later audit sees it again, a NEW open
                     // finding must appear in `overwatch review-queue --json`. Guards: (1) a dismissed gap that
                     // never went away must not resurface; (2) resolved-recurrence is covered by
                     // specguard_recurrence.rs; (3) absence must be observed by an audit run.
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

impl Fx {
    /// Close `id` with a human verdict through the real overwatch CLI.
    fn human_close(&self, id: &str, verdict: &str) {
        let out = Command::new(overwatch_bin())
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args([
                "record-disposition",
                "--finding-id",
                id,
                "--verdict",
                verdict,
                "--reviewer",
                "human-test",
            ])
            .output()
            .expect("spawn overwatch");
        assert!(
            out.status.success(),
            "record-disposition {verdict} failed: {out:?}"
        );
        assert!(
            self.disposition_of(id).is_some(),
            "disposition must be recorded for {id}"
        );
    }
}

/// audit -> human verdict -> (clean audit) -> reintroduce -> audit.
fn run_main_case(verdict: &str) {
    let fx = Fx::new();
    fx.audit();
    let alpha = fx.id_for("untested", "alpha");
    assert!(rows_mentioning(&fx.queue(), "alpha") >= 1);

    fx.human_close(&alpha, verdict);
    assert_eq!(
        rows_mentioning(&fx.queue(), "alpha"),
        0,
        "{verdict}-closed alpha must leave the queue (else the test proves nothing)"
    );

    // An audit OBSERVES the gap gone: alpha is not reported any more.
    fx.set_map(MAP_ALPHA_FIXED);
    fx.audit();
    assert_eq!(rows_mentioning(&fx.queue(), "alpha"), 0);

    // The gap returns; the next audit sees it.
    fx.set_map(MAP_BOTH_UNTESTED);
    fx.audit();
    let q = fx.queue();
    assert!(
        rows_mentioning(&q, "alpha") >= 1,
        "RECURRENCE HIDDEN after human `{verdict}`: alpha's gap was observed gone then came \
         back, but review-queue shows no row. ids in store={:?}; queue={q}",
        fx.finding_ids()
    );
}

#[test]
fn recurrence_after_human_confirmed_is_a_new_open_finding() {
    run_main_case("confirmed");
}

#[test]
fn recurrence_after_human_dismissed_is_a_new_open_finding() {
    run_main_case("dismissed");
}

#[test]
fn recurrence_after_human_false_positive_is_a_new_open_finding() {
    run_main_case("false-positive");
}

/// Guard 1: a dismissal stands while the gap never went away.
#[test]
fn dismissed_gap_that_never_went_away_does_not_resurface() {
    let fx = Fx::new();
    fx.audit();
    let alpha = fx.id_for("untested", "alpha");
    fx.human_close(&alpha, "dismissed");
    for _ in 0..3 {
        fx.audit();
    }
    let q = fx.queue();
    assert_eq!(
        rows_mentioning(&q, "alpha"),
        0,
        "dismissal must stand while the gap is continuously present: {q}"
    );
}

/// Guard 3: absence must be observed by an audit; reintroducing without an intervening
/// audit that saw the gap gone is the same as never gone.
#[test]
fn dismiss_then_reintroduce_without_clean_audit_does_not_resurface() {
    let fx = Fx::new();
    fx.audit();
    let alpha = fx.id_for("untested", "alpha");
    fx.human_close(&alpha, "dismissed");
    fx.set_map(MAP_ALPHA_FIXED); // fixed on disk, but NO audit observes it
    fx.set_map(MAP_BOTH_UNTESTED); // gap is back before any audit ran
    fx.audit();
    let q = fx.queue();
    assert_eq!(
        rows_mentioning(&q, "alpha"),
        0,
        "no audit observed the gap gone, so the dismissal must stand: {q}"
    );
}

/// Guard 2: the automatic resolved-recurrence path still works (kept alongside, since this
/// is the behaviour the human path must match).
#[test]
fn resolved_recurrence_still_a_new_open_finding() {
    let fx = Fx::new();
    fx.audit();
    let alpha = fx.id_for("untested", "alpha");
    fx.set_map(MAP_ALPHA_FIXED);
    let (out, v) = fx.reconcile();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(ids(&v, "resolved"), vec![alpha], "{v}");
    fx.set_map(MAP_BOTH_UNTESTED);
    fx.audit();
    let q = fx.queue();
    assert!(rows_mentioning(&q, "alpha") >= 1, "{q}");
}
