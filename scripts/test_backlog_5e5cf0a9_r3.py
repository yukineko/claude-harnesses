#!/usr/bin/env python3
"""Round-3 RED repros for backlog 5e5cf0a9 -- USER RULING 2026-10-03.

Ruling: the small-task fast path (SKILL.md Phase 4.5.5) and single-worktree
mode (Phase 5 B, `worktree-mode-check`) are RETIRED. Every task has its OWN
worktree; a `serial` task runs one at a time (own worktree -> implement ->
Phase 6 verify -> immediate mid-run `worktree merge --task` + `worktree remove`
-> only then the next serial worktree). Nothing runs in the main tree.

A mention that something was retired is allowed: a hit is waived when its own
sentence (bounded by 。 / blank line) says 廃止 / 撤去 / retired / removed.
Every matcher has a positive and a negative control.
"""
from __future__ import annotations

import os
import re
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SCRIPTS)
SKILL = os.path.join(REPO, "crates", "condukt", "skills", "condukt", "SKILL.md")
WORKER = os.path.join(REPO, "crates", "condukt", "agents", "condukt-worker.md")


def read(p: str) -> str:
    with open(p, encoding="utf-8") as f:
        return f.read()


def lineno(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


RETIRED = re.compile(r"廃止|撤去|retired|removed|削除済", re.I)


def sentence_around(text: str, start: int, end: int) -> str:
    """Sentence = span between the previous/next 。 or blank line."""
    left = max(text.rfind("。", 0, start), text.rfind("\n\n", 0, start))
    cands = [i for i in (text.find("。", end), text.find("\n\n", end)) if i != -1]
    right = min(cands) if cands else len(text)
    return text[left + 1 : right]


def hits(text: str, patterns: list[re.Pattern]) -> list[tuple[int, str]]:
    seen: dict[int, str] = {}
    for rx in patterns:
        for m in rx.finditer(text):
            if RETIRED.search(sentence_around(text, m.start(), m.end())):
                continue
            seen.setdefault(lineno(text, m.start()), re.sub(r"\s+", " ", m.group(0)))
    return sorted(seen.items())


W = r"\s*"
SKILL_PATTERNS = [
    # (1) fast path as an execution route
    re.compile(r"^#+\s*Phase\s*4\.5\.5", re.M),
    re.compile(r"fast[- ]?path", re.I),
    re.compile(r"ファスト\s*パス"),
    re.compile(r"Small-task"),
    # (2) single-worktree mode
    re.compile(r"^#+\s*B\.\s*単一\s*worktree", re.M),
    re.compile(r"単一\s*worktree"),
    re.compile(r"single[- _]worktree", re.I),
    re.compile(r"--topic\s+run\b"),
    re.compile(r"condukt/(?:\$\{?RID\}?/)?run\b"),
    re.compile(r"worktree-mode-check"),
    re.compile(r"no-stage-no-commit"),
    re.compile(r"run\s*worktree|run\s*専用の\s*worktree"),
]
WORKER_PATTERNS = [
    re.compile(r"no-stage-no-commit"),
    re.compile(r"共有\s*(?:run\s*)?worktree"),
    re.compile(r"単一\s*worktree"),
    re.compile(r"single[- _]worktree", re.I),
    re.compile(r"fast[- ]?path", re.I),
    re.compile(r"run\s*worktree"),
]


class MatcherControls(unittest.TestCase):
    def test_positive(self):
        for s in (
            "### Phase 4.5.5 — Small-task fast path (省略可)",
            "#### B. 単一 worktree モード（`single_worktree` 有効時）",
            "WP=$(condukt worktree create --run \"$RID\" --topic run --branch condukt/run)",
            "condukt worktree merge --branch condukt/$RID/run --run \"$RID\"",
            "condukt state worktree-mode-check   # exit 0",
            "`commit_mode: no-stage-no-commit` を渡す",
            "fast path の worker は",
        ):
            self.assertNotEqual(hits(s, SKILL_PATTERNS), [], s)
        for s in (
            "- **`no-stage-no-commit`（共有 run worktree）** → x",
            "単一 worktree モードでは",
        ):
            self.assertNotEqual(hits(s, WORKER_PATTERNS), [], s)

    def test_negative(self):
        for s in (
            "他の worktree や main repo dir を触らない。",
            "condukt/$RID/<t.id> を merge する",
            "worktree merge --branch condukt/$RID/running",  # not the `run` branch
            "各タスクは専用 worktree で実装する。",
            "single_worktree モードは廃止した。",  # retired mention allowed
            "fast path (Phase 4.5.5) は retired であり存在しない。",
        ):
            self.assertEqual(hits(s, SKILL_PATTERNS), [], s)
        self.assertEqual(hits("作業は割り当て worktree 内に限定する。", WORKER_PATTERNS), [])
        self.assertEqual(hits("`no-stage-no-commit` は撤去された。", WORKER_PATTERNS), [])

    def test_retired_allowance_is_per_sentence(self):
        # a retirement note in ANOTHER sentence must not launder a live mention
        s = "単一 worktree モードは廃止した。\n\nB. 単一 worktree モードで実行する。"
        self.assertEqual([l for l, _ in hits(s, SKILL_PATTERNS)], [3])

    def test_worker_prohibition_matcher(self):
        self.assertTrue(has_prohibition("- 他の worktree や main repo dir を触らない。"))
        self.assertTrue(has_prohibition("他の worktree や\nmain repo dir を触らない"))
        self.assertFalse(has_prohibition("main repo dir を触る"))


def has_prohibition(text: str) -> bool:
    return bool(re.search(r"他の\s*worktree\s*や\s*main\s*repo\s*dir\s*を\s*触らない", text))


class SkillRetiredModes(unittest.TestCase):
    def test_no_fast_path_or_single_worktree_execution_route(self):
        h = [f"SKILL.md:{l}: {t}" for l, t in hits(read(SKILL), SKILL_PATTERNS)]
        self.assertEqual(h, [], f"{len(h)} live reference(s) to retired modes:\n" + "\n".join(h))


class WorkerRetiredModes(unittest.TestCase):
    def test_no_shared_worktree_or_no_stage_mode(self):
        h = [f"condukt-worker.md:{l}: {t}" for l, t in hits(read(WORKER), WORKER_PATTERNS)]
        self.assertEqual(h, [], f"{len(h)} live reference(s) to retired modes:\n" + "\n".join(h))

    def test_worker_keeps_other_worktree_prohibition(self):
        self.assertTrue(has_prohibition(read(WORKER)), "line-28 prohibition missing")


# ---------------------------------------------------------- serial procedure
def phase5(text: str) -> str:
    m = re.search(r"^### Phase 5 —.*?(?=^#{3,4} Phase 5\.5\b)", text, re.S | re.M)
    return m.group(0) if m else ""


MERGE = re.compile(
    r'worktree\s+merge\s+--branch\s+condukt/\$\{?RID\}?/<t\.id>\s+--run\s+"?\$\{?RID\}?"?\s+--task\s+<t\.id>'
)
NEXT_CREATE = re.compile(r"次の\s*serial[^。]{0,60}?worktree[^。]{0,20}?(?:作る|作成|create)")
STOP = re.compile(r"(?:HOLD|衝突)[^。]{0,60}?(?:止まる|停止|進まず|進まない)|(?:止まる|停止)[^。]{0,40}?(?:HOLD|衝突)")


def serial_contract(sec: str) -> list[str]:
    """Problems (empty = contract met)."""
    out = []
    m = MERGE.search(sec)
    if not m:
        out.append("no `worktree merge --branch condukt/$RID/<t.id> --run \"$RID\" --task <t.id>`")
        return out
    n = NEXT_CREATE.search(sec, m.end())
    if not n:
        out.append("no 'next serial worktree is created AFTER the merge' instruction following the merge")
    elif n.start() - m.end() > 600:
        out.append("merge and the next-serial create are not in the same instruction")
    if not STOP.search(sec[m.end():]):
        out.append("no stop-on-HOLD/conflict instruction after the merge")
    return out


GOOD = (
    '各 serial の verifier pass 直後に `condukt worktree merge --branch condukt/$RID/<t.id> '
    '--run "$RID" --task <t.id>` を実行し、それから次の serial タスクの worktree を作る。'
    "merge が HOLD / 衝突したら次へ進まず止まる。"
)


class SerialControls(unittest.TestCase):
    def test_positive(self):
        self.assertEqual(serial_contract(GOOD), [])

    def test_negative(self):
        self.assertTrue(serial_contract("serial は1件ずつ実装する。"))
        # merge present but no stop instruction
        self.assertTrue(serial_contract(GOOD.split("merge が")[0]))
        # merge without --task
        self.assertTrue(serial_contract(GOOD.replace(" --task <t.id>", "")))
        # next create phrased BEFORE the merge only
        self.assertTrue(serial_contract("次の serial タスクの worktree を作る。\n" + GOOD.split("それから")[0]))

    def test_phase5_extractor(self):
        self.assertEqual(phase5("### Phase 5 — x\nA\n### Phase 5.5 — y\n"), "### Phase 5 — x\nA\n")
        self.assertEqual(phase5("nothing"), "")


class SerialProcedure(unittest.TestCase):
    def test_phase5_serial_merges_mid_run_before_next_worktree_and_stops_on_hold(self):
        sec = phase5(read(SKILL))
        self.assertNotEqual(sec, "", "Phase 5 section not found")
        self.assertEqual(serial_contract(sec), [])


# ===================================================== round 3b: every document
# The first scan covered only SKILL.md and condukt-worker.md; the verify round
# found live main-tree routing in crates/condukt/docs/internals.ja.md. Scope is
# now EVERY condukt document.
import glob
import sys

sys.path.insert(0, SCRIPTS)
import test_backlog_5e5cf0a9 as r1  # noqa: E402  (round-1 ROUTES matchers)


def all_docs() -> list[str]:
    files = sorted(glob.glob(os.path.join(REPO, "crates", "condukt", "**", "*.md"), recursive=True))
    files = [f for f in files if os.sep + "target" + os.sep not in f]
    spec = os.path.join(REPO, "docs", "specs", "condukt.md")
    return files + [spec]


def rel(p: str) -> str:
    return os.path.relpath(p, REPO)


def line_of(text: str, pos: int) -> str:
    a = text.rfind("\n", 0, pos) + 1
    b = text.find("\n", pos)
    return text[a : b if b != -1 else len(text)]


NEGATED = re.compile(r"^[^\n。]{0,12}?(?:しない|せず|ない|禁止|不可|ではな)")


def route_hits(text: str) -> list[tuple[int, str]]:
    """Main-tree routing (round-1 ROUTES) with two allowances: the matched line
    says retired/removed, or the match is immediately negated (しない/禁止...).
    Line-scoped, so diagram / code-span text like `(main で直接実装)` is seen."""
    seen: dict[int, str] = {}
    for rx in r1.ROUTES:
        for m in rx.finditer(text):
            if RETIRED.search(line_of(text, m.start())) or NEGATED.match(text[m.end() :]):
                continue
            seen.setdefault(lineno(text, m.start()), re.sub(r"\s+", " ", m.group(0)))
    return sorted(seen.items())


def line_scoped_hits(text: str, patterns: list[re.Pattern]) -> list[tuple[int, str]]:
    """Like hits() but the retirement allowance is the whole LINE (table rows and
    README prose describe the retirement in a neighbouring sentence of one line)."""
    seen: dict[int, str] = {}
    for rx in patterns:
        for m in rx.finditer(text):
            if RETIRED.search(line_of(text, m.start())):
                continue
            seen.setdefault(lineno(text, m.start()), re.sub(r"\s+", " ", m.group(0)))
    return sorted(seen.items())


DANGLING = [
    re.compile(r"Phase\s*5\s*[AB]\b"),
    re.compile(r"Phase\s*4\.5\.5"),
]
_PH7 = re.compile(r"Phase\s*7")
_SERIAL = re.compile(r"serial|直列")
_MERGE = re.compile(r"merge|統合")
# Phase 7 mentioned only to say it is NOT where serial merges happen.
_PH7_OK = re.compile(
    r"Phase\s*7[^。\n]{0,12}?(?:を待たず|に任せ|ではなく|まで待たず)|Phase\s*7「[^」]*対処」|mid-run|再\s*merge\s*しない|merge\s*済み"
)


def serial_ph7_hits(text: str) -> list[tuple[int, str]]:
    """Sentences that attribute the serial merge to Phase 7."""
    out: dict[int, str] = {}
    for m in _PH7.finditer(text):
        sent = sentence_around(text, m.start(), m.end())
        if _SERIAL.search(sent) and _MERGE.search(sent) and not _PH7_OK.search(sent):
            out.setdefault(lineno(text, m.start()), re.sub(r"\s+", " ", sent.strip())[:100])
    return sorted(out.items())


class AllDocsControls(unittest.TestCase):
    def test_route_positive(self):
        for s in (
            "serial タスクは worktree に出さず main で順に実装します。",
            "+--(needs-serial)--> (main で直接実装)",
        ):
            self.assertNotEqual(route_hits(s), [], s)

    def test_route_negative(self):
        for s in (
            "main で実装しない。",
            "main の作業ツリーで許されるのは統合だけ",
            "main で順に実装する方式は廃止した。",
            "他の worktree や main repo dir を触らない。",
        ):
            self.assertEqual(route_hits(s), [], s)

    def test_route_line_numbers(self):
        self.assertEqual([l for l, _ in route_hits("a\nb\n(main で直接実装)")], [3])

    def test_line_scope_allowance(self):
        live = "single_worktree モードで実行する。"
        self.assertNotEqual(line_scoped_hits(live, SKILL_PATTERNS), [])
        row = "| `state worktree-mode-check` | per-task を報告する。single-worktree モードは廃止された。 |"
        self.assertEqual(line_scoped_hits(row, SKILL_PATTERNS), [])
        # retirement on a NEIGHBOUR line must not launder a live line
        self.assertNotEqual(line_scoped_hits("廃止した。\nsingle_worktree で実行する", SKILL_PATTERNS), [])

    def test_dangling_positive_negative(self):
        self.assertNotEqual(line_scoped_hits("Phase 5 A の mid-run merge で", DANGLING), [])
        self.assertNotEqual(line_scoped_hits("see Phase 4.5.5", DANGLING), [])
        self.assertNotEqual(line_scoped_hits("Phase 5 B を使う", DANGLING), [])
        self.assertEqual(line_scoped_hits("Phase 5 の serial 手順", DANGLING), [])
        self.assertEqual(line_scoped_hits("Phase 5 A は廃止された", DANGLING), [])
        self.assertEqual(line_scoped_hits("Phase 5.5 の合意", DANGLING), [])

    def test_serial_phase7_positive(self):
        self.assertNotEqual(
            serial_ph7_hits("serial タスクは Phase 7 の `condukt worktree merge` で統合してから次へ進む。"), []
        )
        self.assertNotEqual(serial_ph7_hits("直列タスクは Phase 7 で merge する。"), [])

    def test_serial_phase7_negative(self):
        for s in (
            "各 serial の verifier pass 直後に、Phase 7 を待たず mid-run で merge する。",
            "serial タスクは mid-run merge 済みなので Phase 7 では再 merge しない。",
            "Phase 7 の gate は run 全体を見る。",  # no serial+merge
            "parallel タスクは Phase 7 で merge する。",  # not serial
            "serial の merge が衝突したら Phase 7「merge pre-flight 衝突への対処」に従う。",  # pointer to a subsection
        ):
            self.assertEqual(serial_ph7_hits(s), [], s)

    def test_all_docs_enumerated(self):
        names = {rel(f) for f in all_docs()}
        for must in (
            "crates/condukt/docs/internals.ja.md",
            "crates/condukt/README.md",
            "crates/condukt/README.ja.md",
            "crates/condukt/skills/condukt/SKILL.md",
            "crates/condukt/agents/condukt-worker.md",
            "docs/specs/condukt.md",
        ):
            self.assertIn(must, names)
        for f in all_docs():
            self.assertTrue(os.path.isfile(f), f)


class AllDocsMainTreeRouting(unittest.TestCase):
    def test_no_document_routes_implementation_into_main(self):
        h = [f"{rel(f)}:{l}: {t}" for f in all_docs() for l, t in route_hits(read(f))]
        self.assertEqual(h, [], f"{len(h)} main-tree routing site(s):\n" + "\n".join(h))


class AllDocsRetiredModes(unittest.TestCase):
    def test_no_document_describes_a_retired_mode_as_live(self):
        h = [
            f"{rel(f)}:{l}: {t}"
            for f in all_docs()
            for l, t in line_scoped_hits(read(f), SKILL_PATTERNS)
        ]
        self.assertEqual(h, [], f"{len(h)} live retired-mode reference(s):\n" + "\n".join(h))


class SkillSerialMergeTiming(unittest.TestCase):
    def test_skill_does_not_attribute_serial_merge_to_phase7(self):
        h = [f"SKILL.md:{l}: {t}" for l, t in serial_ph7_hits(read(SKILL))]
        self.assertEqual(h, [], "serial merge is mid-run, not Phase 7:\n" + "\n".join(h))


class SkillDanglingPhaseRefs(unittest.TestCase):
    def test_no_reference_to_removed_phase5_a_b_or_4_5_5(self):
        h = [f"{rel(f)}:{l}: {t}" for f in all_docs() for l, t in line_scoped_hits(read(f), DANGLING)]
        self.assertEqual(h, [], f"{len(h)} dangling reference(s):\n" + "\n".join(h))


if __name__ == "__main__":
    unittest.main()
