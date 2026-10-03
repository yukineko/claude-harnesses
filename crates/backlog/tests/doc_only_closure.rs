//! `done ID --doc-only COMMIT`: the commit must be an ancestor of HEAD,
//! non-root, non-merge, and touch ONLY doc paths. Prompt-bearing markdown
//! (SKILL.md, anything under agents/ commands/ skills/) and comments in code
//! files are CODE, never doc.
mod common;
use common::*;

fn commit_one(f: &Fixture, path: &str, body: &str) -> String {
    f.write(path, body);
    f.commit_paths(&[path], &format!("touch {path}"))
}

fn expect_refused(tag: &str, path: &str, body: &str) {
    let f = Fixture::new(tag);
    let c = commit_one(&f, path, body);
    let id = f.add("t");
    let o = f.run(&["done", &id, "--doc-only", &c]);
    assert_refused_unchanged(&f, &id, "pending", &o, path);
}

fn expect_accepted(tag: &str, path: &str) {
    let f = Fixture::new(tag);
    let c = commit_one(&f, path, "# doc\n\nprose only\n");
    let id = f.add("t");
    let o = f.run(&["done", &id, "--doc-only", &c]);
    assert_eq!(o.code, 0, "{path} is a plain doc: {}", o.both());
    assert_eq!(f.status(&id), "done");
    let cl = f.row(&id)["closure"].clone();
    assert_eq!(cl["doc_only_commit"], c.as_str(), "{cl}");
    assert!(cl["reason"].as_str().unwrap_or("").contains("doc"), "{cl}");
}

#[test]
fn plain_docs_paths_are_accepted() {
    expect_accepted("d1", "docs/guide.md");
    expect_accepted("d2", "README.md");
    expect_accepted("d3", "crates/foo/README.ja.md");
    expect_accepted("d4", "docs/deep/nested/notes.txt");
}

#[test]
fn skill_md_is_code_not_doc() {
    expect_refused("s1", "crates/x/skills/foo/SKILL.md", "# skill\n");
    expect_refused("s2", "SKILL.md", "# skill\n");
}

#[test]
fn md_under_agents_commands_skills_is_code_not_doc() {
    expect_refused("a1", "crates/x/agents/worker.md", "# agent prompt\n");
    expect_refused("a2", "crates/x/commands/run.md", "# command prompt\n");
    expect_refused("a3", "crates/x/skills/foo/notes.md", "# skill notes\n");
}

#[test]
fn comment_only_change_in_a_rs_file_is_code() {
    let f = Fixture::new("rs");
    f.write("src/lib.rs", "fn a() {}\n");
    f.commit_paths(&["src/lib.rs"], "add lib");
    let c = commit_one(&f, "src/lib.rs", "// only a comment was added\nfn a() {}\n");
    let id = f.add("t");
    let o = f.run(&["done", &id, "--doc-only", &c]);
    assert_refused_unchanged(
        &f,
        &id,
        "pending",
        &o,
        "comments/docstrings in code files are code",
    );
}

#[test]
fn mixed_doc_and_code_commit_is_refused() {
    let f = Fixture::new("mixed");
    f.write("docs/a.md", "doc\n");
    f.write("src/lib.rs", "fn a() {}\n");
    let c = f.commit_paths(&["docs/a.md", "src/lib.rs"], "doc and code");
    let id = f.add("t");
    let o = f.run(&["done", &id, "--doc-only", &c]);
    assert_refused_unchanged(&f, &id, "pending", &o, "one code path taints the commit");
}

#[test]
fn root_commit_is_refused() {
    let f = Fixture::new("root");
    let root = f.git(&["rev-list", "--max-parents=0", "HEAD"]);
    let id = f.add("t");
    let o = f.run(&["done", &id, "--doc-only", &root]);
    assert_refused_unchanged(&f, &id, "pending", &o, "root commit");
}

#[test]
fn merge_commit_is_refused() {
    let f = Fixture::new("merge");
    f.git(&["checkout", "-q", "-b", "side"]);
    commit_one(&f, "docs/side.md", "side\n");
    f.git(&["checkout", "-q", "main"]);
    commit_one(&f, "docs/main.md", "main\n");
    f.git(&[
        "merge",
        "-q",
        "--no-ff",
        "--no-verify",
        "-m",
        "merge side",
        "side",
    ]);
    let m = f.head();
    let id = f.add("t");
    let o = f.run(&["done", &id, "--doc-only", &m]);
    assert_refused_unchanged(&f, &id, "pending", &o, "merge commit");
}

#[test]
fn non_ancestor_and_unknown_commits_are_refused() {
    let f = Fixture::new("anc");
    f.git(&["checkout", "-q", "-b", "side"]);
    let side = commit_one(&f, "docs/side.md", "side\n");
    f.git(&["checkout", "-q", "main"]);
    commit_one(&f, "docs/main.md", "main\n");
    let id = f.add("t");
    let o = f.run(&["done", &id, "--doc-only", &side]);
    assert_refused_unchanged(&f, &id, "pending", &o, "not an ancestor of HEAD");
    let o = f.run(&[
        "done",
        &id,
        "--doc-only",
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    ]);
    assert_refused_unchanged(&f, &id, "pending", &o, "unknown commit");
    let o = f.run(&["done", &id, "--doc-only", "HEAD; true"]);
    assert_refused_unchanged(&f, &id, "pending", &o, "garbage commit");
}
