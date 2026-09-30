#!/usr/bin/env python3
"""Every blocking gate must state what it protects, from what, on what grounds.

Backlog 3a8e3b73 (user principle 2026-08-05): a refusal that names only its
mechanism leaves the user unable to tell whether the concern is real. Each gate
in the canonical list declares a `harness_core::gate::Protection` literal:

    pub const PROTECTION: Protection = Protection {
        protects: "...",   // the asset
        against: "...",    // the failure / adversary
        grounds: "...",    // cited code or a recorded incident
    };

This script is what pins those declarations; the Rust type cannot, because a
`const` cannot reject an empty string.

Inputs (all read from the repository given by `--repo PATH`, default the
current directory):
  * crates/harness-core/src/fleet.rs — `pub const BLOCKING_GATES: &[&str] =
    &[...];` (the list of gates that must declare) and `pub const GATE_CRATES`
    (the canary set; BLOCKING_GATES must be a superset of it).
  * crates/<gate>/src/**/*.rs — the `Protection { ... }` literal.
  * scripts/check-gate-protection.baseline — the gates still PENDING (not yet
    declared), one per line, `#` comments allowed. A ratchet: it may shrink,
    never grow, relative to the copy committed at HEAD.

Exit codes:
  0  every listed gate is either declared with three non-empty fields or listed
     PENDING in the baseline, and nothing below failed.
  1  a violation: an empty field; a gate neither declared nor PENDING; a gate
     both declared and still PENDING (remove it from the baseline — the ratchet
     must tighten); a baseline entry that is not a blocking gate; PENDING grew
     relative to HEAD (a baseline absent at HEAD counts as an empty one, so
     every entry is growth); BLOCKING_GATES not a superset of GATE_CRATES.
  2  undetermined: a list or the baseline cannot be read or parsed, a
     declaration is not a parseable literal, a gate has more than one
     declaration, or git cannot report HEAD's baseline. Undetermined is never
     reported as exit 0: a gate that could not be inspected has not passed.

Deferred to later slices of 3a8e3b73: the one-line refusal summary and the
docs/gate-taxonomy.md protection column.
"""
import argparse
import re
import subprocess
import sys
from pathlib import Path

FLEET = "crates/harness-core/src/fleet.rs"
BASELINE = "scripts/check-gate-protection.baseline"
FIELDS = ("protects", "against", "grounds")

_RUST_STRING = r'"(?:\\.|[^"\\])*"'
_RUST_COMMENT = r"/\*.*?\*/|//[^\n]*"
_STRIP_RE = re.compile(f"(?P<s>{_RUST_STRING})|(?:{_RUST_COMMENT})", re.S)


class Undetermined(Exception):
    pass


def strip_comments(text):
    """Blank Rust comments, leaving string literals (and comment markers inside
    them) intact. An unterminated string makes the string alternative fail to
    match, so its opening quote survives as-is and the later literal parse
    reports it as unparseable rather than silently accepting it."""
    return _STRIP_RE.sub(lambda m: m.group("s") if m.group("s") is not None else " ", text)


def unescape(lit):
    """Decode the body of an ordinary Rust string literal. Anything outside the
    escapes handled here is undetermined rather than guessed at."""
    out = []
    i = 0
    while i < len(lit):
        c = lit[i]
        if c != "\\":
            out.append(c)
            i += 1
            continue
        nxt = lit[i + 1] if i + 1 < len(lit) else ""
        if nxt == "\n":
            i += 2
            while i < len(lit) and lit[i] in " \t\r\n":
                i += 1
        elif nxt in ('"', "\\", "'"):
            out.append(nxt)
            i += 2
        elif nxt == "n":
            out.append("\n")
            i += 2
        elif nxt == "t":
            out.append("\t")
            i += 2
        else:
            raise Undetermined(f"unsupported escape \\{nxt!s} in string literal")
    return "".join(out)


def parse_str_list(text, name, path):
    pat = re.compile(r"pub\s+const\s+" + name + r"\s*:[^=]*=\s*&?\[(.*?)\]\s*;", re.S)
    matches = list(pat.finditer(strip_comments(text)))
    if len(matches) != 1:
        raise Undetermined(
            f"{path}: expected exactly one `pub const {name}` literal, found {len(matches)}"
        )
    body = matches[0].group(1)
    items = re.findall(r'"([A-Za-z0-9_-]+)"', body)
    rest = re.sub(r'"[A-Za-z0-9_-]+"', "", body)
    if rest.replace(",", "").strip() or not items:
        raise Undetermined(f"{path}: `{name}` is not a non-empty list of plain string literals")
    return items


_DECL_START = re.compile(r"\bProtection\s*\{")
_FIELD = re.compile(r'\s*([a-z_]+)\s*:\s*(' + _RUST_STRING + r')\s*(,|(?=\}))', re.S)


def parse_declaration(text, start, where):
    """Parse the `{ protects: "..", against: "..", grounds: ".." }` body that
    starts at `start` (just after the `{`). Exactly those three fields, each an
    ordinary string literal, in any order."""
    pos = start
    fields = {}
    while True:
        m = re.compile(r"\s*\}").match(text, pos)
        if m:
            break
        m = _FIELD.match(text, pos)
        if not m:
            raise Undetermined(f"{where}: Protection literal is not parseable")
        name = m.group(1)
        if name not in FIELDS or name in fields:
            raise Undetermined(f"{where}: unexpected or repeated field `{name}`")
        fields[name] = unescape(m.group(2)[1:-1])
        pos = m.end()
    missing = [f for f in FIELDS if f not in fields]
    if missing:
        raise Undetermined(f"{where}: Protection literal lacks {', '.join(missing)}")
    return fields


def declarations_in(src_dir, repo):
    """[(where, fields)] for every Protection literal under src_dir."""
    found = []
    if not src_dir.exists():
        return found
    if not src_dir.is_dir():
        raise Undetermined(f"{src_dir} exists but is not a directory")
    try:
        files = sorted(src_dir.rglob("*.rs"))
    except OSError as e:
        raise Undetermined(f"cannot walk {src_dir}: {e}")
    for f in files:
        try:
            text = strip_comments(f.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError) as e:
            raise Undetermined(f"cannot read {f}: {e}")
        for m in _DECL_START.finditer(text):
            before = text[: m.start()].rstrip()
            # The type's own definition / impl blocks / return types are not
            # declarations.
            if re.search(r"(\bstruct|\bimpl|\bfor|->)\s*$", before) or re.search(
                r"\bimpl\b[^;{}]*$", before
            ):
                continue
            where = f"{f.relative_to(repo)}"
            found.append((where, parse_declaration(text, m.end(), where)))
    return found


def read_baseline_text(text, where):
    entries = []
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        if not re.fullmatch(r"[A-Za-z0-9_-]+", line):
            raise Undetermined(f"{where}: unparseable baseline line {line!r}")
        entries.append(line)
    return set(entries)


def head_baseline(repo):
    """HEAD's PENDING set. A baseline absent at HEAD is an empty set; a git
    failure is undetermined."""
    ls = subprocess.run(
        ["git", "-C", str(repo), "ls-tree", "--name-only", "HEAD", "--", BASELINE],
        capture_output=True,
        text=True,
    )
    if ls.returncode != 0:
        raise Undetermined(f"git ls-tree HEAD failed (rc={ls.returncode}): {ls.stderr.strip()}")
    if ls.stdout.strip() != BASELINE:
        return set()
    show = subprocess.run(
        ["git", "-C", str(repo), "show", f"HEAD:{BASELINE}"], capture_output=True, text=True
    )
    if show.returncode != 0:
        raise Undetermined(f"git show HEAD:{BASELINE} failed (rc={show.returncode})")
    return read_baseline_text(show.stdout, f"HEAD:{BASELINE}")


def check(repo):
    """Return (violations, undetermined, report_lines)."""
    violations, undetermined, report = [], [], []
    try:
        fleet_text = (repo / FLEET).read_text(encoding="utf-8")
        blocking = parse_str_list(fleet_text, "BLOCKING_GATES", FLEET)
        canary = parse_str_list(fleet_text, "GATE_CRATES", FLEET)
    except (OSError, UnicodeDecodeError) as e:
        return [], [f"cannot read {FLEET}: {e}"], report
    except Undetermined as e:
        return [], [str(e)], report

    missing = sorted(set(canary) - set(blocking))
    if missing:
        violations.append(f"BLOCKING_GATES is not a superset of GATE_CRATES: missing {missing}")

    try:
        pending = read_baseline_text((repo / BASELINE).read_text(encoding="utf-8"), BASELINE)
    except (OSError, UnicodeDecodeError) as e:
        return violations, [f"cannot read {BASELINE}: {e}"], report
    except Undetermined as e:
        return violations, [str(e)], report

    try:
        grown = sorted(pending - head_baseline(repo))
        if grown:
            violations.append(f"PENDING grew relative to HEAD: {grown} (the baseline may only shrink)")
    except Undetermined as e:
        undetermined.append(str(e))

    unknown = sorted(pending - set(blocking))
    if unknown:
        violations.append(f"{BASELINE} lists gates that are not in BLOCKING_GATES: {unknown}")

    for gate in blocking:
        try:
            decls = declarations_in(repo / "crates" / gate / "src", repo)
        except Undetermined as e:
            undetermined.append(f"{gate}: {e}")
            continue
        if len(decls) > 1:
            undetermined.append(
                f"{gate}: {len(decls)} Protection literals ({', '.join(w for w, _ in decls)}); "
                "cannot tell which one is the gate's statement"
            )
            continue
        if not decls:
            if gate in pending:
                report.append(f"PENDING   {gate}")
            else:
                violations.append(
                    f"{gate}: no Protection declaration under crates/{gate}/src/ and not PENDING in {BASELINE}"
                )
            continue
        where, fields = decls[0]
        empty = [f for f in FIELDS if not fields[f].strip()]
        if empty:
            violations.append(f"{gate}: empty {', '.join(empty)} in {where}")
            continue
        if gate in pending:
            violations.append(
                f"{gate}: declared in {where} but still PENDING in {BASELINE}; remove it from the baseline"
            )
            continue
        report.append(f"DECLARED  {gate}  ({where})")
    return violations, undetermined, report


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default=".", help="repository root (default: cwd)")
    args = ap.parse_args(argv)
    repo = Path(args.repo).resolve()

    violations, undetermined, report = check(repo)
    for line in report:
        print(f"gate-protection: {line}")
    for v in violations:
        print(f"gate-protection: VIOLATION: {v}", file=sys.stderr)
    for u in undetermined:
        print(f"gate-protection: UNDETERMINED: {u}", file=sys.stderr)
    if undetermined:
        return 2
    if violations:
        return 1
    print("gate-protection: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
