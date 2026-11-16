#!/usr/bin/env python3
"""Adversarial fail-open probes for the b0de2710 blastguard fix (f63162f8f).

Written by an independent verifier. Each case feeds a harmful command to the
blastguard binary and asserts the decision is NOT allow (ask/deny). Controls
assert the three original false-positive shapes are allow.

Set BLASTGUARD_BIN to test a prebuilt binary (e.g. the pre-fix revision);
otherwise `cargo build -p blastguard` of this checkout is used.

DOES NOT PROVE: that no other heredoc/python shape slips through; only the
listed shapes are probed. Hermetic HOME/approvals dirs, no agent_id.
"""
import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_blastguard_heredoc_literal_fp as base  # noqa: E402

if os.environ.get("BLASTGUARD_BIN"):
    base.build_blastguard = lambda: os.environ["BLASTGUARD_BIN"]


class Adv(base.HeredocLiteralFalsePositives):
    pass


# drop inherited tests: only run our own
for _n in [n for n in dir(base.HeredocLiteralFalsePositives) if n.startswith("test_")]:
    setattr(Adv, _n, None)


def mk(name, cmd, allowed):
    def t(self):
        d, why = self.verdict(cmd)
        if allowed:
            self.assertEqual(d, "allow", why)
        else:
            self.assertIn(d, ("ask", "deny"), "FAIL-OPEN: %r -> %s" % (cmd, d))
    t.__name__ = "test_" + name
    setattr(Adv, t.__name__, t)


RM = "rm -rf ~"
HOOK = ".githooks/pre-push"
PY = "python3 - <<'EOF'\n%s\nEOF"

# --- A: `2>&1` skip on heredoc readers that may execute/persist the body
mk("a_cat_redirect_then_sh", "cat <<'EOF' 2>&1 > run.sh\n%s\nEOF\nsh run.sh" % RM, False)
mk("a_tee_redirect_then_sh", "tee run.sh <<'EOF' 2>&1\n%s\nEOF\nsh run.sh" % RM, False)
mk("a_tee_redirect_devnull_then_bash", "tee run.sh <<'EOF' 2>&1 >/dev/null\n%s\nEOF\nbash run.sh" % RM, False)
mk("a_bash_stdin_redirect", "bash <<'EOF' 2>&1\n%s\nEOF" % RM, False)
mk("a_sh_s_stdin_redirect", "sh -s <<'EOF' 2>&1\n%s\nEOF" % RM, False)
mk("a_cat_pipe_sh", "cat <<'EOF' 2>&1 | sh\n%s\nEOF" % RM, False)
mk("a_cat_pipe_bash_unquoted", "cat <<EOF 2>&1 | bash\n%s\nEOF" % RM, False)
mk("a_tee_githook", "tee %s <<'EOF' 2>&1\n#!/bin/sh\nexit 0\nEOF" % HOOK, False)
mk("a_cat_to_githook", "cat <<'EOF' 2>&1 > %s\n#!/bin/sh\nexit 0\nEOF" % HOOK, False)
mk("a_unquoted_heredoc_cmdsub", "cat <<EOF 2>&1\n$(rm -rf ~)\nEOF", False)
mk("a_commit_F_then_chain_rm", "git commit -F - <<'EOF' 2>&1 ; rm -rf ~\nm\nEOF", False)
mk("a_commit_F_after_eof_rm", "git commit -F - <<'EOF' 2>&1\nm\nEOF\nrm -rf ~", False)

# --- B: python stdin segment view
mk("b_os_system", PY % "import os; os.system('rm -rf ~')", False)
mk("b_write_githook", PY % ("open('%s','w').write('x')" % HOOK), False)
mk("b_string_semicolon_then_write",
   PY % ("x = 'a b; c'\nopen('%s','w').write(x)" % HOOK), False)
mk("b_string_newline_escape_then_write",
   PY % ("x = 'a b\\n c'\nopen('%s','w').write(x)" % HOOK), False)
mk("b_triple_string_then_write",
   PY % ("x = '''a b\nc d; e'''\nopen('%s','w').write(x)" % HOOK), False)
mk("b_comment_with_quote_then_write",
   PY % ("# it's a trap 'x y\nopen('%s','w').write('z')" % HOOK), False)
mk("b_unterminated_quote_then_write",
   PY % ("x = \"a b\nopen('%s','w').write('z')" % HOOK), False)
mk("b_subprocess_shell",
   PY % "import subprocess\nsubprocess.run('rm -rf ' + chr(126), shell=True)", False)
mk("b_exec_string_write",
   PY % ("exec(\"open('%s','w').write('x y')\")" % HOOK), False)
mk("b_write_with_spaced_content",
   PY % ("open('%s', 'w').write('hello world')" % HOOK), False)
mk("b_shutil_copy_githook",
   PY % ("import shutil\nshutil.copy('/tmp/x', '%s')" % HOOK), False)
mk("b_os_rename_githook",
   PY % ("import os\nos.rename('/tmp/x y', '%s')" % HOOK), False)
mk("b_tab_dash_heredoc",
   "python3 - <<-'EOF'\n\topen('%s','w').write('x y')\n\tEOF" % HOOK, False)

# --- C: for ... in open(...)
mk("c_read_passwd_write_githook",
   PY % ("for l in open('/etc/passwd'): pass\nopen('%s','w').write('x')" % HOOK), False)
mk("c_for_open_w", PY % ("for l in open('%s','w'): pass" % HOOK), False)
mk("c_for_open_a", PY % ("for l in open('%s','a'): pass" % HOOK), False)
mk("c_for_open_rplus", PY % ("for l in open('%s','r+'): pass" % HOOK), False)
mk("c_for_tuple_open_w", PY % ("for a, b in open('%s','w'): pass" % HOOK), False)
mk("c_for_open_mode_kw", PY % ("for l in open('%s', mode='w'): pass" % HOOK), False)
mk("c_for_open_body_writes_same_file",
   PY % ("for l in open('%s'):\n    open('%s','w').write(l)" % (HOOK, HOOK)), False)
mk("c_for_open_oneline_body_write",
   PY % ("for l in open('%s'): open('%s','w').write(l)" % (HOOK, HOOK)), False)
mk("c_for_open_chained_write",
   PY % ("for l in open('%s').write('x'): pass" % HOOK), False)
mk("c_for_open_body_remove",
   PY % ("import os\nfor l in open('/etc/passwd'): os.remove('%s')" % HOOK), False)
mk("c_for_nested_open_w_arg",
   PY % ("for l in open(open('%s','w').name): pass" % HOOK), False)

# --- controls: the three original FP shapes are allow; live detector still fires
mk("ctl_a_commit_redirect",
   "git commit -q -F - <<'EOF' 2>&1\nsubject\n\n%s\nEOF" % base.CACHE_LINE, True)
mk("ctl_b_print_literal", PY % "print('''Unit tests for .githooks/pre-push''')", True)
mk("ctl_c_for_open_read_only", PY % "for l in open('/etc/hostname'):\n    print(l)", True)

if __name__ == "__main__":
    unittest.main()
