#!/usr/bin/env python3
"""RED repro for backlog 5e5cf0a9.

CLAUDE.md section 8 forbids implementing anything in the MAIN working tree
(only merge / conflict resolution is allowed there) and names the
"serial / single_worktree implemented directly in main" route explicitly.
condukt's SKILL.md and condukt-worker.md still route several execution shapes
(serial, fast path, needs-serial, single-worktree mode) into the main tree.

Contract pinned here: while the CLAUDE.md prohibition stands, neither
SKILL.md nor condukt-worker.md routes any implementation work into the main
tree.

  * control: CLAUDE.md still carries the prohibition;
  * matcher controls: the matcher flags routing sentences and does NOT flag
    the negative instruction "main repo dir を触らない" or CLAUDE.md's own
    "main の作業ツリーで許されるのは統合だけ" (guards vacuous / overbroad regex);
  * defect (RED): no routing hit in either condukt document.

Matching runs over the whole file text (not per line) so a phrase wrapped
across a line break is still found; hits are reported as file:line of the
match start.
"""

from __future__ import annotations

import os
import re
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SCRIPTS)
SKILL = os.path.join(REPO, "crates", "condukt", "skills", "condukt", "SKILL.md")
WORKER = os.path.join(REPO, "crates", "condukt", "agents", "condukt-worker.md")
CLAUDE_MD = os.path.join(REPO, "CLAUDE.md")

PROHIBITION = "`serial` タスクを「main で直接実装する」経路として使わない"

# \s* tolerates a line break (or markdown ** emphasis via \**) between tokens.
_W = r"\s*"
_B = r"(?:\*\*)?"
ROUTES = [
    # "main で実装" / "main で順に実装" / "main で1件ずつ実装" / "main で直接実装" / "main で実装し直す"
    re.compile(rf"main{_W}で{_W}{_B}(?:順に|1件ずつ|直接){_B}{_W}{_B}実装|main{_W}で{_W}{_B}実装"),
    # "main の作業ツリーで実装" (also "...1つで実行": whole run executed in the main tree)
    re.compile(rf"main{_W}の{_W}作業ツリー{_W}(?:1{_W}つ{_W})?で{_W}{_B}(?:直接{_W})?(?:実装|実行)"),
    # "main repo dir" as a worker's working directory (positive routing only;
    # "main repo dir を触らない" is a prohibition and does not match)
    re.compile(rf"作業ディレクトリ{_W}[=＝]{_W}{_B}main{_W}repo{_W}dir"),
    re.compile(rf"モード{_W}[=＝]{_W}{_B}main{_W}repo{_W}dir"),
    re.compile(rf"作業ディレクトリ{_W}は{_W}{_B}main{_W}repo{_W}dir"),
    # skipping the worktree
    re.compile(rf"worktree{_W}を{_W}作らず"),
    re.compile(rf"worktree{_W}に{_W}出さず"),
    re.compile(rf"worktree{_W}作成{_W}を{_W}省略"),
]


def read(p: str) -> str:
    with open(p, encoding="utf-8") as f:
        return f.read()


def find_routes(text: str) -> list[tuple[int, str]]:
    """Return (1-based line of match start, matched text collapsed) for every hit."""
    seen: dict[int, str] = {}
    for rx in ROUTES:
        for m in rx.finditer(text):
            line = text.count("\n", 0, m.start()) + 1
            seen.setdefault((line), re.sub(r"\s+", " ", m.group(0)))
    return sorted(seen.items())


def prohibition_stands() -> bool:
    return re.sub(r"\s+", "", PROHIBITION) in re.sub(r"\s+", "", read(CLAUDE_MD))


def routing_hits() -> list[str]:
    out = []
    for label, path in (("SKILL.md", SKILL), ("condukt-worker.md", WORKER)):
        for line, frag in find_routes(read(path)):
            out.append(f"{label}:{line}: {frag}")
    return out


class MatcherControls(unittest.TestCase):
    def test_negative_sentences_not_flagged(self) -> None:
        for s in (
            "他の worktree や main repo dir を触らない。",
            "main の作業ツリーで許されるのは統合（merge・conflict 解決）だけ",
            "承認はユーザーから main で得る。",
            "main の作業ツリーで編集・stage・commit しない。",
        ):
            self.assertEqual(find_routes(s), [], s)

    def test_routing_sentences_flagged(self) -> None:
        for s in (
            "serial タスクは worktree に出さず main で順に実装する。",
            "main の作業ツリーで実装する",
            "作業ディレクトリ = **main repo dir**（専用 worktree なし）",
            "単一 worktree モード=**main repo dir**",
            "serial 降格して main で実装し直す",
            "worktree 作成を省略して main で直接実装する",
            "worktree を作らず",
        ):
            self.assertNotEqual(find_routes(s), [], s)

    def test_line_break_tolerated_and_line_reported(self) -> None:
        hits = find_routes("a\nb\nmain の\n作業ツリーで\n実装する")
        self.assertEqual([h[0] for h in hits], [3])
        hits = find_routes("x\nworktree を作らず\ny")
        self.assertEqual([h[0] for h in hits], [2])


class MainTreeRouteMatchesSection8(unittest.TestCase):
    def test_claude_md_prohibits_main_tree_serial_route(self) -> None:
        self.assertTrue(prohibition_stands(), "CLAUDE.md section 8 prohibition text not found")

    def test_condukt_docs_do_not_route_work_to_main(self) -> None:
        self.assertTrue(prohibition_stands(), "control failed: nothing to contradict")
        hits = routing_hits()
        self.assertEqual(
            hits,
            [],
            f"{len(hits)} site(s) route implementation into the main tree, which "
            "CLAUDE.md section 8 forbids:\n" + "\n".join(hits),
        )


if __name__ == "__main__":
    unittest.main()
