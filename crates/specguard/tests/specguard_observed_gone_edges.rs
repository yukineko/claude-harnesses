// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! EDGES of the "observed gone" ledger (user ruling 2026-10-03; the main
//! contract is in `specguard_human_recurrence.rs`). A human-closed structural
//! finding resurfaces as a new episode only after an audit OBSERVED the gap
//! gone. What must NOT count as an observation:
//!
//! 1. an audit run with the spec map ABSENT (it loads as an empty map);
//! 2. an audit run with the spec map UNPARSEABLE;
//! 3. a FILTERED audit whose scope excludes the key;
//! 4. a map entry REMOVED, seen by a filtered audit (only a whole-map audit
//!    may treat a removed entry as gone);
//! 5. an absence observed BEFORE the human verdict existed.
//!
//! And: an unreadable observed-gone ledger resurfaces the finding (a visible
//! duplicate over a hidden finding) with a warning on stderr.
//!
//! Independent test (author != implementer).

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fx {
    repo: PathBuf,
    home: PathBuf,
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

/// alpha's entry is REMOVED from the map; beta remains.
const MAP_ALPHA_REMOVED: &str = r#"last_synced = ""

[entries.beta]
key = "beta"
kind = "feature"
spec_doc = "docs/b.md"
impl_files = ["src/b.rs"]
test_files = []
"#;

const LEDGER: &str = "specguard_observed_gone.jsonl";

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

    fn map_path(&self) -> PathBuf {
        self.repo.join(".specguard/spec-map.toml")
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

    /// How many recorded episodes exist for the `untested` finding of `key`.
    fn episodes(&self, key: &str) -> usize {
        self.finding_ids()
            .iter()
            .filter(|i| i.starts_with("specguard:untested:") && i.contains(key))
            .count()
    }

    fn id_for(&self, key: &str) -> String {
        self.finding_ids()
            .into_iter()
            .find(|i| i.starts_with("specguard:untested:") && i.contains(key))
            .unwrap_or_else(|| {
                panic!(
                    "audit must record specguard:untested:..{key}.. ; have {:?}",
                    self.finding_ids()
                )
            })
    }

    /// Run `specguard audit [extra...]` so the real producer records findings.
    fn audit_with(&self, extra: &[&str]) -> Output {
        let mut args = vec!["audit"];
        args.extend_from_slice(extra);
        let out = self.sg(&args);
        assert!(out.status.code().is_some(), "audit must run: {out:?}");
        out
    }

    fn audit(&self) -> Output {
        self.audit_with(&[])
    }

    fn overwatch(&self, args: &[&str]) -> Output {
        Command::new(overwatch_bin())
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(args)
            .output()
            .expect("spawn overwatch")
    }

    fn queue(&self) -> Value {
        let out = self.overwatch(&["review-queue", "--json"]);
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("review-queue --json must print JSON ({e}); {out:?}"))
    }

    fn human_close(&self, id: &str, verdict: &str) {
        let out = self.overwatch(&[
            "record-disposition",
            "--finding-id",
            id,
            "--verdict",
            verdict,
            "--reviewer",
            "human-test",
        ]);
        assert!(
            out.status.success(),
            "record-disposition {verdict} failed: {out:?}"
        );
    }

    /// audit (both gaps) -> dismiss alpha; returns with alpha out of the queue.
    fn seed_dismissed_alpha(&self) {
        self.audit();
        let alpha = self.id_for("alpha");
        assert!(rows_mentioning(&self.queue(), "alpha") >= 1);
        self.human_close(&alpha, "dismissed");
        assert_eq!(
            rows_mentioning(&self.queue(), "alpha"),
            0,
            "dismissed alpha must leave the queue (else the test proves nothing)"
        );
    }

    /// The dismissal must have stood: still ONE alpha episode, none in the queue.
    fn assert_dismissal_stands(&self, why: &str) {
        let q = self.queue();
        assert_eq!(
            rows_mentioning(&q, "alpha"),
            0,
            "{why}: the dismissal must stand, but alpha is back in the queue: {q}; ids={:?}",
            self.finding_ids()
        );
        assert_eq!(
            self.episodes("alpha"),
            1,
            "{why}: ids={:?}",
            self.finding_ids()
        );
    }

    fn assert_resurfaced(&self, why: &str) {
        let q = self.queue();
        assert!(
            rows_mentioning(&q, "alpha") >= 1,
            "{why}: expected a new open alpha episode; ids={:?}; queue={q}",
            self.finding_ids()
        );
        assert!(
            self.episodes("alpha") >= 2,
            "{why}: ids={:?}",
            self.finding_ids()
        );
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

/// Count queue rows whose serialized form mentions `needle`.
fn rows_mentioning(v: &Value, needle: &str) -> usize {
    fn walk(v: &Value, needle: &str, n: &mut usize) {
        match v {
            Value::Array(a) => {
                for x in a {
                    if x.is_object() && x.to_string().contains(needle) {
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

// ---------------------------------------------------------------- 1

/// An ABSENT map during the "clean" audit is not an observation.
#[test]
fn absent_map_audit_does_not_count_as_observed_gone() {
    let fx = Fx::new();
    fx.seed_dismissed_alpha();
    std::fs::remove_file(fx.map_path()).unwrap();
    fx.audit();
    fx.set_map(MAP_BOTH_UNTESTED);
    fx.audit();
    fx.assert_dismissal_stands("absent map is not 'every gap gone'");
}

// ---------------------------------------------------------------- 2

/// An UNPARSEABLE map during the "clean" audit is not an observation.
#[test]
fn unparseable_map_audit_does_not_count_as_observed_gone() {
    let fx = Fx::new();
    fx.seed_dismissed_alpha();
    fx.set_map("this is [[[ not = valid toml\n");
    fx.audit(); // may fail; whatever it does it must not record an observation
    fx.set_map(MAP_BOTH_UNTESTED);
    fx.audit();
    fx.assert_dismissal_stands("unparseable map is not 'every gap gone'");
}

// ---------------------------------------------------------------- 3

/// `--filter beta` does not cover alpha: alpha's absence there is unobserved.
#[test]
fn filtered_audit_not_covering_the_key_records_nothing() {
    let fx = Fx::new();
    fx.seed_dismissed_alpha();
    fx.set_map(MAP_ALPHA_FIXED);
    fx.audit_with(&["--filter", "beta"]);
    fx.set_map(MAP_BOTH_UNTESTED);
    fx.audit();
    fx.assert_dismissal_stands("filtered audit excluded alpha");
}

/// Control for 3: the same sequence with a filter that COVERS alpha resurfaces.
#[test]
fn filtered_audit_covering_the_key_does_record_it() {
    let fx = Fx::new();
    fx.seed_dismissed_alpha();
    fx.set_map(MAP_ALPHA_FIXED);
    fx.audit_with(&["--filter", "alpha"]);
    fx.set_map(MAP_BOTH_UNTESTED);
    fx.audit();
    fx.assert_resurfaced("a covering filtered audit observed alpha gone");
}

// ---------------------------------------------------------------- 4

/// A removed entry seen only by a FILTERED audit is not observed gone.
#[test]
fn removed_entry_counts_only_on_a_whole_map_audit_filtered_does_not() {
    let fx = Fx::new();
    fx.seed_dismissed_alpha();
    fx.set_map(MAP_ALPHA_REMOVED);
    fx.audit_with(&["--filter", "beta"]);
    fx.set_map(MAP_BOTH_UNTESTED);
    fx.audit();
    fx.assert_dismissal_stands("a filtered audit cannot tell alpha was removed");
}

/// Control for 4: the same removal seen by a WHOLE-map audit resurfaces.
#[test]
fn removed_entry_on_a_whole_map_audit_counts_as_observed_gone() {
    let fx = Fx::new();
    fx.seed_dismissed_alpha();
    fx.set_map(MAP_ALPHA_REMOVED);
    fx.audit();
    fx.set_map(MAP_BOTH_UNTESTED);
    fx.audit();
    fx.assert_resurfaced("whole-map audit saw alpha's entry removed");
}

// ---------------------------------------------------------------- 5

/// An absence observed BEFORE the human verdict does not undo it later.
#[test]
fn absence_observed_before_the_human_verdict_does_not_count() {
    let fx = Fx::new();
    fx.audit(); // gap recorded, still open
    let alpha = fx.id_for("alpha");
    fx.set_map(MAP_ALPHA_FIXED);
    fx.audit(); // gap observed gone while nothing is human-closed
    fx.set_map(MAP_BOTH_UNTESTED);
    fx.audit(); // gap back; episode still the same open one
    assert_eq!(fx.episodes("alpha"), 1, "ids={:?}", fx.finding_ids());
    fx.human_close(&alpha, "dismissed");
    fx.audit(); // gap continuously present since the verdict
    fx.assert_dismissal_stands("the only absence predates the verdict");
}

// ---------------------------------------------------------------- 6

/// An unreadable observed-gone ledger must not keep a human-closed finding
/// hidden: it resurfaces as a new episode, with a warning on stderr.
#[test]
fn unreadable_observed_gone_ledger_resurfaces_with_a_warning() {
    let fx = Fx::new();
    fx.seed_dismissed_alpha();
    let ledger = fx.findings_file().with_file_name(LEDGER);
    std::fs::create_dir_all(&ledger).unwrap(); // a directory: cannot be read as a file
    let out = fx.audit(); // gap still present, no map change
    fx.assert_resurfaced("an unreadable ledger is not an answer of 'never observed'");
    let err = stderr(&out);
    assert!(
        err.contains("observed-gone ledger could not be read")
            && err.contains("re-raised as a new episode"),
        "the resurfacing warning (ledger unreadable, finding re-raised) must be on stderr; got: {err}"
    );
}
