#!/usr/bin/env python3
"""Behavioural tests for scripts/check-doc-duplication.py (doc-duplication ratchet gate).

Contract: the spec in the task (exit 0 clean / 1 findings / 2 undetermined).
Every test runs the gate as a subprocess, with cwd inside a throwaway git repo.
DUPGATE_SCRIPT overrides the script under test (used to prove the suite can go RED).
"""
import os
import pathlib
import re
import subprocess
import sys

import pytest

SCRIPT = pathlib.Path(os.environ.get(
    "DUPGATE_SCRIPT",
    str(pathlib.Path(__file__).resolve().parent / "check-doc-duplication.py")))

UNDET = "check-doc-duplication: UNDETERMINED:"

SKILL_A = "crates/a/skills/s1/SKILL.md"
SKILL_B = "crates/b/skills/s2/SKILL.md"

# Three-line paragraph (instruction prose), well over 80 normalised chars.
PARA_LINES = [
    "When the verifier reports UNVERIFIED, record the item in the review queue and do not close it;",
    "re-run the verification with an independent model before any status change is made to the ticket,",
    "and attach the verbatim quotation of the code path that was traced so the next reviewer can retrace it.",
]
PARA = "\n".join(PARA_LINES) + "\n"
PARA_ONE_WORD = PARA.replace("UNVERIFIED", "UNCERTAIN")

OTHER_LINES = [
    "Rollout copies the freshly built binary into the versioned plugin cache directory and repoints the registry entry,",
    "then the launcher wrapper is executed once to confirm that the deployed version matches the committed manifest,",
    "and the deployment provenance file is written next to the binary with the commit hash and the dirty flag.",
]
OTHER = "\n".join(OTHER_LINES) + "\n"

THIRD = ("Mutation testing deletes one construct at a time from the implementation and expects at least one "
         "test to turn red; a surviving mutant means the suite never observed that construct at all.\n")

K, MINLEN, THRESH = 5, 80, 0.6


# --- reference similarity (test-side, independent of the gate) -------------
def norm(text):
    t = text.lower()
    t = re.sub(r"[`*_>#|\-\[\]()~:=]+", " ", t)
    return re.sub(r"\s+", "", t)


def shingles(text):
    n = norm(text)
    return {n[i:i + K] for i in range(len(n) - K + 1)}


def jaccard(a, b):
    sa, sb = shingles(a), shingles(b)
    return len(sa & sb) / len(sa | sb)


def mix(a, b, k):
    """First k words of a, then the remaining words of b, as one line."""
    wa, wb = a.split(), b.split()
    return " ".join(wa[:k] + wb[k:]) + "\n"


def fixture_with_jaccard(lo, hi):
    a = " ".join(PARA_LINES)
    x = " ".join(OTHER_LINES)
    for k in range(len(a.split()) + 1):
        cand = mix(a, x, k)
        j = jaccard(a, cand)
        if lo <= j <= hi:
            assert len(norm(cand)) >= MINLEN
            return a + "\n", cand, j
    raise AssertionError(f"no fixture with jaccard in [{lo},{hi}]")


# --- repo helpers -----------------------------------------------------------
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
    if isinstance(text, bytes):
        p.write_bytes(text)
    else:
        p.write_text(text, encoding="utf-8")
    return p


def commit_all(repo, msg="base"):
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", msg)


def make_repo(tmp_path, committed):
    repo = init_repo(tmp_path / "repo")
    for rel, text in committed.items():
        write(repo, rel, text)
    commit_all(repo)
    return repo


def run(repo, *args):
    return subprocess.run([sys.executable, str(SCRIPT), *args], cwd=str(repo),
                          capture_output=True, text=True, timeout=60)


def out(r):
    return r.stdout + r.stderr


def doc(title, *blocks):
    return f"# {title}\n\n" + "\n".join(blocks)


BASE_A = doc("Skill A", PARA)
BASE_B = doc("Skill B", OTHER)


# 1 ------------------------------------------------------------------------
def test_clean_repo_no_change_passes(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B})
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_changed_but_distinct_content_passes(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B})
    write(repo, SKILL_B, doc("Skill B", OTHER, THIRD))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 2 ------------------------------------------------------------------------
def test_copy_into_another_skill_blocks_and_names_both(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B})
    # B: line1 heading, 2 blank, 3-5 OTHER, 6 blank, 7-9 the copy
    write(repo, SKILL_B, doc("Skill B", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert f"{SKILL_B}:7-9" in out(r), out(r)
    assert f"{SKILL_A}:3-5" in out(r), out(r)
    pair_lines = [l for l in out(r).splitlines() if "~" in l and SKILL_A in l and SKILL_B in l]
    assert pair_lines, out(r)
    assert re.search(r"jaccard [01]\.\d\d", pair_lines[0]), pair_lines[0]


def test_near_copy_one_word_changed_blocks(tmp_path):
    assert jaccard(PARA, PARA_ONE_WORD) >= 0.8
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B})
    write(repo, SKILL_B, doc("Skill B", OTHER, PARA_ONE_WORD))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)


def test_copy_into_agent_and_command_files_blocks(tmp_path):
    for rel in ("crates/b/agents/helper.md", "crates/b/commands/do.md"):
        sub = tmp_path / rel.replace("/", "_")
        sub.mkdir()
        repo = make_repo(sub, {SKILL_A: BASE_A, rel: doc("X", OTHER)})
        write(repo, rel, doc("X", OTHER, PARA))
        r = run(repo)
        assert r.returncode == 1, (rel, out(r), r.stderr)


def test_copy_into_claude_md_blocks(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, "CLAUDE.md": doc("Rules", OTHER)})
    write(repo, "CLAUDE.md", doc("Rules", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "CLAUDE.md" in out(r) and SKILL_A in out(r)


# 3 ------------------------------------------------------------------------
def test_preexisting_duplicate_with_unrelated_edit_passes(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: doc("Skill B", OTHER, PARA)})
    write(repo, SKILL_B, doc("Skill B", OTHER, PARA, THIRD))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 4 ------------------------------------------------------------------------
def test_slight_edit_of_already_duplicated_block_passes(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: doc("Skill B", OTHER, PARA)})
    write(repo, SKILL_B, doc("Skill B", OTHER, PARA_ONE_WORD))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 5 ------------------------------------------------------------------------
def test_docs_to_docs_duplicate_passes(tmp_path):
    repo = make_repo(tmp_path, {"docs/one.md": doc("One", OTHER), "docs/two.md": doc("Two", OTHER)})
    write(repo, "docs/two.md", doc("Two", OTHER, PARA))
    write(repo, "docs/one.md", doc("One", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_readme_to_docs_duplicate_passes(tmp_path):
    repo = make_repo(tmp_path, {"crates/a/README.md": doc("A", OTHER), "docs/one.md": doc("One", OTHER)})
    write(repo, "crates/a/README.md", doc("A", OTHER, PARA))
    write(repo, "docs/one.md", doc("One", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_readme_of_different_crates_duplicate_passes(tmp_path):
    repo = make_repo(tmp_path, {"crates/a/README.md": doc("A", OTHER), "crates/b/README.md": doc("B", OTHER)})
    write(repo, "crates/a/README.md", doc("A", OTHER, PARA))
    write(repo, "crates/b/README.md", doc("B", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 6 ------------------------------------------------------------------------
def test_same_crate_readme_translation_mirror_passes(tmp_path):
    repo = make_repo(tmp_path, {"crates/a/README.md": doc("A", OTHER),
                                "crates/a/README.ja.md": doc("A", OTHER)})
    write(repo, "crates/a/README.md", doc("A", OTHER, PARA))
    write(repo, "crates/a/README.ja.md", doc("A", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_readme_mirror_still_blocks_when_copied_into_instruction_file(tmp_path):
    # the translation-mirror exemption must not leak to a third (instruction) file
    repo = make_repo(tmp_path, {"crates/a/README.md": doc("A", PARA),
                                "crates/a/README.ja.md": doc("A", PARA),
                                SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "crates/a/README.md" in out(r)


# 7 ------------------------------------------------------------------------
def test_instruction_to_readme_duplicate_blocks(tmp_path):
    repo = make_repo(tmp_path, {"crates/a/README.md": doc("A", PARA), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert "crates/a/README.md" in out(r) and SKILL_B in out(r)


def test_instruction_to_docs_duplicate_blocks(tmp_path):
    repo = make_repo(tmp_path, {"docs/guide.md": doc("G", PARA), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)


def test_readme_edited_to_copy_instruction_blocks(tmp_path):
    # changed file is the README, the instruction file is the unchanged counterpart
    repo = make_repo(tmp_path, {"crates/a/README.md": doc("A", OTHER), SKILL_B: doc("B", PARA)})
    write(repo, "crates/a/README.md", doc("A", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)


# 8 ------------------------------------------------------------------------
def test_blocks_below_min_length_ignored(tmp_path):
    short = "Always run the tests before you commit anything.\n"
    assert len(norm(short)) < MINLEN
    repo = make_repo(tmp_path, {SKILL_A: doc("A", short), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, short))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_block_just_over_min_length_counts(tmp_path):
    words = " ".join(PARA_LINES).split()
    text = None
    for n in range(3, len(words) + 1):
        cand = " ".join(words[:n]) + "\n"
        if len(norm(cand)) >= MINLEN:
            text = cand
            break
    assert text is not None and len(norm(text)) < MINLEN + 15
    repo = make_repo(tmp_path, {SKILL_A: doc("A", text), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, text))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)


# 9 ------------------------------------------------------------------------
def test_similarity_just_below_threshold_passes(tmp_path):
    a, b, j = fixture_with_jaccard(0.50, 0.59)
    assert j < THRESH
    repo = make_repo(tmp_path, {SKILL_A: doc("A", a), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, b))
    r = run(repo)
    assert r.returncode == 0, (j, out(r), r.stderr)


def test_similarity_just_above_threshold_blocks(tmp_path):
    a, b, j = fixture_with_jaccard(0.61, 0.70)
    assert j >= THRESH
    repo = make_repo(tmp_path, {SKILL_A: doc("A", a), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, b))
    r = run(repo)
    assert r.returncode == 1, (j, out(r), r.stderr)


def test_clearly_different_blocks_pass(tmp_path):
    assert jaccard(PARA, OTHER) < 0.3
    repo = make_repo(tmp_path, {SKILL_A: doc("A", PARA), SKILL_B: doc("B", THIRD)})
    write(repo, SKILL_B, doc("B", THIRD, OTHER))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 10 -----------------------------------------------------------------------
FENCE = ("```bash\n"
         "cargo build --release -p specguard && cp target/release/specguard crates/specguard/bin/specguard-darwin-arm64\n"
         "scripts/rollout-plugins.sh specguard --verify-provenance --restart-sessions-after-install\n"
         "```\n")


def test_fenced_code_block_copied_blocks(tmp_path):
    assert len(norm(FENCE)) >= MINLEN
    repo = make_repo(tmp_path, {SKILL_A: doc("A", FENCE), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, FENCE))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    # B: 1 heading, 2 blank, 3-5 OTHER, 6 blank, 7-10 fence (incl. both fence lines)
    assert f"{SKILL_B}:7-10" in out(r), out(r)
    assert f"{SKILL_A}:3-6" in out(r), out(r)


def test_fenced_block_with_blank_and_heading_lines_stays_one_block(tmp_path):
    fence = ("```text\n"
             "# not a heading, inside a fence\n"
             "\n"
             "scripts/rollout-plugins.sh specguard --verify-provenance --restart-sessions-after-install now\n"
             "```\n")
    repo = make_repo(tmp_path, {SKILL_A: doc("A", fence), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, fence))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert f"{SKILL_B}:7-11" in out(r), out(r)


# 11 -----------------------------------------------------------------------
HALF1 = "Record every undetermined result in the ledger together with the reason it was undetermined.\n"
HALF2 = "Never translate an undetermined result into a pass when the ledger cannot be written either.\n"


def test_halves_are_individually_short_but_joint_long():
    assert len(norm(HALF1)) < 2 * MINLEN and len(norm(HALF2)) < 2 * MINLEN
    # choose shorter halves so each is < MINLEN while the join is >= MINLEN
    h1 = "Record every undetermined result in the ledger with its reason.\n"
    h2 = "Never turn an undetermined result into a pass silently, ever.\n"
    assert len(norm(h1)) < MINLEN and len(norm(h2)) < MINLEN
    assert len(norm(h1 + h2)) >= MINLEN


H1 = "Record every undetermined result in the ledger with its reason.\n"
H2 = "Never turn an undetermined result into a pass silently, ever.\n"


def test_heading_splits_paragraph_into_short_blocks(tmp_path):
    joined = H1 + H2  # one two-line paragraph in A, long enough to count
    split = H1 + "\n## Next\n" + H2  # heading between: two short blocks, ignored
    repo = make_repo(tmp_path, {SKILL_A: doc("A", joined), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, split))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_heading_directly_after_paragraph_without_blank_line_still_splits(tmp_path):
    joined = H1 + H2
    split = H1 + "## Next\n" + H2  # no blank lines around the heading
    repo = make_repo(tmp_path, {SKILL_A: doc("A", joined), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, split))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_blank_line_splits_paragraph_into_short_blocks(tmp_path):
    joined = H1 + H2
    split = H1 + "\n" + H2
    repo = make_repo(tmp_path, {SKILL_A: doc("A", joined), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, split))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_unsplit_control_for_split_tests_blocks(tmp_path):
    joined = H1 + H2
    repo = make_repo(tmp_path, {SKILL_A: doc("A", joined), SKILL_B: doc("B", OTHER)})
    write(repo, SKILL_B, doc("B", OTHER, joined))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)


# 12 -----------------------------------------------------------------------
def test_deleting_a_duplicated_file_passes(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: doc("B", OTHER, PARA)})
    (repo / SKILL_B).unlink()
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_deleting_the_duplicate_block_passes(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: doc("B", OTHER, PARA)})
    write(repo, SKILL_B, doc("B", OTHER))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 13 -----------------------------------------------------------------------
def test_all_mode_with_existing_duplicate_exits_1(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: doc("B", OTHER, PARA)})
    r = run(repo, "--all")
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert f"{SKILL_A}:3-5" in out(r) and f"{SKILL_B}:7-9" in out(r), out(r)


def test_all_mode_without_duplicate_exits_0(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B})
    r = run(repo, "--all")
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_all_mode_ignores_head_and_sees_uncommitted_duplicate(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B})
    write(repo, SKILL_B, doc("B", OTHER, PARA))
    r = run(repo, "--all")
    assert r.returncode == 1, (r.stdout, r.stderr)


def test_all_mode_works_without_head_commit(tmp_path):
    repo = init_repo(tmp_path / "repo")
    write(repo, SKILL_A, BASE_A)
    write(repo, SKILL_B, doc("B", OTHER, PARA))
    r = run(repo, "--all")
    assert r.returncode == 1, (r.stdout, r.stderr)


def test_all_mode_ignores_instruction_less_duplicate(tmp_path):
    repo = make_repo(tmp_path, {"docs/one.md": doc("One", PARA), "docs/two.md": doc("Two", PARA)})
    r = run(repo, "--all")
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_all_mode_empty_corpus_is_undetermined(tmp_path):
    repo = make_repo(tmp_path, {"src/main.rs": "fn main() {}\n", "notes.md": doc("N", PARA)})
    r = run(repo, "--all")
    assert r.returncode == 2, (r.stdout, r.stderr)
    assert UNDET in (out(r) + r.stderr)


# 14 -----------------------------------------------------------------------
def test_non_utf8_corpus_file_is_undetermined_default_mode(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B})
    write(repo, SKILL_B, b"# B\n\n" + b"\xff\xfe\xfa invalid utf8 \x80\x81\n")
    r = run(repo)
    assert r.returncode == 2, (r.stdout, r.stderr)
    assert UNDET in (out(r) + r.stderr)


def test_non_utf8_corpus_file_is_undetermined_all_mode(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B})
    write(repo, "docs/bad.md", b"\xff\xfe\xfa bad bytes \x80\n")
    r = run(repo, "--all")
    assert r.returncode == 2, (r.stdout, r.stderr)
    assert UNDET in (out(r) + r.stderr)


def test_non_utf8_file_at_head_is_undetermined(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: b"# B\n\n\xff\xfe bad head \x80\n"})
    write(repo, SKILL_B, doc("B", OTHER))  # working tree is fine, HEAD copy is not
    r = run(repo)
    assert r.returncode == 2, (r.stdout, r.stderr)


def test_undetermined_takes_precedence_over_findings(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B, "docs/bad.md": "ok\n"})
    write(repo, SKILL_B, doc("B", OTHER, PARA))  # a real finding
    write(repo, "docs/bad.md", b"\xff\xfe\xfa\n")  # and an unreadable corpus file
    r = run(repo)
    assert r.returncode == 2, (r.stdout, r.stderr)


# 15 -----------------------------------------------------------------------
def test_no_head_commit_default_mode_is_undetermined(tmp_path):
    repo = init_repo(tmp_path / "repo")
    write(repo, SKILL_A, BASE_A)
    write(repo, SKILL_B, doc("B", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 2, (r.stdout, r.stderr)
    assert UNDET in (out(r) + r.stderr)


def test_not_a_git_repo_is_undetermined(tmp_path):
    d = tmp_path / "plain"
    d.mkdir()
    write(d, SKILL_A, BASE_A)
    for args in ((), ("--all",)):
        r = run(d, *args)
        assert r.returncode == 2, (args, out(r), r.stderr)
        assert UNDET in (out(r) + r.stderr)


# 16 -----------------------------------------------------------------------
def test_newly_added_staged_duplicate_file_blocks(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A})
    write(repo, SKILL_B, doc("B", OTHER, PARA))
    git(repo, "add", SKILL_B)
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert SKILL_A in out(r) and SKILL_B in out(r)


def test_newly_added_non_duplicate_file_passes(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A})
    write(repo, SKILL_B, BASE_B)
    git(repo, "add", SKILL_B)
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


# 17 -----------------------------------------------------------------------
def test_second_pair_between_same_files_blocks_and_reports_counts(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: doc("A", PARA, OTHER), SKILL_B: doc("B", PARA, THIRD)})
    # HEAD has exactly one pair (PARA~PARA). Add OTHER to B => now 2 > head 1.
    write(repo, SKILL_B, doc("B", PARA, THIRD, OTHER))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    head_line = [l for l in out(r).splitlines() if SKILL_A in l and SKILL_B in l and "~" not in l]
    assert head_line, out(r)
    assert "2" in head_line[0] and "1" in head_line[0], head_line[0]
    assert sum(1 for l in out(r).splitlines() if "~" in l) == 2, out(r)


def test_pair_count_unchanged_when_moved_within_file_passes(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: doc("B", OTHER, PARA)})
    write(repo, SKILL_B, doc("B", PARA, OTHER))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_pair_count_decrease_in_one_pair_does_not_mask_new_pair_elsewhere(tmp_path):
    third = "crates/c/skills/s3/SKILL.md"
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: doc("B", OTHER, PARA), third: doc("C", THIRD)})
    write(repo, SKILL_B, doc("B", OTHER))  # removes B~A
    write(repo, third, doc("C", THIRD, PARA))  # adds C~A
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    assert third in out(r)


def test_both_files_changed_reports_unordered_pair_once(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B})
    write(repo, SKILL_A, doc("Skill A", PARA, OTHER))
    write(repo, SKILL_B, doc("Skill B", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)
    head_lines = [l for l in out(r).splitlines() if SKILL_A in l and SKILL_B in l and "~" not in l]
    assert len(head_lines) == 1, out(r)


def test_head_side_uses_head_content_of_other_file_not_working_tree(tmp_path):
    # HEAD: B holds PARA, A does not. Working tree: A gains PARA (finding),
    # even though B is unchanged. Comparing with the WORKING-TREE B on the head
    # side would wrongly give head==now.
    repo = make_repo(tmp_path, {SKILL_A: BASE_B, SKILL_B: doc("B", PARA)})
    write(repo, SKILL_A, doc("A", OTHER, PARA))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)


def test_non_corpus_change_exits_zero(tmp_path):
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: doc("B", OTHER, PARA), "src/lib.rs": "// a\n"})
    write(repo, "src/lib.rs", "// b\n")
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)


def test_case_and_markup_differences_still_match(tmp_path):
    marked = "**" + PARA.upper().replace("\n", "**\n> ", 1)
    assert jaccard(PARA, marked) >= THRESH
    repo = make_repo(tmp_path, {SKILL_A: BASE_A, SKILL_B: BASE_B})
    write(repo, SKILL_B, doc("B", OTHER, marked))
    r = run(repo)
    assert r.returncode == 1, (r.stdout, r.stderr)


def test_copy_within_the_same_file_passes(tmp_path):
    # pairs must be in DIFFERENT files: a paragraph repeated inside one skill is not a finding
    repo = make_repo(tmp_path, {SKILL_A: doc("Skill A", PARA, OTHER)})
    write(repo, SKILL_A, doc("Skill A", PARA, OTHER, PARA))
    r = run(repo)
    assert r.returncode == 0, (r.stdout, r.stderr)
    r = run(repo, "--all")
    assert r.returncode == 0, (r.stdout, r.stderr)
