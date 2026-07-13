//! End-to-end coverage for `condukt review-order` in its primary, hermetic
//! `--diff-file` mode. This test fails before the `review-order` subcommand
//! exists (unrecognized subcommand -> clap nonzero exit, no `hunks` JSON on
//! stdout) and passes once it is wired = a genuine Fail->Pass reproduction
//! oracle for the feature.
//!
//! Fixture: `a.rs` defines `helper`, `b.rs` calls it — arranged so the
//! DEFINER hunk (a.rs) must be ordered strictly before the REFERENCER hunk
//! (b.rs) in the printed review order.

use std::fs;
use std::process::Command;

const DIFF_FIXTURE: &str = "\
diff --git a/a.rs b/a.rs
new file mode 100644
index 0000000..1111111
--- /dev/null
+++ b/a.rs
@@ -0,0 +1,3 @@
+fn helper() -> i32 {
+    42
+}
diff --git a/b.rs b/b.rs
new file mode 100644
index 0000000..2222222
--- /dev/null
+++ b/b.rs
@@ -0,0 +1,3 @@
+fn user() {
+    let _ = helper();
+}
";

const A_RS: &str = "fn helper() -> i32 {\n    42\n}\n";
const B_RS: &str = "fn user() {\n    let _ = helper();\n}\n";

fn run_review_order(cwd: &std::path::Path, diff_path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_condukt"))
        .arg("review-order")
        .arg("--diff-file")
        .arg(diff_path)
        .arg("--json")
        .current_dir(cwd)
        .output()
        .expect("spawn condukt")
}

#[test]
fn definer_hunk_ordered_before_referencer_hunk_and_deterministic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let diff_path = dir.path().join("fixture.diff");
    fs::write(&diff_path, DIFF_FIXTURE).expect("write diff fixture");
    fs::write(dir.path().join("a.rs"), A_RS).expect("write a.rs");
    fs::write(dir.path().join("b.rs"), B_RS).expect("write b.rs");

    let out = run_review_order(dir.path(), &diff_path);
    assert!(
        out.status.success(),
        "review-order failed: {out:?}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON, got {e}: {stdout}"));

    let hunks = val["hunks"].as_array().expect("hunks array");
    assert_eq!(hunks.len(), 2, "every hunk appears; got: {val}");

    // Every hunk_index appears exactly once.
    let mut hunk_indices: Vec<i64> = hunks
        .iter()
        .map(|h| h["hunk_index"].as_i64().expect("hunk_index"))
        .collect();
    hunk_indices.sort();
    assert_eq!(hunk_indices, vec![0, 1]);

    let a_hunk = hunks
        .iter()
        .find(|h| h["file"] == "a.rs")
        .expect("a.rs hunk present");
    let b_hunk = hunks
        .iter()
        .find(|h| h["file"] == "b.rs")
        .expect("b.rs hunk present");

    assert!(
        a_hunk["defines"]
            .as_array()
            .expect("defines array")
            .iter()
            .any(|s| s == "helper"),
        "a.rs hunk should be attributed the 'helper' definition; got: {a_hunk}"
    );

    let a_pos = a_hunk["position"].as_i64().expect("position");
    let b_pos = b_hunk["position"].as_i64().expect("position");
    assert!(
        a_pos < b_pos,
        "definer (a.rs, pos {a_pos}) must come before referencer (b.rs, pos {b_pos})"
    );

    // Determinism: running twice yields byte-identical output.
    let out2 = run_review_order(dir.path(), &diff_path);
    assert_eq!(out.stdout, out2.stdout, "two runs must be byte-identical");
}

#[test]
fn subcommand_reports_clusters_count() {
    let dir = tempfile::tempdir().expect("tempdir");
    let diff_path = dir.path().join("fixture.diff");
    fs::write(&diff_path, DIFF_FIXTURE).expect("write diff fixture");
    fs::write(dir.path().join("a.rs"), A_RS).expect("write a.rs");
    fs::write(dir.path().join("b.rs"), B_RS).expect("write b.rs");

    let out = run_review_order(dir.path(), &diff_path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        val["clusters"], 1,
        "the two dependency-linked hunks form a single cluster; got: {val}"
    );
}
