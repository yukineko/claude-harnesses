#!/usr/bin/env python3
"""The real .githooks/pre-commit must not block a commit because of documents.

User ruling 2026-10-17: 「止めたGateを不具合と今後みなす。文書ではない、テストである」.

Each case builds a throwaway git repo holding a copy of the REAL pre-commit
hook. Every scanner the hook runs is stubbed to exit 0, EXCEPT the document
checkers (check-doc-claims.py, check-claudemd-claims.py, check-prose-tests.py),
which are copied for real when they still exist in this repo — so if the hook
still runs them, they judge the fixture's documents exactly as in production,
and if the hook still names them after they were deleted, the hook's own
"scanner missing" rule blocks. Either way the commit is blocked and the case
is RED.
"""

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
HOOK = REPO / ".githooks" / "pre-commit"
DOC_CHECKERS = ("check-doc-claims.py", "check-claudemd-claims.py", "check-prose-tests.py")

STUB = "#!/usr/bin/env python3\nimport sys\nsys.exit(0)\n"


def hook_scanners():
    return re.findall(r"^run\s+(\S+)\s", HOOK.read_text(), flags=re.M)


class Fixture:
    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "repo"
        self.root.mkdir()
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "t@example.invalid")
        self.git("config", "user.name", "t")
        (self.root / ".githooks").mkdir()
        shutil.copy2(HOOK, self.root / ".githooks" / "pre-commit")
        os.chmod(self.root / ".githooks" / "pre-commit", 0o755)
        scripts = self.root / "scripts"
        scripts.mkdir()
        for name in hook_scanners():
            real = HERE / name
            if name in DOC_CHECKERS:
                if real.exists():
                    shutil.copy2(real, scripts / name)
                # absent: leave it missing; the hook blocks on a missing scanner
            else:
                (scripts / name).write_text(STUB)
        # check-claudemd-claims.py loads check-doc-claims.py as its engine.
        for name in DOC_CHECKERS:
            if (HERE / name).exists() and not (scripts / name).exists():
                shutil.copy2(HERE / name, scripts / name)
        bindir = Path(self.tmp.name) / "bin"
        bindir.mkdir()
        condukt = bindir / "condukt"
        condukt.write_text("#!/bin/sh\nexit 0\n")
        os.chmod(condukt, 0o755)
        self.env = dict(os.environ, CONDUKT_BIN=str(condukt), HOME=self.tmp.name)
        # A clean, non-empty document set, so the doc checkers (while they
        # exist) have something to judge and the control case passes.
        self.write("CLAUDE.md", "# C\n")
        self.write("docs/index.md", "# I\n")

    def write(self, rel, text):
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def git(self, *args):
        return subprocess.run(
            ["git", *args], cwd=self.root, capture_output=True, text=True,
            env=getattr(self, "env", None),
        )

    def baseline(self):
        self.git("add", "-A")
        p = self.git("commit", "-q", "-m", "baseline")  # hooksPath not yet set
        assert p.returncode == 0, p.stderr
        self.git("config", "core.hooksPath", ".githooks")

    def commit(self, msg):
        self.git("add", "-A")
        return self.git("commit", "-q", "-m", msg)

    def close(self):
        self.tmp.cleanup()


class NoDocGates(unittest.TestCase):
    def setUp(self):
        self.f = Fixture()

    def tearDown(self):
        self.f.close()

    def test_control_plain_code_commit_passes(self):
        """Anti-vacuity: the fixture's own hook machinery lets a plain commit
        through, so a block below is caused by the document content."""
        self.f.write("src/x.py", "def f():\n    return 42\n")
        self.f.baseline()
        self.f.write("src/y.py", "Y = 1\n")
        p = self.f.commit("plain")
        self.assertEqual(p.returncode, 0, p.stderr)

    def test_code_change_drifting_a_doc_line_citation_is_not_blocked(self):
        self.f.write("src/x.py", "def f():\n    return 42\n")
        self.f.write("docs/a.md", "`src/x.py:2` 「return 42」 is the answer.\n")
        self.f.write("CLAUDE.md", "`src/x.py:2` 「return 42」 is the answer.\n")
        self.f.baseline()
        self.f.write("src/x.py", "def f():\n    return 43\n")
        p = self.f.commit("drift the citation")
        self.assertEqual(p.returncode, 0, p.stderr)

    def test_uncited_doc_sentence_is_not_blocked(self):
        self.f.write("docs/b.md", "# B\n")
        self.f.baseline()
        self.f.write("docs/b.md", "# B\n\nThe widget always returns forty-two.\n")
        p = self.f.commit("uncited prose")
        self.assertEqual(p.returncode, 0, p.stderr)


if __name__ == "__main__":
    unittest.main()
