# blastguard: Python sink set (backlog 596ef57d, round-2 ruling 2026-11-20)

Status: specification document, no code. Line numbers are as of worktree HEAD
`acdfd56cb` (2026-11-20) and must be re-measured if the files move.

## Purpose

Ruling Q1 (2026-11-14) moves Python judgement from a denylist to an allowlist. The
round-2 ruling (2026-11-20) fixes what the allowlist applies to: only a **closed set of
side-effect-capable APIs ("sinks")**. A call that is not a sink is a pure call and is
not subject to the allowlist.

## Granularity

The unit is **API name + argument effect**.

1. A call whose resolved API is **not a sink** (see "Non-sinks") is not judged by the
   allowlist and stays Allow (for example `python_stdin_re_compile_fp_20261103.rs:73`
   stays green).
2. A sink API **not on the allowlist** resolves to **Ask** (A2a: "unknown program =
   Allow" does NOT apply to Python sinks).
3. A sink API **on the allowlist** is judged by its arguments (argv, paths, mode)
   through the existing shell effect and path rules. It is Allow only when those rules
   give Allow.
4. Arguments that are **not statically resolvable** resolve to **Ask**.

Examples from the ruling: `subprocess.run(['git','status','--short'])` is Allow;
`subprocess.run(['git','reset','--hard'])` is not Allow.

## The closed set

Class, then the concrete APIs already modelled in `crates/blastguard/src`. All
citations are in `crates/blastguard/src/detect/interpreter/python_semantics.rs` unless
stated.

### 1. Process spawn

- `os.system`, `os.popen`, `os.exec*` (`execv`, `execve`, `execl`, `execle`, `execlp`,
  `execlpe`, `execvp`, `execvpe`), `os.spawn*`, `os.posix_spawn`, `os.posix_spawnp`
  (`RUNNERS`, L250-269)
- `subprocess.run`, `call`, `check_call`, `check_output`, `Popen`, `getoutput`,
  `getstatusoutput`; `commands.getoutput`, `commands.getstatusoutput` (L274-281)
- `pty.spawn`, `asyncio.create_subprocess_exec`, `asyncio.create_subprocess_shell`
  (L284-286; renamed to `subprocess.run` / `os.system` for judging by `SPAWNER_AS`,
  L299-301)

Not modelled (no effect model found at HEAD): `loop.subprocess_shell` /
`loop.subprocess_exec` (case `stdin_x_loop_subprocess_shell`). Per A2a this must be
Ask until an effect model exists.

### 2. Filesystem write

- `open(path, mode)`, `io.open`, `codecs.open`, `builtins.open` with a write, append,
  exclusive or update mode (L1481-1490)
- `os.open` with `O_WRONLY` / `O_RDWR` / `O_CREAT` / `O_TRUNC` (L1491-1508)
- `Path.write_text`, `Path.write_bytes`, `Path.touch`, `Path.open(mode)` (L1517-1519,
  L1543)
- `os.truncate` (L1473, effect Write)
- remove: `os.remove`, `os.unlink`, `os.rmdir`, `os.removedirs`, `shutil.rmtree`,
  `Path.unlink`, `Path.rmdir` (L1461-1465, L1513)

### 3. Filesystem metadata (chmod / chown / utime / truncate)

- `os.chmod`, `os.lchmod`, `Path.chmod`, `Path.lchmod` (L1474-1479, L1514-1516)
- `os.chown`, `os.lchown`, `shutil.chown` (L1480)
- `os.truncate` (L1473)
- `os.utime`: no effect model found at HEAD (case `stdin_x_os_utime_chown` only
  exercises `os.chown`). Per A2a an unmodelled metadata sink must be Ask.

### 4. Filesystem move / link / replace

- `shutil.copy`, `copy2`, `copyfile`, `copytree`, `move`; `os.rename`, `os.replace`,
  `os.renames` (destination judged as a write, L1467-1472)
- `Path.replace`, `Path.rename` (one-argument form, L1526-1540)
- `Path.symlink_to`, `Path.hardlink_to` (L1520)
- `os.symlink`, `os.link`: no model found in this file at HEAD; they are Ask by the
  aggregate test (`os_symlink_zshrc`, `os_link`) through other paths. They remain in
  the set and must stay not-Allow.

### 5. Network

- No effect model found at HEAD. `socket` is only listed as an importable stdlib
  module (`STDLIB`, L1792). Members of the set: `urllib.request.urlopen`,
  `urllib.request.urlretrieve`, `socket.create_connection`, `socket.socket(...).send`,
  `http.client.*`. Not on the allowlist, therefore Ask (A2a).

### 6. exec / eval

- `exec`, `eval`, `compile` + exec, `__import__`, `importlib.import_module`,
  `runpy.run_path`, `runpy.run_module`, `importlib.util` `exec_module`,
  `pickle.loads`, `marshal.loads`, `code.interact`. No effect model found for these
  names in `python_semantics.rs` at HEAD (the only `__import__` hit, L536, is a doc
  comment). Not on the allowlist, therefore Ask.

## Example non-sinks (pure calls, never judged by the allowlist)

`re.compile`, `re.sub`, `re.findall`, `print`, `os.path.join`, `os.path.expanduser`
(path value resolver, L1090-1093), `os.getcwd`, `json.loads`, `json.dumps`,
`str.replace`, `str.split`, `len`, `Path(...)` construction, `Path.read_text`.

Note: `json.dump(obj, open(p, 'w'))` is judged through its inner `open(p, 'w')`
(class 2), not through `json.dump`.

## Initial allowlist

Per A2b, exactly the sink APIs used in the FP array of
`heredoc_script_fp_cases_stay_allow`
(`crates/blastguard/tests/heredoc_script_failopen_red_20261108.rs`, `FP`, L432-466),
extended later only through RED tests:

| API (class) | FP cases (test line) |
|---|---|
| `open(p)` read, `open(p, 'w')` / `open(p, 'w', encoding=...)` write (2) | `fp_read_replace_write` 433, `fp_with_open` 434, `fp_re_sub` 437, `fp_json_edit` 438, `fp_glob_edit` 440, `fp_walk_edit` 441, `fp_re_sub_version` 446, `fp_class_edit` 447, `fp_tmp_write` 448, `fp_listdir_edit` 458, `fp_abs_repo_write` 461, `fp_edit_with_heredoc_text` 465 |
| `Path.write_text` (2) | `fp_pathlib` 435, `fp_rglob_edit` 460 |
| `subprocess.run` (1) | `fp_git_status` 442, `fp_cargo_fmt` 443, `fp_subprocess_git_commit_msg` 462 |
| `subprocess.check_output` (1) | `fp_git_diff` 444 |
| `shutil.copy` (4) | `fp_shutil_copy_repo` 449 |
| `shutil.move` (4) | `fp_shutil_move_repo` 464 |
| `os.rename` (4) | `fp_os_rename_repo` 463 |
| `os.remove` (2) | `fp_remove_target` 451 |
| `os.makedirs` (2, fs create; no effect model found in `python_semantics.rs` at HEAD) | `fp_makedirs` 450 |

Even for an allowlisted API, the arguments are judged (granularity rule 3): the same
APIs with a protected path (`/Users/<n>/.ssh/**`, gate scripts) or a destructive argv
(`git reset --hard`, `git clean -fdx`) are not Allow.
