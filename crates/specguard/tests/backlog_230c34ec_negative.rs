#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Negative cases for backlog 230c34ec: `specguard brief` now applies the
//! tracked `.specguard/spec-docs.toml` bindings, and that new source of
//! `covered` must stay fail-closed (CLAUDE.md §3):
//!
//! * a binding the gate would reject (missing doc, empty doc, doc outside
//!   `docs/specs/`, blank reason) never makes an entry covered — the verdict
//!   stays `not-covered`, exit 0;
//! * an unparseable or unreadable binding file is `undetermined`, exit 10 —
//!   never `covered` and never `not-covered`.
//!
//! Written by the implementing worker (not an independent auditor); each case
//! was observed RED against a mutant of the implementation before landing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "specguard-backlog-230c34ec-neg-{tag}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
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
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A repo whose map entry for `src/gate.rs` has no map-level `spec_doc`; the
/// only possible source of coverage is `spec_docs` (written verbatim to
/// `.specguard/spec-docs.toml`). `docs/specs/gate.md` exists and is non-empty;
/// `docs/specs/empty.md` exists and is blank; `docs/other/gate.md` exists
/// outside `docs/specs/`.
fn repo(tag: &str, spec_docs: &str) -> PathBuf {
    let dir = scratch(tag);
    fs::create_dir_all(dir.join(".specguard")).unwrap();
    fs::create_dir_all(dir.join("docs/specs")).unwrap();
    fs::create_dir_all(dir.join("docs/other")).unwrap();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("specguard.toml"),
        "[project]\nname = \"Demo\"\nroot = \".\"\n\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n\n[[area]]\nname = \"src\"\nglobs = [\"src/**\"]\ncanon = [\"docs/specs/gate.md\"]\n",
    )
    .unwrap();
    fs::write(
        dir.join("docs/specs/gate.md"),
        "# Gate\n\nThe gate blocks.\n",
    )
    .unwrap();
    fs::write(dir.join("docs/specs/empty.md"), "   \n").unwrap();
    fs::write(dir.join("docs/other/gate.md"), "# Gate\n\nElsewhere.\n").unwrap();
    fs::write(dir.join("src/gate.rs"), "fn main() {}\n").unwrap();
    fs::write(
        dir.join(".specguard/spec-map.toml"),
        "last_synced = \"deadbeef\"\n[entries.\"src/gate.rs\"]\nkey = \"src/gate.rs\"\nkind = \"feature\"\nstatus = \"changed\"\nimpl_files = [\"src/gate.rs\"]\n",
    )
    .unwrap();
    fs::write(dir.join(".specguard/spec-docs.toml"), spec_docs).unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "t@t.t"]);
    git(&dir, &["config", "user.name", "t"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

/// `(verdict, exit code, stdout)` of `specguard brief --json src/gate.rs`.
fn brief(dir: &Path) -> (String, i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(dir)
        .args([
            "--config",
            "specguard.toml",
            "brief",
            "--json",
            "src/gate.rs",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("brief --json not JSON ({e}): {out:?}"));
    (
        v["verdict"].as_str().unwrap_or("<none>").to_string(),
        out.status.code().unwrap_or(-1),
        stdout,
    )
}

fn binding(doc: &str, reason: &str) -> String {
    format!("[[spec]]\npath = \"src/gate.rs\"\ndoc = \"{doc}\"\nreason = \"{reason}\"\n")
}

fn assert_not_covered(tag: &str, spec_docs: &str) {
    let dir = repo(tag, spec_docs);
    let (verdict, code, out) = brief(&dir);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        verdict, "not-covered",
        "{tag}: a rejected binding must not make the entry covered: {out}"
    );
    assert_eq!(code, 0, "{tag}: not-covered is an answer (exit 0): {out}");
}

fn assert_undetermined(tag: &str, dir: &Path) {
    let (verdict, code, out) = brief(dir);
    assert_eq!(
        verdict, "undetermined",
        "{tag}: an unobservable binding file must be undetermined: {out}"
    );
    assert_eq!(code, 10, "{tag}: undetermined must exit 10: {out}");
}

#[test]
fn backlog_230c34ec_binding_to_missing_doc_is_not_covered() {
    assert_not_covered(
        "missing-doc",
        &binding("docs/specs/nope.md", "describes src/gate.rs"),
    );
}

#[test]
fn backlog_230c34ec_binding_to_empty_doc_is_not_covered() {
    assert_not_covered(
        "empty-doc",
        &binding("docs/specs/empty.md", "describes src/gate.rs"),
    );
}

#[test]
fn backlog_230c34ec_binding_outside_docs_specs_is_not_covered() {
    assert_not_covered(
        "outside-dir",
        &binding("docs/other/gate.md", "describes src/gate.rs"),
    );
}

#[test]
fn backlog_230c34ec_binding_with_blank_reason_is_not_covered() {
    assert_not_covered("blank-reason", &binding("docs/specs/gate.md", "   "));
}

#[test]
fn backlog_230c34ec_unparseable_spec_docs_is_undetermined() {
    // Contains a VALID binding after a parse error: a reader that skipped the
    // bad part would report covered.
    let text = format!(
        "this is not TOML [[[ ??\n{}",
        binding("docs/specs/gate.md", "describes src/gate.rs")
    );
    let dir = repo("unparseable", &text);
    assert_undetermined("unparseable", &dir);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn backlog_230c34ec_unreadable_spec_docs_is_undetermined() {
    use std::os::unix::fs::PermissionsExt;
    let dir = repo(
        "unreadable",
        &binding("docs/specs/gate.md", "describes src/gate.rs"),
    );
    let path = dir.join(".specguard/spec-docs.toml");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    // Prove the precondition instead of assuming it: if this environment cannot
    // make a file unreadable, the branch is unobserved and the test must fail.
    assert!(
        fs::read_to_string(&path).is_err(),
        "cannot make spec-docs.toml unreadable here; the unreadable branch is UNOBSERVED"
    );
    let (verdict, code, out) = brief(&dir);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        verdict, "undetermined",
        "unreadable: an unobservable binding file must be undetermined: {out}"
    );
    assert_eq!(code, 10, "unreadable: undetermined must exit 10: {out}");
}
