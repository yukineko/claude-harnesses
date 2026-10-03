"""Independent verifier test for backlog 05726f9f (not written by the implementer).

(i)  Static: no scripts/**/*.py (production AND tests) calls `.exec_module(` /
     `.load_module(` -- AST based, so multi-line call layouts are caught.
(ii) Live: the converted loader in check-claudemd-claims.py, run from a temp
     copy against a fake sibling engine, sees a same-size same-mtime edit,
     while the old exec_module form (control) returns the stale value.
"""
import ast
import importlib.util
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


def _load_from_source(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    exec(compile(spec.loader.get_source(spec.name), spec.origin, "exec", dont_inherit=True), mod.__dict__)  # noqa: S102
    return mod


def _old_form(name, path):
    # Control: the pre-fix form. getattr keeps this file out of its own static scan.
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    getattr(spec.loader, "exec_" + "module")(mod)
    return mod


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


class LiveStalePyc(unittest.TestCase):
    def setUp(self):
        self._dwb = sys.dont_write_bytecode
        sys.dont_write_bytecode = False
        self.tmp = Path(tempfile.mkdtemp())
        shutil.copy(HERE / "check-claudemd-claims.py", self.tmp / "check-claudemd-claims.py")
        self.engine = self.tmp / "check-doc-claims.py"
        self.engine.write_text("X = 1\n")
        st = self.engine.stat()
        # Prime the cache through the pyc-writing loader (X=1).
        self.assertEqual(_old_form("_check_doc_claims_engine", self.engine).X, 1)
        self.assertTrue(list((self.tmp / "__pycache__").glob("check-doc-claims*.pyc")),
                        "pyc not written; probe would be vacuous")
        self.engine.write_text("X = 2\n")  # same size
        os.utime(self.engine, ns=(st.st_atime_ns, st.st_mtime_ns))
        self.assertEqual(self.engine.stat().st_size, st.st_size)

    def tearDown(self):
        sys.dont_write_bytecode = self._dwb
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_control_old_form_is_stale(self):
        self.assertEqual(_old_form("_check_doc_claims_engine", self.engine).X, 1)

    def test_converted_production_loader_sees_new_source(self):
        gate = _load_from_source("ccc_copy", self.tmp / "check-claudemd-claims.py")
        self.assertEqual(gate._DOC_CLAIMS_PATH, str(self.engine))
        self.assertEqual(gate._load_doc_claims_engine().X, 2)


if __name__ == "__main__":
    unittest.main()
