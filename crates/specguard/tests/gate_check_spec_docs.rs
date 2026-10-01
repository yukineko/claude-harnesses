// Integration tests: unwrap/expect are fine here (workspace lints are for production code).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED tests for the TRACKED spec-doc pass source of `specguard map gate-check`.
//!
//! The spec map is a gitignored machine-local cache, so a map `spec_doc` cannot
//! be the only durable pass source. Contract: a tracked `.specguard/spec-docs.toml`
//!
//! ```toml
//! [[spec]]
//! path = "<entry key or one of its impl files>"
//! doc = "docs/specs/<name>.md"
//! reason = "<why this doc describes that file>"
//! ```
//!
//! lets a changed gate entry pass (exit 0) only when the binding names the entry
//! key or one of its impl files, the reason is non-blank, and `doc` is a relative
//! `docs/specs/**.md` path without `..` that exists as a non-blank file under the
//! repo root. A binding that fails any of those does not count (exit 1 when no
//! other pass source exists). An unparseable file or unknown field is exit 2,
//! never 0. An absent file keeps the old behaviour.
//!
//! Written by an independent test author (CLAUDE.md §2(a)); fixture style mirrors
//! `gate_check.rs`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const GATE_PATH: &str = "crates/blastguard/src/a.rs";
const OTHER_IMPL: &str = "crates/blastguard/src/other.rs";
const ENTRY_KEY: &str = "blast-a";
const SPEC_DOCS: &str = ".specguard/spec-docs.toml";
const ACKS: &str = ".specguard/spec-doc-acks.toml";
const DOC: &str = "docs/specs/blastguard.md";

fn git(repo: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn write(repo: &Path, rel: &str, body: &str) {
    let p = repo.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

/// Base commit with gate + non-gate files and a README; returns the base rev.
fn fixture(repo: &Path) -> String {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t.t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    write(
        repo,
        "specguard.toml",
        "[project]\nname = \"Demo\"\nroot = \".\"\n[output]\nreport_dir = \"reports\"\nsentinel = \".pending\"\n",
    );
    write(repo, GATE_PATH, "fn a() {}\n");
    write(repo, OTHER_IMPL, "fn o() {}\n");
    write(repo, "crates/notagate/src/b.rs", "fn b() {}\n");
    write(repo, "README.md", "# Demo\n\nA real, non-blank readme.\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["rev-parse", "HEAD"])
}

/// Changes the gate file and commits everything currently in the working tree
/// (map, spec-docs.toml, docs), so the pass source is both tracked and on disk.
fn commit_change(repo: &Path) {
    write(repo, GATE_PATH, "fn changed() {}\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "change"]);
}

/// Map with one gate entry (no spec_doc) listing GATE_PATH and OTHER_IMPL.
fn write_map_no_spec_doc(repo: &Path) {
    write(
        repo,
        ".specguard/spec-map.toml",
        &format!(
            "last_synced = \"x\"\n\n[entries.\"{ENTRY_KEY}\"]\nkey = \"{ENTRY_KEY}\"\n\
             status = \"changed\"\nimpl_files = [\"{GATE_PATH}\", \"{OTHER_IMPL}\"]\n\n"
        ),
    );
}

fn binding(path: &str, doc: &str, reason: Option<&str>) -> String {
    let r = reason
        .map(|r| format!("reason = \"{r}\"\n"))
        .unwrap_or_default();
    format!("[[spec]]\npath = \"{path}\"\ndoc = \"{doc}\"\n{r}")
}

fn write_doc(repo: &Path, rel: &str) {
    write(
        repo,
        rel,
        "# blastguard spec\n\nThe gate decides Deny > Ask > Allow.\n",
    );
}

fn gate_check(repo: &Path, base: &str) -> Output {
    let o = Command::new(env!("CARGO_BIN_EXE_specguard"))
        .current_dir(repo)
        .args([
            "--config",
            "specguard.toml",
            "map",
            "gate-check",
            "--base",
            base,
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !err.contains("unrecognized subcommand") && !err.contains("unexpected argument"),
        "`map gate-check` usage error must not satisfy an exit-code assertion: {err}"
    );
    o
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn all(o: &Output) -> String {
    format!(
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Build a repo: map entry without spec_doc, optional spec-docs.toml body,
/// optional docs, then commit the gate change. Returns the gate-check output.
fn run(spec_docs: Option<&str>, docs: &[(&str, &str)], ack: Option<&str>) -> Output {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    write_map_no_spec_doc(d.path());
    if let Some(body) = spec_docs {
        write(d.path(), SPEC_DOCS, body);
    }
    for (rel, body) in docs {
        write(d.path(), rel, body);
    }
    if let Some(a) = ack {
        write(d.path(), ACKS, a);
    }
    commit_change(d.path());
    gate_check(d.path(), &base)
}

const DOC_BODY: &str = "# blastguard spec\n\nThe gate decides Deny > Ask > Allow.\n";

// ---- 1. valid bindings pass ----------------------------------------------------

#[test]
fn binding_naming_changed_impl_file_passes() {
    let o = run(
        Some(&binding(
            GATE_PATH,
            DOC,
            Some("describes blastguard decisions"),
        )),
        &[(DOC, DOC_BODY)],
        None,
    );
    assert_eq!(
        code(&o),
        0,
        "a reasoned binding of the changed impl file to an existing non-blank \
         docs/specs/*.md must be a pass source (tracked replacement for map spec_doc)\n{}",
        all(&o)
    );
}

#[test]
fn binding_naming_entry_key_passes() {
    let o = run(
        Some(&binding(
            ENTRY_KEY,
            DOC,
            Some("describes blastguard decisions"),
        )),
        &[(DOC, DOC_BODY)],
        None,
    );
    assert_eq!(
        code(&o),
        0,
        "a reasoned binding naming the ENTRY KEY must satisfy the entry\n{}",
        all(&o)
    );
}

#[test]
fn binding_naming_sibling_impl_file_of_the_entry_passes() {
    // The entry lists both GATE_PATH (changed) and OTHER_IMPL (unchanged); a binding
    // on OTHER_IMPL names "one of its impl files", mirroring the ack semantics.
    let o = run(
        Some(&binding(
            OTHER_IMPL,
            DOC,
            Some("same spec covers the entry"),
        )),
        &[(DOC, DOC_BODY)],
        None,
    );
    assert_eq!(
        code(&o),
        0,
        "a binding naming any impl file of the triggered entry must satisfy it\n{}",
        all(&o)
    );
}

#[test]
fn nested_docs_specs_path_passes() {
    let nested = "docs/specs/gates/blastguard.md";
    let o = run(
        Some(&binding(GATE_PATH, nested, Some("nested spec dir"))),
        &[(nested, DOC_BODY)],
        None,
    );
    assert_eq!(
        code(&o),
        0,
        "a doc in a subdirectory of docs/specs/ is still under docs/specs/\n{}",
        all(&o)
    );
}

// ---- 2. reason required ---------------------------------------------------------

#[test]
fn blank_or_missing_reason_does_not_count() {
    for (label, body) in [
        ("blank", binding(GATE_PATH, DOC, Some("  "))),
        ("empty", binding(GATE_PATH, DOC, Some(""))),
        ("missing", binding(GATE_PATH, DOC, None)),
    ] {
        let o = run(Some(&body), &[(DOC, DOC_BODY)], None);
        assert_eq!(
            code(&o),
            1,
            "a binding with {label} reason must NOT be a pass source (and a missing \
             reason must parse to a violation, not undetermined)\n{}",
            all(&o)
        );
    }
}

// ---- 3. doc must exist ------------------------------------------------------------

#[test]
fn binding_to_missing_doc_exits_1_and_names_the_doc() {
    let missing = "docs/specs/no-such-spec.md";
    let o = run(
        Some(&binding(GATE_PATH, missing, Some("spec is here"))),
        &[],
        None,
    );
    assert_eq!(
        code(&o),
        1,
        "a binding to a doc that does not exist must not pass\n{}",
        all(&o)
    );
    assert!(
        all(&o).contains(missing),
        "the output must name the missing doc path so the dangling binding is \
         diagnosable\n{}",
        all(&o)
    );
}

// ---- 4. doc must be under docs/specs/, *.md, no `..` --------------------------------

#[test]
fn doc_outside_docs_specs_does_not_count() {
    let o = run(
        Some(&binding(
            GATE_PATH,
            "README.md",
            Some("readme describes it"),
        )),
        &[],
        None,
    );
    assert_eq!(
        code(&o),
        1,
        "an existing non-blank doc outside docs/specs/ (README.md) must not be a pass \
         source\n{}",
        all(&o)
    );
}

#[test]
fn doc_escaping_docs_specs_via_dotdot_does_not_count() {
    for doc in [
        "docs/specs/../../README.md",
        "docs/specs/../specs/blastguard.md",
    ] {
        let o = run(
            Some(&binding(GATE_PATH, doc, Some("dotdot escape"))),
            &[(DOC, DOC_BODY)],
            None,
        );
        assert_eq!(
            code(&o),
            1,
            "a doc path with a `..` component ({doc}) must not count, even when it \
             resolves to an existing file\n{}",
            all(&o)
        );
    }
}

#[test]
fn absolute_or_non_md_doc_does_not_count() {
    for absolute in [true, false] {
        let d = tempfile::tempdir().unwrap();
        let base = fixture(d.path());
        write_map_no_spec_doc(d.path());
        write_doc(d.path(), DOC);
        write(d.path(), "docs/specs/blastguard.txt", DOC_BODY);
        let doc = if absolute {
            d.path().join(DOC).display().to_string()
        } else {
            "docs/specs/blastguard.txt".to_string()
        };
        write(
            d.path(),
            SPEC_DOCS,
            &binding(GATE_PATH, &doc, Some("abs or non-md")),
        );
        commit_change(d.path());
        let o = gate_check(d.path(), &base);
        assert_eq!(
            code(&o),
            1,
            "doc {doc:?} (absolute, or not ending in .md) must not be a pass source\n{}",
            all(&o)
        );
    }
}

// ---- 5. doc must be non-blank ------------------------------------------------------

#[test]
fn empty_or_whitespace_doc_does_not_count() {
    for body in ["", "  \n\t\n"] {
        let o = run(
            Some(&binding(GATE_PATH, DOC, Some("describes it"))),
            &[(DOC, body)],
            None,
        );
        assert_eq!(
            code(&o),
            1,
            "a doc under docs/specs/ with blank content ({body:?}) must not be a pass \
             source\n{}",
            all(&o)
        );
    }
}

#[test]
fn doc_that_is_a_directory_does_not_count() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    write_map_no_spec_doc(d.path());
    // docs/specs/dir.md is a directory containing a file (so git tracks it).
    write_doc(d.path(), "docs/specs/dir.md/inner.md");
    write(
        d.path(),
        SPEC_DOCS,
        &binding(GATE_PATH, "docs/specs/dir.md", Some("dir")),
    );
    commit_change(d.path());
    let o = gate_check(d.path(), &base);
    assert_eq!(
        code(&o),
        1,
        "`doc` must exist as a FILE, not a directory\n{}",
        all(&o)
    );
}

// ---- 6. binding must name the triggered entry --------------------------------------

#[test]
fn binding_for_a_different_path_does_not_satisfy_the_entry() {
    let o = run(
        Some(&binding(
            "crates/notagate/src/b.rs",
            DOC,
            Some("describes b"),
        )),
        &[(DOC, DOC_BODY)],
        None,
    );
    assert_eq!(
        code(&o),
        1,
        "a binding naming an unrelated path must not satisfy the changed gate entry\n{}",
        all(&o)
    );
    assert!(
        all(&o).contains(GATE_PATH),
        "the unsatisfied gate path must still be reported\n{}",
        all(&o)
    );
}

// ---- 7. unparseable / unknown field is undetermined ---------------------------------

#[test]
fn unparseable_spec_docs_exits_2() {
    let o = run(Some("[[spec\npath = nope\n"), &[(DOC, DOC_BODY)], None);
    assert_eq!(
        code(&o),
        2,
        "an unparseable spec-docs.toml is undetermined (exit 2), not a violation or \
         pass\n{}",
        all(&o)
    );
}

#[test]
fn unknown_field_in_spec_docs_exits_2() {
    let body = format!(
        "{}extra = \"surprise\"\n",
        binding(GATE_PATH, DOC, Some("describes it"))
    );
    let o = run(Some(&body), &[(DOC, DOC_BODY)], None);
    assert_eq!(
        code(&o),
        2,
        "an unknown field (deny_unknown_fields) makes spec-docs.toml undetermined — a \
         typo'd key must never silently pass\n{}",
        all(&o)
    );
}

#[test]
fn unparseable_spec_docs_is_2_even_when_an_ack_would_pass() {
    let ack = format!("[[ack]]\npath = \"{GATE_PATH}\"\nreason = \"tracked in backlog\"\n");
    let o = run(Some("[[spec\n"), &[], Some(&ack));
    assert_eq!(
        code(&o),
        2,
        "an unreadable pass-source file must block (exit 2) even if another source \
         would pass — undetermined is never resolved to 0\n{}",
        all(&o)
    );
}

// ---- 8/9. controls -----------------------------------------------------------------

#[test]
fn control_absent_spec_docs_keeps_violation() {
    let o = run(None, &[(DOC, DOC_BODY)], None);
    assert_eq!(
        code(&o),
        1,
        "with no spec-docs.toml, no spec_doc and no ack the entry is still a violation \
         (an existing doc on disk alone is not a pass source)\n{}",
        all(&o)
    );
}

#[test]
fn control_ack_still_passes_when_spec_docs_has_no_binding_for_entry() {
    let ack = format!("[[ack]]\npath = \"{GATE_PATH}\"\nreason = \"spec pending\"\n");
    let o = run(
        Some(&binding(
            "crates/notagate/src/b.rs",
            DOC,
            Some("describes b"),
        )),
        &[(DOC, DOC_BODY)],
        Some(&ack),
    );
    assert_eq!(
        code(&o),
        0,
        "the existing reasoned-ack path must still pass alongside a spec-docs.toml \
         that does not bind the entry\n{}",
        all(&o)
    );
}

#[test]
fn control_empty_spec_docs_file_keeps_violation() {
    let o = run(Some(""), &[(DOC, DOC_BODY)], None);
    assert_eq!(
        code(&o),
        1,
        "an empty (zero-binding) spec-docs.toml is a valid empty set: violation, not \
         pass\n{}",
        all(&o)
    );
}

// ---- follow-up defects found in simulation ------------------------------------------

/// Real `map sync` maps key each entry by the file path itself, so a binding on
/// that path matches both the entry key AND the impl file. The rejection reason
/// for one binding must still be reported once, not once per match route.
#[test]
fn rejected_binding_reason_is_reported_once_when_key_equals_impl_path() {
    let missing = "docs/specs/x.md";
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    write(
        d.path(),
        ".specguard/spec-map.toml",
        &format!(
            "last_synced = \"x\"\n\n[entries.\"{GATE_PATH}\"]\nkey = \"{GATE_PATH}\"\n\
             status = \"changed\"\nimpl_files = [\"{GATE_PATH}\"]\n\n"
        ),
    );
    write(
        d.path(),
        SPEC_DOCS,
        &binding(GATE_PATH, missing, Some("spec lives here")),
    );
    commit_change(d.path());
    let o = gate_check(d.path(), &base);
    assert_eq!(
        code(&o),
        1,
        "a binding to a nonexistent doc is still a violation when key == impl path\n{}",
        all(&o)
    );
    let n = all(&o).matches(missing).count();
    assert_eq!(
        n,
        1,
        "the rejected binding's doc path must be reported exactly once (one binding, \
         one rejection reason), even though it matches via both the entry key and the \
         impl file; saw {n} occurrences\n{}",
        all(&o)
    );
}

/// An unreferenced changed gate path is undetermined (exit 2). Its remedy text
/// must point at the TRACKED pass source: `map set-spec` only writes the
/// gitignored map, which a push-time check on another machine never sees.
#[test]
fn unreferenced_gate_path_remedy_names_tracked_spec_docs_not_map_set_spec() {
    let d = tempfile::tempdir().unwrap();
    let base = fixture(d.path());
    // The map knows only OTHER_IMPL; the changed GATE_PATH is referenced by no entry.
    write(
        d.path(),
        ".specguard/spec-map.toml",
        &format!(
            "last_synced = \"x\"\n\n[entries.\"{OTHER_IMPL}\"]\nkey = \"{OTHER_IMPL}\"\n\
             status = \"changed\"\nimpl_files = [\"{OTHER_IMPL}\"]\n\n"
        ),
    );
    commit_change(d.path());
    let o = gate_check(d.path(), &base);
    assert_eq!(
        code(&o),
        2,
        "premise: a changed gate path referenced by no map entry is undetermined\n{}",
        all(&o)
    );
    assert!(
        all(&o).contains(SPEC_DOCS),
        "the exit-2 remedy must name {SPEC_DOCS} (the tracked place to bind a spec \
         doc)\n{}",
        all(&o)
    );
    assert!(
        !all(&o).contains("map set-spec"),
        "the exit-2 remedy must not recommend `map set-spec`, which writes only the \
         untracked map that the push-time check cannot rely on\n{}",
        all(&o)
    );
}
