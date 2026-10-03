// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Backlog 89544915 (T10) — specguard STRUCTURAL findings
// (`specguard:<undocumented|dangling-reference|untested>:<key>`, recorded by
// `specguard audit`) are closed by RE-RUNNING the same structural detection,
// never by commit-message matching. Shard-audit kinds (`spec-drift`,
// `spec-doc-stale`, `audit-indeterminate`: agent judgment) are NEVER
// auto-closed. Independent tests (author != implementer).
//
// CONTRACT ASSUMED — the implementer MUST match these exactly:
//
// CLI: `specguard [--config specguard.toml --date D] reconcile-findings [--json]`
//   * cwd = repo root. Loads the spec map (`.specguard/spec-map.toml`), runs the
//   SAME deterministic structural detection `specguard audit` uses, and for
//   every OPEN review finding with id `specguard:<structural-kind>:<key>`:
//   - detection ran and no longer reports that (kind,key) => append a
//   disposition: verdict `resolved`, non-empty `reviewer`, `evidence`
//   (string) naming the kind and key, non-empty `observed_source`;
//   - detection still reports it => stays open, NOT undetermined;
//   - detection cannot run (spec map absent / unparseable) => UNDETERMINED:
//   stays open, stderr line containing `NOT closed` + the finding id,
//   JSON `undetermined:[{finding_id, why}]`, exit 3.
//   * `--json` stdout: `{"resolved":["<finding_id>",..],
//   "undetermined":[{"finding_id":"..","why":".."}]}`
//   (both keys always present).
//   * Exit 0 when nothing is undetermined, else 3.
//   * Findings of any other id shape (incl. `specguard:spec-drift:*`) are never
//   touched.

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

/// Anti-vacuity precondition shared by every test: the real `audit` recorded
/// BOTH untested findings (so a missing reconcile can't hide behind no data).
fn seeded() -> (Fx, String, String) {
    let fx = Fx::new();
    fx.audit();
    let a = fx.id_for("untested", "alpha");
    let b = fx.id_for("untested", "beta");
    (fx, a, b)
}

/// T10 (RED): alpha's structural condition is fixed (test attributed) so the
/// re-detection no longer reports it => closed `resolved` with evidence; beta,
/// still reported, stays open (control: a closer that closes everything fails).
#[test]
fn t10_structural_finding_closes_when_redetection_no_longer_reports_it() {
    let (fx, a, b) = seeded();
    fx.set_map(MAP_ALPHA_FIXED);
    let (out, v) = fx.reconcile();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(ids(&v, "resolved"), vec![a.clone()], "{v}");
    assert!(ids(&v, "undetermined").is_empty(), "{v}");
    let d = fx.disposition_of(&a).expect("alpha closed");
    assert_eq!(d["verdict"], "resolved", "{d}");
    assert!(!d["reviewer"].as_str().unwrap_or("").is_empty(), "{d}");
    assert!(
        !d["observed_source"].as_str().unwrap_or("").is_empty(),
        "{d}"
    );
    let ev = d["evidence"].as_str().expect("evidence string");
    assert!(
        ev.contains("untested") && ev.contains("alpha"),
        "evidence must name kind and key: {ev}"
    );
    assert!(
        fx.disposition_of(&b).is_none(),
        "still-reported finding must stay open"
    );
}

/// T10 (stays open): nothing fixed => nothing closed, exit 0 (not undetermined).
#[test]
fn t10_still_reported_findings_stay_open() {
    let (fx, _a, _b) = seeded();
    let (out, v) = fx.reconcile();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(ids(&v, "resolved").is_empty(), "{v}");
    assert!(ids(&v, "undetermined").is_empty(), "{v}");
    assert!(fx.dispositions().is_empty(), "{:?}", fx.dispositions());
}

/// T10 (undetermined): the map is unparseable, so detection cannot run. "No
/// findings reported" must NOT be read as "resolved": both stay open, exit 3.
#[test]
fn t10_unparseable_map_is_undetermined_not_resolved() {
    let (fx, a, b) = seeded();
    fx.set_map("this is [[ not toml at all");
    let (out, v) = fx.reconcile();
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let mut u = ids(&v, "undetermined");
    u.sort();
    let mut want = vec![a.clone(), b.clone()];
    want.sort();
    assert_eq!(u, want, "{v}");
    assert!(!v["undetermined"][0]["why"].as_str().unwrap().is_empty());
    assert!(stderr(&out).contains("NOT closed"), "{}", stderr(&out));
    assert!(stderr(&out).contains(&a), "{}", stderr(&out));
    assert!(fx.dispositions().is_empty());
}

/// T10 (undetermined): the map file is ABSENT.
#[test]
fn t10_absent_map_is_undetermined_not_resolved() {
    let (fx, a, _b) = seeded();
    std::fs::remove_file(fx.repo.join(".specguard/spec-map.toml")).unwrap();
    let (out, v) = fx.reconcile();
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(ids(&v, "undetermined").contains(&a), "{v}");
    assert!(ids(&v, "resolved").is_empty(), "{v}");
    assert!(fx.dispositions().is_empty());
}

/// R4 (guard): agent-judgment shard-audit kinds are never auto-closed, even
/// when the structural detection is clean for their key.
#[test]
fn t10_shard_audit_kinds_are_never_auto_closed() {
    let (fx, _a, _b) = seeded();
    // Append hand-made shard-audit findings to the same store.
    let path = fx.findings_file();
    let mut txt = std::fs::read_to_string(&path).unwrap();
    for id in [
        "specguard:spec-drift:src",
        "specguard:spec-doc-stale:src",
        "specguard:audit-indeterminate:src",
    ] {
        let f = overwatch::review_finding::ReviewFinding::new(
            id.to_string(),
            "specguard".to_string(),
            Some("medium".to_string()),
            format!("shard finding {id}"),
            None,
            None,
            1_700_000_000,
        );
        txt.push_str(&serde_json::to_string(&f).unwrap());
        txt.push('\n');
    }
    std::fs::write(&path, txt).unwrap();
    // Fix everything structural so a wrongly-broad closer would close them.
    fx.set_map(
        r#"last_synced = ""

[entries.alpha]
key = "alpha"
kind = "feature"
spec_doc = "docs/a.md"
impl_files = ["src/a.rs"]
test_files = ["tests/a.rs"]
"#,
    );
    let (_out, v) = fx.reconcile();
    for id in [
        "specguard:spec-drift:src",
        "specguard:spec-doc-stale:src",
        "specguard:audit-indeterminate:src",
    ] {
        assert!(
            !ids(&v, "resolved").contains(&id.to_string()),
            "{id} must not be auto-closed: {v}"
        );
        assert!(
            fx.disposition_of(id).is_none(),
            "{id} must have no disposition"
        );
    }
}
