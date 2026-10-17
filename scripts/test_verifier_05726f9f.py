"""Independent verifier test for backlog 05726f9f (not written by the implementer).

Static: no scripts/**/*.py (production AND tests) calls `.exec_module(` /
`.load_module(` -- AST based, so multi-line call layouts are caught.
"""
import ast
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


class StaticAllScripts(unittest.TestCase):
    def test_no_pyc_trusting_loader_calls_anywhere_under_scripts(self):
        offenders = []
        files = sorted(HERE.rglob("*.py"))
        self.assertGreater(len(files), 50, "scan set suspiciously small")
        for p in files:
            if "__pycache__" in p.parts:
                continue
            tree = ast.parse(p.read_text(encoding="utf-8"), str(p))
            for n in ast.walk(tree):
                if (isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute)
                        and n.func.attr in ("exec_module", "load_module")):
                    offenders.append(f"{p.relative_to(HERE)}:{n.lineno}")
        self.assertEqual(offenders, [])

    def test_scanner_detects_multiline_form(self):
        src = "spec.loader." + "exec_module(\n    mod\n)\n"
        calls = [n for n in ast.walk(ast.parse(src)) if isinstance(n, ast.Call)
                 and isinstance(n.func, ast.Attribute) and n.func.attr == "exec_module"]
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
