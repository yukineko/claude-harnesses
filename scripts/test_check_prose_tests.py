#!/usr/bin/env python3
"""Behavioural tests for scripts/check-prose-tests.py (prose-to-test citation gate).

Contract: the spec in the task (exit 0 clean / 1 findings / 2 undetermined).
Every test runs the gate as a subprocess against a throwaway git repo.
"""
import os
import pathlib
import subprocess
import sys

import pytest

SCRIPT = pathlib.Path(__file__).resolve().parent / "check-prose-tests.py"
PLACEHOLDER = "Never skip the hook.\n"

UNDET = "check-prose-tests: UNDETERMINED:"
CLAIM = "The gate blocks unsigned commits."


def git(repo, *args):
    subprocess.run(["git", "-C", str(repo), *args], check=True,
                   capture_output=True, text=True)


def init_repo(path):
    path.mkdir(parents=True, exist_ok=True)
    git(path, "init", "-q")
    git(path, "config", "user.name", "t")
    git(path, "config", "user.email", "t@example.com")
    git(path, "config", "commit.gpgsign", "false")
    return path


def write(repo, rel, text):
    p = repo / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text, encoding="utf-8")
    return p


def commit_all(repo, msg="base"):
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", msg)


def make_repo(tmp_path, files, extra=None):
    """Commit placeholders for `files`, then rewrite them (modified vs HEAD).

    `extra` is a dict of other committed files (tests, fixtures).
    """
    repo = init_repo(tmp_path / "repo")
    for rel, _ in files.items():
        write(repo, rel, PLACEHOLDER)
    for rel, text in (extra or {}).items():
        write(repo, rel, text)
    commit_all(repo)
    for rel, text in files.items():
        write(repo, rel, text)
    return repo


def run(repo, *args):
    return subprocess.run([sys.executable, str(SCRIPT), "--repo", str(repo), *args],
                          capture_output=True, text=True, timeout=60)


RUST_TEST = "#[test]\nfn x() {}\n"
PY_TEST = "def test_x():\n    pass\n"


# 1 ------------------------------------------------------------------
def test_uncited_claim_in_changed_file_blocks(tmp_path):
    repo = make_repo(tmp_path, {"CLAUDE.md": CLAIM + "\n"})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    lines = [l for l in r.stdout.splitlines() if "uncited-claim" in l]
    assert lines and all("CLAUDE.md" in l for l in lines), r.stdout


# 2 ------------------------------------------------------------------
def test_cited_claim_with_existing_rust_test_passes(tmp_path):
    repo = make_repo(
        tmp_path,
        {"CLAUDE.md": f"{CLAIM} [test: crates/foo/tests/t.rs::x]\n"},
        {"crates/foo/tests/t.rs": RUST_TEST})
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_cited_claim_with_existing_python_test_passes(tmp_path):
    repo = make_repo(
        tmp_path,
        {"CLAUDE.md": f"{CLAIM} [test: scripts/test_y.py::test_x]\n"},
        {"scripts/test_y.py": PY_TEST})
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 3 ------------------------------------------------------------------
def test_dangling_citation_file_missing(tmp_path):
    repo = make_repo(tmp_path,
                     {"CLAUDE.md": f"{CLAIM} [test: crates/foo/tests/nope.rs::x]\n"})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "dangling-citation" in r.stdout


def test_dangling_citation_named_fn_absent(tmp_path):
    repo = make_repo(
        tmp_path,
        {"CLAUDE.md": f"{CLAIM} [test: crates/foo/tests/t.rs::missing_fn]\n"},
        {"crates/foo/tests/t.rs": RUST_TEST})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "dangling-citation" in r.stdout


def test_dangling_citation_rust_fn_without_test_attribute(tmp_path):
    repo = make_repo(
        tmp_path,
        {"CLAUDE.md": f"{CLAIM} [test: crates/foo/tests/t.rs::helper]\n"},
        {"crates/foo/tests/t.rs": "fn helper() {}\n\n#[test]\nfn other() {}\n"})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "dangling-citation" in r.stdout


def test_dangling_citation_rust_fn_with_non_test_attribute(tmp_path):
    repo = make_repo(
        tmp_path,
        {"CLAUDE.md": f"{CLAIM} [test: crates/foo/tests/t.rs::x]\n"},
        {"crates/foo/tests/t.rs": "#[inline]\nfn x() {}\n"})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "dangling-citation" in r.stdout


def test_dangling_citation_python_def_not_a_test_name(tmp_path):
    repo = make_repo(
        tmp_path,
        {"CLAUDE.md": f"{CLAIM} [test: scripts/test_y.py::helper]\n"},
        {"scripts/test_y.py": "def helper():\n    pass\n"})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "dangling-citation" in r.stdout


@pytest.mark.parametrize("line", [
    "1. **Run the tests.**",
    "2. コミット前にテストを実行すること。",
    "判定不能なら 3. に従って制限側へ倒すこと。",
])
def test_numbered_item_and_section_ref_imperatives_pass(tmp_path, line):
    repo = make_repo(tmp_path, {"CLAUDE.md": line + "\n"})
    r = run(repo)
    assert r.returncode == 0, (line, r.stdout, r.stderr)


def test_digit_period_before_ascii_capital_still_splits(tmp_path):
    repo = make_repo(tmp_path, {"CLAUDE.md": "The gate exits 2. Run it again.\n"})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    found = [l for l in r.stdout.splitlines() if "uncited-claim" in l]
    assert len(found) == 1, r.stdout
    assert "The gate exits 2" in found[0]


# 4 ------------------------------------------------------------------
@pytest.mark.parametrize("sentence", [
    "Never skip the hook.",
    "コミット前にテストを実行すること。",
    "迂回は禁止。",
])
def test_imperatives_pass(tmp_path, sentence):
    repo = make_repo(tmp_path, {"CLAUDE.md": sentence + "\n"})
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 5 ------------------------------------------------------------------
def test_unreadable_covered_file_is_undetermined(tmp_path):
    if hasattr(os, "geteuid") and os.geteuid() == 0:
        pytest.skip("running as root")
    repo = make_repo(tmp_path, {"CLAUDE.md": CLAIM + "\n"})
    p = repo / "CLAUDE.md"
    p.chmod(0o000)
    try:
        try:
            p.read_text()
            pytest.skip("file still readable after chmod 000")
        except PermissionError:
            pass
        r = run(repo, "CLAUDE.md")
        assert r.returncode == 2, (r.stdout, r.stderr)
        assert UNDET in r.stderr, r.stderr
    finally:
        p.chmod(0o644)


# 6 ------------------------------------------------------------------
def test_non_git_directory_default_mode_is_undetermined(tmp_path):
    d = tmp_path / "plain"
    write(d, "CLAUDE.md", PLACEHOLDER)
    r = run(d)
    assert r.returncode == 2, (r.stdout, r.stderr)
    assert UNDET in r.stderr, r.stderr


def test_git_repo_without_head_default_mode_is_undetermined(tmp_path):
    repo = init_repo(tmp_path / "nohead")
    write(repo, "CLAUDE.md", PLACEHOLDER)
    r = run(repo)
    assert r.returncode == 2, (r.stdout, r.stderr)
    assert UNDET in r.stderr, r.stderr


# 7 ------------------------------------------------------------------
def test_record_marker_dated_exempt(tmp_path):
    repo = make_repo(tmp_path, {"CLAUDE.md": "実測 2026-07-21 にゲートが X を通した。\n"})
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_record_block_exempts_claims(tmp_path):
    # A record block must carry `rev:` (user ruling 2026-10-15); a rev-less
    # opener is covered by test_block_without_rev_is_malformed_and_not_exempt.
    text = ("<!-- record: 2026-07-21 rev: {full} -->\n"
            "The gate blocked unsigned commits.\n\n"
            "Another observed claim here.\n"
            "<!-- /record -->\n")
    repo = _rev_repo(tmp_path, text)
    r = run(repo, "--all")
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_undated_record_like_sentence_is_claim(tmp_path):
    repo = make_repo(tmp_path, {"CLAUDE.md": "実測で X は Y を返した。\n"})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout


def test_invalid_date_marker_is_claim(tmp_path):
    repo = make_repo(tmp_path, {"CLAUDE.md": "実測 2026-13-45 にゲートが X を通した。\n"})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout


def test_mid_sentence_marker_is_claim(tmp_path):
    repo = make_repo(tmp_path,
                     {"CLAUDE.md": "ゲートは 実測 2026-07-21 に X を通した。\n"})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout


def test_unterminated_record_block_is_malformed(tmp_path):
    text = "<!-- record: 2026-07-21 -->\nThe gate blocked things.\n"
    repo = make_repo(tmp_path, {"CLAUDE.md": text})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "malformed-record-block" in r.stdout


# 8 ------------------------------------------------------------------
def test_all_with_empty_covered_set_is_undetermined(tmp_path):
    repo = init_repo(tmp_path / "repo")
    write(repo, "notes/x.md", CLAIM + "\n")
    commit_all(repo)
    r = run(repo, "--all")
    assert r.returncode == 2, (r.stdout, r.stderr)
    assert UNDET in r.stderr, r.stderr


def test_all_scans_unchanged_file_but_default_mode_does_not(tmp_path):
    repo = init_repo(tmp_path / "repo")
    write(repo, "CLAUDE.md", CLAIM + "\n")
    commit_all(repo)
    a = run(repo, "--all")
    assert a.returncode == 1, (a.stdout, a.stderr)
    assert "uncited-claim" in a.stdout and "CLAUDE.md" in a.stdout
    d = run(repo)
    assert d.returncode == 0, (d.stdout, d.stderr)


# 9 ------------------------------------------------------------------
def test_fenced_code_blocks_not_scanned(tmp_path):
    text = f"```\n{CLAIM}\n```\n\n~~~\n{CLAIM}\n~~~\n"
    repo = make_repo(tmp_path, {"CLAUDE.md": text})
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_html_comments_not_scanned(tmp_path):
    text = f"<!-- {CLAIM} -->\n\n<!--\n{CLAIM}\n-->\n"
    repo = make_repo(tmp_path, {"CLAUDE.md": text})
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 10 -----------------------------------------------------------------
def test_non_covered_file_ignored(tmp_path):
    repo = make_repo(tmp_path, {"notes/x.md": CLAIM + "\n"})
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 11 -----------------------------------------------------------------
@pytest.mark.parametrize("rel", [
    "CLAUDE.md",
    "docs/sub/dir/x.md",
    "crates/foo/README.ja.md",
    "crates/foo/skills/bar/SKILL.md",
])
def test_covered_paths_are_checked(tmp_path, rel):
    repo = make_repo(tmp_path, {rel: CLAIM + "\n"})
    r = run(repo)
    assert r.returncode == 1, (rel, r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout and rel in r.stdout


# 12 -----------------------------------------------------------------
SKILL = "crates/foo/skills/bar/SKILL.md"


def test_skill_front_matter_name_is_not_a_claim(tmp_path):
    text = "---\nname: bar\n---\n\nNever skip the hook.\n"
    repo = make_repo(tmp_path, {SKILL: text})
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_skill_front_matter_description_claim_blocks(tmp_path):
    text = f"---\nname: bar\ndescription: {CLAIM}\n---\n\nNever skip the hook.\n"
    repo = make_repo(tmp_path, {SKILL: text})
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout


# ===== AMENDMENT 2026-10-15 ===========================================
CITE = "[test: crates/foo/tests/t.rs::x]"
RS = {"crates/foo/tests/t.rs": RUST_TEST}


def one_line(tmp_path, line, rel="CLAUDE.md", extra=None):
    return run(make_repo(tmp_path, {rel: line + "\n"}, extra))


# A1 -----------------------------------------------------------------
@pytest.mark.parametrize("line", [
    "Run `cargo test` before committing.",
    "Disable it with TASKPROG_DISABLED=1.",
    "Edit taskprog.toml to change it.",
    "Pass --dry-run to preview.",
    "Run /taskprog to refresh.",
    "Read scripts/x first.",
    "**Stop** で進捗ファイルを更新すること。",
    "Stop and ask the user.",
])
def test_a1_mechanism_naming_imperative_needs_citation(tmp_path, line):
    r = one_line(tmp_path, line)
    assert r.returncode == 1, (line, r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout


def test_a1_mechanism_imperative_with_valid_citation_passes(tmp_path):
    r = one_line(tmp_path, f"Run `cargo test` before committing. {CITE}", extra=RS)
    assert r.returncode == 0, (r.stdout, r.stderr)


@pytest.mark.parametrize("line", ["Never skip the hook.", "迂回は禁止。"])
def test_a1_plain_imperative_controls_still_pass(tmp_path, line):
    r = one_line(tmp_path, line)
    assert r.returncode == 0, (line, r.stdout, r.stderr)


def test_a1_dated_record_naming_mechanism_stays_exempt(tmp_path):
    r = one_line(tmp_path, "実測 2026-07-21 `cargo build` が失敗した。")
    assert r.returncode == 0, (r.stdout, r.stderr)


# A1b: Japanese prohibitions ------------------------------------------
@pytest.mark.parametrize("line", [
    "main の作業ツリーで直接編集しない。",
    "迂回を禁止する。",
    "Do not skip the hook.",
])
def test_a1b_prohibitions_are_imperatives(tmp_path, line):
    r = one_line(tmp_path, line)
    assert r.returncode == 0, (line, r.stdout, r.stderr)


@pytest.mark.parametrize("line", [
    "`cargo build` を省略しない。",
    "Do not pass --no-verify.",
])
def test_a1b_prohibition_naming_mechanism_needs_citation(tmp_path, line):
    r = one_line(tmp_path, line)
    assert r.returncode == 1, (line, r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout


# A2 -----------------------------------------------------------------
@pytest.mark.parametrize("line", [
    "The detector must refuse destructive commands.",
    "検出器は破壊的コマンドを拒否しなければならない。",
])
def test_a2_normative_in_docs_specs_is_imperative(tmp_path, line):
    r = one_line(tmp_path, line, rel="docs/specs/x.md")
    assert r.returncode == 0, (line, r.stdout, r.stderr)


def test_a2_normative_naming_mechanism_needs_citation(tmp_path):
    r = one_line(tmp_path, "The detector must call `detect`.", rel="docs/specs/x.md")
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout


def test_a2_normative_outside_docs_specs_is_claim(tmp_path):
    r = one_line(tmp_path, "The detector must refuse destructive commands.",
                 rel="docs/other.md")
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout


# A3 -----------------------------------------------------------------
def fenced(tmp_path, info, body, extra=None):
    text = f"Never skip the hook.\n\n```{info}\n{body}\n```\n"
    return run(make_repo(tmp_path, {"CLAUDE.md": text}, extra))


@pytest.mark.parametrize("info,body", [
    ("sh", "taskprog show   # print the current progress file"),
    ("toml", "inject_limit = 4096  # bytes injected"),
    ("rust", "// returns the value"),
    ("", "# a claim here"),
])
def test_a3_uncited_comment_in_fence_is_claim(tmp_path, info, body):
    r = fenced(tmp_path, info, body)
    assert r.returncode == 1, (info, r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout


def test_a3_cited_comment_in_fence_passes(tmp_path):
    r = fenced(tmp_path, "sh",
               f"taskprog show   # print the current progress file {CITE}", RS)
    assert r.returncode == 0, (r.stdout, r.stderr)


@pytest.mark.parametrize("info,body", [
    ("markdown", "# Progress"),
    ("json", '{"a": 1}\n// returns the value'),
    ("sh", 'echo "# not a comment"'),
    ("sh", "#!/bin/sh\necho hi"),
])
def test_a3_non_comments_and_other_languages_not_scanned(tmp_path, info, body):
    r = fenced(tmp_path, info, body)
    assert r.returncode == 0, (info, r.stdout, r.stderr)


def test_a3_reported_line_is_comment_line_in_markdown(tmp_path):
    text = "Never skip the hook.\n\n```sh\necho hi\ntaskprog show # prints progress\n```\n"
    r = run(make_repo(tmp_path, {"CLAUDE.md": text}))
    assert r.returncode == 1, (r.stdout, r.stderr)
    lines = [l for l in r.stdout.splitlines() if "uncited-claim" in l]
    assert len(lines) == 1 and lines[0].startswith("CLAUDE.md:5:"), r.stdout


@pytest.mark.parametrize("body", ['echo "a # b"', "echo 'a # b'"])
def test_a3_hash_inside_quotes_is_not_a_comment(tmp_path, body):
    r = fenced(tmp_path, "sh", body)
    assert r.returncode == 0, (body, r.stdout, r.stderr)


def test_a3_empty_info_string_scans_slash_comments(tmp_path):
    r = fenced(tmp_path, "", "// a claim here")
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout


# ---------------------------------------------------------------------
# AMENDMENT B: diff-scoped default mode. Default mode checks only the
# units whose span contains a line ADDED or MODIFIED vs HEAD; --all and
# explicit FILE args still check every unit.
# ---------------------------------------------------------------------
BASE_CLAIMS = [
    "The gate blocks unsigned commits.",
    "The hook rejects empty messages.",
    "The scanner flags stale locks.",
    "The runner retries failed jobs.",
    "The store keeps every ledger entry.",
]


def committed_claims(tmp_path, claims=BASE_CLAIMS, extra=None):
    """Commit CLAUDE.md with one uncited claim per paragraph; the working
    tree starts identical to HEAD (so the diff starts empty)."""
    repo = init_repo(tmp_path / "repo")
    write(repo, "CLAUDE.md", "\n\n".join(claims) + "\n")
    for rel, text in (extra or {}).items():
        write(repo, rel, text)
    commit_all(repo)
    return repo


def uncited_lines(r):
    return [l for l in r.stdout.splitlines() if "uncited-claim" in l]


# A1
def test_b_a1_modified_claim_is_the_only_finding(tmp_path):
    repo = committed_claims(tmp_path)
    claims = list(BASE_CLAIMS)
    claims[2] = "The scanner flags every stale lock."  # 3rd paragraph -> line 5
    write(repo, "CLAUDE.md", "\n\n".join(claims) + "\n")
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    found = uncited_lines(r)
    assert len(found) == 1, r.stdout
    assert "CLAUDE.md:5:" in found[0], found


# A2
def test_b_a2_added_claim_is_the_only_finding(tmp_path):
    repo = committed_claims(tmp_path)
    text = "\n\n".join(BASE_CLAIMS) + "\n\nThe cache evicts the oldest entry.\n"
    write(repo, "CLAUDE.md", text)
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    found = uncited_lines(r)
    assert len(found) == 1, r.stdout
    assert "CLAUDE.md:11:" in found[0], found


# A3
def test_b_a3_citing_one_claim_leaves_others_unreported(tmp_path):
    repo = committed_claims(tmp_path, extra={"scripts/test_y.py": PY_TEST})
    claims = list(BASE_CLAIMS)
    claims[1] = BASE_CLAIMS[1] + " [test: scripts/test_y.py::test_x]"
    write(repo, "CLAUDE.md", "\n\n".join(claims) + "\n")
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# A4
def test_b_a4_all_still_reports_every_claim(tmp_path):
    repo = committed_claims(tmp_path)
    claims = list(BASE_CLAIMS)
    claims[2] = "The scanner flags every stale lock."
    write(repo, "CLAUDE.md", "\n\n".join(claims) + "\n")
    r = run(repo, "--all")
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert len(uncited_lines(r)) == len(BASE_CLAIMS), r.stdout


def test_b_a4b_explicit_file_arg_still_reports_every_claim(tmp_path):
    repo = committed_claims(tmp_path)
    claims = list(BASE_CLAIMS)
    claims[2] = "The scanner flags every stale lock."
    write(repo, "CLAUDE.md", "\n\n".join(claims) + "\n")
    r = run(repo, "CLAUDE.md")
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert len(uncited_lines(r)) == len(BASE_CLAIMS), r.stdout


# A5
def test_b_a5_new_staged_file_every_claim_is_checked(tmp_path):
    repo = init_repo(tmp_path / "repo")
    write(repo, "README.md", "placeholder\n")
    commit_all(repo)
    write(repo, "CLAUDE.md",
          "The gate blocks unsigned commits.\n\nThe hook rejects empty messages.\n")
    git(repo, "add", "CLAUDE.md")
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert len(uncited_lines(r)) == 2, r.stdout


# A6
def test_b_a6_modifying_second_physical_line_reports_the_sentence(tmp_path):
    repo = committed_claims(
        tmp_path, claims=["The gate blocks\nunsigned commits.", "The hook rejects empty messages."])
    write(repo, "CLAUDE.md",
          "The gate blocks\nunsigned merges.\n\nThe hook rejects empty messages.\n")
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    found = uncited_lines(r)
    assert len(found) == 1, r.stdout
    assert "CLAUDE.md:1:" in found[0], found


# A7
def test_b_a7_deleting_middle_line_of_sentence_reports_the_sentence(tmp_path):
    repo = committed_claims(
        tmp_path,
        claims=["The gate blocks\nevery single\nunsigned commit.",
                "The hook rejects empty messages."])
    write(repo, "CLAUDE.md",
          "The gate blocks\nunsigned commit.\n\nThe hook rejects empty messages.\n")
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    found = uncited_lines(r)
    assert len(found) == 1, r.stdout
    assert "CLAUDE.md:1:" in found[0], found


def test_b_unrelated_edit_elsewhere_in_file_reports_nothing(tmp_path):
    repo = committed_claims(tmp_path, claims=BASE_CLAIMS + ["Run the build."])
    write(repo, "CLAUDE.md",
          "\n\n".join(BASE_CLAIMS + ["Run the build now."]) + "\n")
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# Record rev (user ruling 2026-10-15): a record carries the commit at which
# the recorded observation was true. ---------------------------------------
def _head(repo):
    return subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                          check=True, capture_output=True, text=True).stdout.strip()


def _rev_repo(tmp_path, template):
    """Fixture repo whose CLAUDE.md is `template` with {rev} filled by HEAD sha
    (computed from a committed placeholder repo). `template` may use {full}/{short}."""
    repo = make_repo(tmp_path, {"CLAUDE.md": PLACEHOLDER})
    sha = _head(repo)
    write(repo, "CLAUDE.md", template.format(full=sha, short=sha[:8]))
    return repo


@pytest.mark.parametrize("which", ["full", "short"])
def test_rev_block_with_resolvable_rev_exempts_claims(tmp_path, which):
    repo = _rev_repo(tmp_path,
                     "<!-- record: 2026-07-21 rev: {" + which + "} -->\n"
                     "The gate blocked unsigned commits.\n"
                     "<!-- /record -->\n")
    r = run(repo, "--all")
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_block_without_rev_is_malformed_and_not_exempt(tmp_path):
    text = ("<!-- record: 2026-10-01 -->\n"
            "The gate blocked unsigned commits.\n"
            "<!-- /record -->\n")
    repo = make_repo(tmp_path, {"CLAUDE.md": text})
    r = run(repo, "--all")
    assert r.returncode != 0, (r.stdout, r.stderr)
    assert "malformed-record-block" in r.stdout, r.stdout
    assert "rev" in r.stdout, r.stdout
    assert "uncited-claim" in r.stdout, r.stdout  # contents not exempt


def test_block_with_unresolvable_rev_is_malformed_and_not_exempt(tmp_path):
    text = ("<!-- record: 2026-10-01 rev: deadbeef -->\n"
            "The gate blocked unsigned commits.\n"
            "<!-- /record -->\n")
    repo = make_repo(tmp_path, {"CLAUDE.md": text})
    r = run(repo, "--all")
    assert r.returncode != 0, (r.stdout, r.stderr)
    assert "malformed-record-block" in r.stdout, r.stdout
    assert "deadbeef" in r.stdout or "rev" in r.stdout, r.stdout
    assert "uncited-claim" in r.stdout, r.stdout


def test_block_with_invalid_date_and_valid_rev_is_malformed(tmp_path):
    repo = _rev_repo(tmp_path,
                     "<!-- record: 2026-13-45 rev: {full} -->\n"
                     "The gate blocked unsigned commits.\n"
                     "<!-- /record -->\n")
    r = run(repo, "--all")
    assert r.returncode != 0, (r.stdout, r.stderr)
    assert "malformed-record-block" in r.stdout, r.stdout
    assert "uncited-claim" in r.stdout, r.stdout


@pytest.mark.parametrize("which", ["full", "short"])
def test_inline_record_with_resolvable_rev_is_exempt(tmp_path, which):
    repo = _rev_repo(tmp_path,
                     "[record: 2026-07-21 rev: {" + which + "}] The gate blocked unsigned commits.\n")
    r = run(repo, "--all")
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_inline_record_with_unresolvable_rev_is_reported(tmp_path):
    repo = make_repo(tmp_path, {
        "CLAUDE.md": "[record: 2026-07-21 rev: deadbeef] The gate blocked unsigned commits.\n"})
    r = run(repo, "--all")
    assert r.returncode != 0, (r.stdout, r.stderr)
    assert "CLAUDE.md" in r.stdout, r.stdout


def test_inline_record_with_invalid_date_and_valid_rev_not_exempt(tmp_path):
    repo = _rev_repo(tmp_path,
                     "[record: 2026-13-45 rev: {full}] The gate blocked unsigned commits.\n")
    r = run(repo, "--all")
    assert r.returncode != 0, (r.stdout, r.stderr)
    assert "uncited-claim" in r.stdout, r.stdout


def test_inline_record_without_rev_still_accepted_control(tmp_path):
    repo = make_repo(tmp_path, {
        "CLAUDE.md": "[record: 2026-07-21] The gate blocked unsigned commits.\n"})
    r = run(repo, "--all")
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_date_only_inline_marker_still_works_control(tmp_path):
    repo = make_repo(tmp_path, {"CLAUDE.md": "実測 2026-10-01 にゲートが X を通した。\n"})
    r = run(repo, "--all")
    assert r.returncode == 0, (r.stdout, r.stderr)
