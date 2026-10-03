#!/usr/bin/env python3
"""backlog abba6f0d: hook-reachable code spawns harness binaries by BARE name
(`Command::new("overwatch")` etc.). Hook child processes do not inherit the
plugin bin dirs on PATH, so a bare-name spawn only sees the login PATH — the
same defect overwatch 0.2.28 (e2a22f3b) fixed for `overwatch status` by moving
to `harness_core::plugin_bin::resolve`.

This test enumerates exactly the sites the item lists (file + spawned binary)
and fails while any of them still passes a bare harness binary name to
`Command::new`. (autoflow/src/main.rs's condukt spawn, also in the item, is
already gone; autoflow/src/backlog.rs's lexical `candidates.sort()` half is
measured by plugin_bin's own RED and not repeated here.)

Run:  python3 -m unittest scripts.test_backlog_abba6f0d
"""
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

SITES = [
    ("crates/autoflow/src/backlog.rs", "backlog"),
    ("crates/backlog/src/store.rs", "condukt"),
    ("crates/condukt/src/claim.rs", "backlog"),
    ("crates/ctxrot/src/hooks/guard.rs", "overwatch"),
    ("crates/daily/src/main.rs", "backlog"),
    ("crates/specguard/src/forge/queue.rs", "backlog"),
    ("crates/stuckguard/src/anchor.rs", "overwatch"),
    ("crates/stuckguard/src/anchor.rs", "condukt"),
]


def bare_spawns(rel, name):
    pat = re.compile(r'Command::new\(\s*"%s"\s*\)' % re.escape(name))
    out = []
    for n, line in enumerate((REPO / rel).read_text().splitlines(), 1):
        code = line.split("//", 1)[0]
        if pat.search(code):
            out.append(f"{rel}:{n}: {line.strip()}")
    return out


class NoBareNameHarnessSpawns(unittest.TestCase):
    def test_precondition_listed_files_exist(self):
        for rel, _ in SITES:
            self.assertTrue((REPO / rel).is_file(), rel)

    def test_precondition_plugin_bin_resolver_exists(self):
        self.assertTrue((REPO / "crates/harness-core/src/plugin_bin.rs").is_file())

    def test_listed_sites_do_not_spawn_harness_binaries_by_bare_name(self):
        """backlog abba6f0d: open defect (RED observed)."""
        hits = [h for rel, name in SITES for h in bare_spawns(rel, name)]
        self.assertEqual(
            hits, [],
            "bare-name harness spawns (not via harness_core::plugin_bin::resolve):\n"
            + "\n".join(hits))


# --- remaining sites (the 43780aa1 migration did not reach these) -----------

PLUGIN_BIN = "crates/harness-core/src/plugin_bin.rs"

_CFG_TEST = re.compile(r'#\[cfg\((?:all\()?\s*test\b')


def plugin_names():
    import json
    return sorted({json.loads(p.read_text())["name"]
                   for p in REPO.glob("crates/*/.claude-plugin/plugin.json")})


def _lex(text):
    """Return (code, scan), both the same length as `text` with newlines kept:
    `code` has comments blanked; `scan` additionally blanks the contents of
    string/char literals (for brace counting). A naive `split("//")` would cut
    a string literal containing `//` and unbalance every later brace."""
    code, scan = list(text), list(text)
    n, i = len(text), 0

    def blank(a, b, both):
        for k in range(a, b):
            if text[k] != "\n":
                scan[k] = " "
                if both:
                    code[k] = " "

    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            blank(i, j, True)
            i = j
        elif text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j, True)
            i = j
        elif c == "r" and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")) \
                and re.match(r'r(#*)"', text[i:]):
            hashes = re.match(r'r(#*)"', text[i:]).group(1)
            start = i + 2 + len(hashes)
            j = text.find('"' + hashes, start)
            j = n if j < 0 else j + 1 + len(hashes)
            blank(start, j - 1 - len(hashes), False)
            i = j
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            blank(i + 1, j, False)
            i = j + 1
        elif c == "'":
            if i + 1 < n and text[i + 1] == "\\":
                j = text.find("'", i + 2)
                j = n if j < 0 else j
                blank(i + 1, j, False)
                i = j + 1
            elif i + 2 < n and text[i + 2] == "'":
                blank(i + 1, i + 2, False)
                i += 3
            else:
                i += 1  # lifetime
        else:
            i += 1
    return "".join(code), "".join(scan)


def non_test_lines(rel):
    """(lineno, code) for every line of `rel` that is NOT inside a
    `#[cfg(test)]` item, with comments stripped. Test modules may use
    PATH shims / fixture cache dirs legitimately, so they are excluded."""
    code_txt, scan_txt = _lex((REPO / rel).read_text())
    code = code_txt.split("\n")
    scan = scan_txt.split("\n")
    skip = set()
    i = 0
    while i < len(scan):
        if _CFG_TEST.search(scan[i]):
            # Skip the attributed item: up to its `;` or its balanced `{...}`.
            depth, j, opened = 0, i, False
            while j < len(scan):
                seg = scan[j] if j != i else scan[j][_CFG_TEST.search(scan[j]).end():]
                for ch in seg:
                    if ch == "{":
                        depth += 1
                        opened = True
                    elif ch == "}":
                        depth -= 1
                    elif ch == ";" and not opened:
                        opened = True  # `mod tests;` / `use ..;` item ends here
                        depth = 0
                        break
                skip.add(j)
                if opened and depth == 0:
                    break
                j += 1
            i = j + 1
            continue
        i += 1
    return [(n + 1, code[n]) for n in range(len(code)) if n not in skip]


def fn_bodies(rel):
    """Yield (lineno, fn_name, body_text) for non-test fns in `rel`."""
    kept = non_test_lines(rel)
    src = "\n".join(c for _, c in kept)
    lnos = [n for n, _ in kept]
    scan = _lex(src)[1]
    for m in re.finditer(r'\bfn\s+([A-Za-z_][A-Za-z0-9_]*)', scan):
        start = scan.find("{", m.end())
        semi = scan.find(";", m.end())
        if start < 0 or (0 <= semi < start):
            continue
        depth, k = 0, start
        while k < len(scan):
            if scan[k] == "{":
                depth += 1
            elif scan[k] == "}":
                depth -= 1
                if depth == 0:
                    break
            k += 1
        yield lnos[src.count("\n", 0, m.start())], m.group(1), src[start:k + 1]


def scan_rules(rel, rules):
    """Line rules: (label, regex) over non-test code of `rel`."""
    kept = non_test_lines(rel)
    out = []
    for label, pat in rules:
        for n, c in kept:
            if re.search(pat, c):
                out.append(f"{rel}:{n}: [{label}] {c.strip()}")
    return out


def _names_alt():
    return "|".join(re.escape(n) for n in plugin_names())


def version_pickers(rel):
    """fns hand-rolling a "current version" pick: enumerate a dir, sort, take
    the last. Version ordering lives in plugin_bin (numeric, not lexical)."""
    out = []
    for n, name, body in fn_bodies(rel):
        if ("read_dir(" in body and re.search(r'\.sort(_by\w*)?\(', body)
                and re.search(r'\.(pop|last|max)\(\)', body)):
            out.append(f"{rel}:{n}: [ad-hoc cache version pick] fn {name}")
    return out


def erased_read_dir(rel):
    """`read_dir(..)` whose error is erased by `.ok()?` (missing and
    unreadable both become "nothing here")."""
    src = "\n".join(c for _, c in non_test_lines(rel))
    lnos = [n for n, _ in non_test_lines(rel)]
    return [f"{rel}:{lnos[src.count(chr(10), 0, m.start())]}: "
            "[read_dir(..).ok()? erasure]"
            for m in re.finditer(r'read_dir\([^;]*?\)\s*\.ok\(\)\s*\?', src)]


REMAINING = {
    "crates/fugu-router/src/budget.rs": "budgetguard",
    "crates/harness-status/src/path_shadow.rs": None,
    "crates/condukt/src/main.rs": "fugu-router",
    "crates/overwatch/src/bridge.rs": "backlog",
    "crates/condukt/src/state.rs": "gauge",
    "crates/condukt/src/oracle.rs": "tdd",
}


class RemainingAdHocResolvers(unittest.TestCase):
    def test_precondition_remaining_files_exist(self):
        for rel in REMAINING:
            self.assertTrue((REPO / rel).is_file(), rel)
        self.assertIn("gauge", plugin_names())  # plugin.json enumeration works

    def test_cfg_test_exclusion_is_not_vacuous(self):
        # The exclusion must drop the oracle.rs PATH shims (test-only) but keep
        # the production `plugin_bin::resolve("tdd")` call.
        kept = "\n".join(c for _, c in non_test_lines("crates/condukt/src/oracle.rs"))
        self.assertIn('plugin_bin::resolve("tdd")', kept)
        self.assertNotIn("split_paths", kept)
        full = (REPO / "crates/condukt/src/oracle.rs").read_text()
        self.assertIn("split_paths", full)

    def test_no_bare_spawn_or_adhoc_resolver_outside_plugin_bin(self):
        """backlog abba6f0d (remaining sites): open defect (RED observed)."""
        names = _names_alt()
        hits = []
        # 1. Sweep: no non-test code anywhere spawns a harness plugin binary
        #    by bare name (covers the 43780aa1 sites, oracle.rs, condukt
        #    main.rs, and any new site).
        bare = re.compile(r'Command::new\(\s*"(%s)"\s*\)' % names)
        for p in sorted(REPO.glob("crates/*/src/**/*.rs")):
            rel = str(p.relative_to(REPO))
            if rel == PLUGIN_BIN:
                continue
            hits += scan_rules(rel, [("bare-name spawn", bare)])
        # 2. Listed files: no hand-rolled resolver of any kind.
        common = [
            ("bare-name fallback",
             r'(PathBuf::from|unwrap_or(_else)?)\(\s*(\|\|\s*)?"(%s)"' % names),
            ("hand-built plugin cache path", r'\.join\(\s*"cache"\s*\)|plugins/cache'),
            ("installed_plugins.json resolver", r'installed_plugins'),
        ]
        path_scan = [("hand-rolled PATH scan", r'split_paths\(|var(_os)?\(\s*"PATH"\s*\)')]
        for rel, binary in REMAINING.items():
            # path_shadow's job IS to read $PATH (it diagnoses shadowing), so
            # only the PATH-scan rule is waived for it.
            rules = common + ([] if binary is None else path_scan)
            hits += scan_rules(rel, rules)
            hits += version_pickers(rel)
            hits += erased_read_dir(rel)
            if binary is not None:
                code = "\n".join(c for _, c in non_test_lines(rel))
                if not re.search(r'plugin_bin::\w+\(\s*"%s"' % re.escape(binary), code):
                    hits.append(f"{rel}: [missing] no harness_core::plugin_bin "
                                f"lookup of {binary!r} in non-test code")
        self.assertEqual(
            sorted(set(hits)), [],
            "ad-hoc plugin-binary resolution (not via harness_core::plugin_bin):\n"
            + "\n".join(sorted(set(hits))))

    def test_no_plugin_name_literal_into_a_bare_spawn_helper(self):
        """backlog abba6f0d (missed site, found by the independent verifier):
        a helper `fn f(.., program: &str, ..)` whose body does
        `Command::new(program)` spawns its argument by bare name, so a caller
        passing a harness plugin name literal (`run_tool(repo, "overwatch",
        ..)`) is the same bare-name spawn the sweep above forbids, hidden one
        call deep. Open defect (RED observed): condukt maintree.rs
        observe_peers."""
        helpers = spawn_param_helpers()
        # Anti-vacuity: the detector must see maintree's run_tool, and must NOT
        # flag helpers that resolve first (overwatch shell_source, stuckguard
        # heartbeat_via spawn a resolved local, not their name parameter).
        self.assertIn("run_tool", helpers)
        self.assertNotIn("shell_source", helpers)
        self.assertNotIn("heartbeat_via", helpers)

        hits = plugin_literal_calls(helpers)
        # Concrete pin for the known site: maintree must locate both binaries
        # through plugin_bin.
        rel = "crates/condukt/src/maintree.rs"
        code = "\n".join(c for _, c in non_test_lines(rel))
        for binary in ("overwatch", "backlog"):
            if not re.search(r'plugin_bin::(resolve|cache_lookup_in)\([^;]*"%s"'
                             % re.escape(binary), code):
                hits.append(f"{rel}: [missing] no harness_core::plugin_bin "
                            f"lookup of {binary!r} in non-test code")
        self.assertEqual(
            sorted(set(hits)), [],
            "harness plugin name literal passed to a helper that spawns its "
            "parameter by bare name (not via harness_core::plugin_bin):\n"
            + "\n".join(sorted(set(hits))))


def spawn_param_helpers():
    """{fn_name: [(rel, lineno, param)]} for non-test fns that pass one of
    their OWN `&str` parameters straight to `Command::new` (and do not call
    plugin_bin themselves)."""
    out = {}
    for p in sorted(REPO.glob("crates/*/src/**/*.rs")):
        rel = str(p.relative_to(REPO))
        kept = non_test_lines(rel)
        src = "\n".join(c for _, c in kept)
        lnos = [n for n, _ in kept]
        scan = _lex(src)[1]
        for m in re.finditer(r'\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*(<[^>]*>)?\s*\(', scan):
            start = scan.find("{", m.end())
            semi = scan.find(";", m.end())
            if start < 0 or (0 <= semi < start):
                continue
            sig = scan[m.end():start]
            params = set(re.findall(r'\b([a-z_][a-z0-9_]*)\s*:\s*&\s*str\b', sig))
            depth, k = 0, start
            while k < len(scan):
                if scan[k] == "{":
                    depth += 1
                elif scan[k] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                k += 1
            body = src[start:k + 1]
            if "plugin_bin::" in body:
                continue
            for arg in re.findall(r'Command::new\(\s*&?\s*([a-z_][a-z0-9_]*)\s*\)', body):
                if arg in params:
                    out.setdefault(m.group(1), []).append(
                        (rel, lnos[src.count("\n", 0, m.start())], arg))
    return out


def plugin_literal_calls(helpers):
    """Non-test call sites `helper(.., "<plugin>", ..)` for each helper."""
    names = _names_alt()
    lit = re.compile(r'"(%s)"' % names)
    hits = []
    for p in sorted(REPO.glob("crates/*/src/**/*.rs")):
        rel = str(p.relative_to(REPO))
        for n, c in non_test_lines(rel):
            if c.lstrip().startswith("#["):
                continue
            for fname, defs in helpers.items():
                for m in re.finditer(r'(?<![A-Za-z0-9_])%s\(([^;]*)' % re.escape(fname), c):
                    if re.search(r'\bfn\s+$', c[:m.start()]):
                        continue  # the definition itself
                    found = lit.search(m.group(1))
                    if found:
                        d = defs[0]
                        hits.append(
                            f"{rel}:{n}: [{fname}({found.group(0)}) -> "
                            f"Command::new({d[2]}) at {d[0]}:{d[1]}] {c.strip()}")
    return hits


if __name__ == "__main__":
    unittest.main()
