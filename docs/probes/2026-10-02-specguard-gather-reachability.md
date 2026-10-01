# specguard forge/gather reachability probe (2026-10-02)

condukt run `run-20261002-040905-21607`, task `c3-probe-specguard-gather-branch`,
backlog `80a46e9f`. This is a **measurement record**. Nothing in specguard was
changed. It records what `scripts/reachability-probe.py` observed. It does not
deliver a verdict on finding `9a5e3c28`.

## What a probe result does and does not prove (read this first)

- **"The arm was reached" (`reproduced`) does NOT prove "a permissive outcome is
  reachable".** A `reproduced` result with the panic marker means only that at
  least one test in the suite executes the mutated arm. It says nothing about
  whether that execution leads to a clean or permissive result downstream.
- **`not_reproduced` proves only that no test in the suite reached the arm**
  (more exactly, that the test command still exited 0 with the arm replaced by a
  panic). The probe's own note applies verbatim: *"not_reproduced does not
  establish unreachability: it only means the mutated branch did not turn the
  test command red."* It is a statement about the test suite, not about the
  program.
- `panic_marker_seen` is informational only. The verdict comes from exit codes.
  See H2 below for a case where the marker was seen even though no panic ran.

## The finding

**Overwatch ledger lookup: not found.** `grep -rl 9a5e3c28 ~/.overwatch` returned no
file (the `~/.overwatch` tree holds per-tmpdir `review_findings.jsonl` stores; none of
them contain the id). `overwatch` 0.2.47 has no list/show subcommand for findings,
only `review-queue`. `9a5e3c28` is a **backlog** id. Its entry is quoted verbatim below
from `.backlog/tasks.done.toml` at worktree HEAD `fc8cc3a6`:

```toml
[[task]]
id = "9a5e3c28"
title = "specguard forge/gather の走査を testaudit と同じ fail-closed 形に揃える"
project = "/Users/yuki/src/harness"
project_unresolved = false
tags = [
    "L5",
    "scout",
    "failopen",
    "p1",
]
touched_files = []
status = "done"
notes = """
05df9b2 は testaudit.rs の read_dir/read_to_string 握り潰しを decision.rs にミラーして直したが、同一 crate 内の forge/gather.rs には同じ3形状が残る。読めないサブツリーが黙って走査対象から消え「該当仕様なし」で監査が通る / 証拠: crates/specguard/src/forge/gather.rs:107 'let Ok(entries) = std::fs::read_dir(dir) else {' :110 'entries.flatten().collect()' :127 と :237 'let Ok(text) = std::fs::read_to_string(..) else {'。対比 crates/specguard/src/decision.rs:44-45 '// EXISTS but is unreadable is incomplete input, not "no decisions" → fail closed' / 完了条件: walk_suffix / md_fragments が Result を返し NotFound 以外のエラーを伝播。unreadable subtree を仕込んだテストで GREEN にならない
敵対的検証で REFUTED: 消費者が違う。gather は specforge (生成側ハーネス) にコンパイルされ、監査経路の decision.rs/testaudit.rs は 05df9b2 で修正済み。fragment 欠落は forge の rigor pre-flight (G1-G4 sentinel on shortfall) を停止させる方向 = restrictive であり permissive な到達路が示せない
【verdict 再分類 2026-07-21: REFUTED → CONFIRMED】三値化 (6af99c5c) に伴う再審査。上記 REFUTED は「permissive な到達路が示せない」に依拠しているが、これは『経路を辿れなかった』であって『経路が無い』の立証ではない (追ったのは shortfall→sentinel の 1 経路のみ)。反証成立の証拠: 別セッションの修正 crates/specguard/src/forge/main.rs:56 'const EXIT_INTAKE_INCOMPLETE: u8 = 8;' / :436 'println!("intake_incomplete: yes");' / :438 'return Ok(EXIT_INTAKE_INCOMPLETE);' — 件数閾値を満たす部分的な束が clean として下流へ渡る経路が実在したため、この専用 exit code が追加された。よって指摘は実在 = CONFIRMED。status:done は『修正が landed した』意味で維持する (REFUTED による棄却ではない)。"""
created_at = 1784603234
updated_at = 1784637516
defer_until = 1784778341
weight = 0.0
```

The task driving this probe is backlog `80a46e9f`. It is quoted verbatim from
`/Users/yuki/src/.harness-worktrees/session-c6ed7cfd/.backlog/tasks.toml`.
`backlog show` does not exist in the installed CLI ("unrecognized subcommand
'show'").

```toml
[[task]]
id = "80a46e9f"
title = "overwatch に決定論の裁定を入れ、REFUTED を witness 無しで成立させない"
project = "/Users/yuki/src/harness"
project_unresolved = false
tags = [
    "L5",
    "scout",
    "verified",
    "p1",
]
touched_files = []
status = "pending"
notes = """
CLAUDE.md:188-197 に記録された実測の失敗 — 既定 REFUTED という指示が検証者を棄却側に倒し、4件の REFUTED を無検証で採用した結果うち1件が実在の欠陥だった (specguard forge gather.rs)。crates/overwatch/src/review_finding.rs:108 は壊れた入力を既に Unverified に倒しているが、整形式の反証を裁定するものが無い。adjudicate(llm_verdict, probe_result) を入れ、(Refuted, NoWitness) から Refuted への辺を持たせない。到達性プローブ P4 (到達不能と主張された分岐の本体を panic に置換して全スイートと proptest を回す) は反証を偽なりと示せるが確立はできない — witness が見つからなかったは witness が無いではない。プローブ機構は新規ではなく scripts/check-fail-open-mutation.py が既に apply して cargo test して exit code を assert して revert しバイト同一を検証する形を持つ。受け入れる代償を明示: REFUTED を閉じられるのは人間だけになるのでキューは人間が働くまで単調増加する。完了条件: adjudicate が唯一の verdict 変更経路になり、refuted_without_a_witness_never_survives_adjudication を先に誤実装 (llm をそのまま返す) に対して RED で観測してから実装する。歴史的に誤棄却された gather.rs の分岐に P4 を当てて Reproduced が出ることを確認し、出なければプローブが弱いという事実をそのまま記録する (プローブを十分と宣言しない)。

8f5901a7 で取り込み済み: record-finding は adjudicate を通し、REFUTED は probe が not_reproduced かつ人の sign-off がある場合だけ保存される。reproduced なら confirmed、それ以外は unverified。continuous-audit は verifier の REFUTED を捨てずに unverified として記録するよう変更。残り (done 条件未達): gather.rs の誤棄却箇所に対する probe の実行。probe を生成する道具がまだ無い。"""
created_at = 1785926766
updated_at = 1790793912
weight = 0.0
issue_number = 148
issue_url = "https://github.com/yukineko/claude-harnesses/issues/148"
```

## Revisions

| label | commit | how it was found |
|---|---|---|
| OLD | `786c1ce7189b176ae512e159c7257ba9d4ecc6dc` | parent of `02b80c629f62222507cd7a3a04dbaba9cf9a0114`, which is the only (and therefore oldest) hit of `git log -S EXIT_INTAKE_INCOMPLETE --oneline -- crates/specguard` ("fix(specguard): forge gather intake fails closed on an unreadable source (9a5e3c28)") |
| HEAD | `fc8cc3a62e40a007e383c6e1a8cb5626470f6ac4` | HEAD of the probe worktree |

OLD was checked out with `git worktree add --detach <scratchpad>/c3-old 786c1ce7…` and
removed afterwards with `git worktree remove`. The probe script was always run from the
HEAD worktree (`scripts/reachability-probe.py` at `fc8cc3a6`).

**Caveat on the `rev` field in the result JSON.** The script fills `rev` with `git
rev-parse HEAD` of the repo that contains *the script*, not the tree being probed.
So every result JSON below says `fc8cc3a6…`, including the OLD-revision probes.
For the OLD rows the authoritative revision is the `file` path (`…/c3-old/…`) plus the
table above. This is a limitation of the probe script and is recorded here, not fixed.

## Probe targets

(a) Every arm in `crates/specguard/src/forge/gather.rs` that handles a non-NotFound IO
error. (b) The branch in `crates/specguard/src/forge/main.rs` that consumes an
unreadable intake.

- At **HEAD** the non-NotFound arms are explicit `Err(e) =>` arms (H1 to H4). The consumer
  is the `if !outcome.unreadable.is_empty() { … return Ok(EXIT_INTAKE_INCOMPLETE); }` body
  in `cmd_gather` (H5). For each arm, the arm body was replaced by
  `panic!("reachability-probe")`.
- At **OLD** there are **no non-NotFound-specific arms**. Errors were handled by
  catch-all `let … else` bodies that also catch NotFound (O1, O3, O4), or they were
  discarded by `.flatten()`, which has no arm at all (O2). So the OLD probes cover a
  **superset** of the non-NotFound path. A `reproduced` result there does not show that
  a non-NotFound error reached the arm.
  - O2 is a **synthesized arm**. `entries.flatten().collect()` was replaced by
    `entries.map(|r| r.unwrap_or_else(|_| panic!("reachability-probe"))).collect()`,
    which panics exactly when an `Err` entry would have been silently dropped.
- **(b) at OLD: absent.** `forge/main.rs` at `786c1ce7` contains no `unreadable` or
  `EXIT_INTAKE_INCOMPLETE` code. It only has `EXIT_INTAKE_SHORTFALL` (empty bundle),
  which is not an unreadable-intake consumer. Nothing was probed for (b) at OLD.

Every anchor occurred exactly once in its file. This was checked before running, and
the probe re-checks it itself. No anchor failed to anchor uniquely.

## Results (arm x revision)

| arm | location | revision | probe exit | verdict | test exit | panic_marker_seen |
|---|---|---|---|---|---|---|
| H1 | HEAD `walk_suffix`: `read_dir` `Err(e)` (non-NotFound) arm | fc8cc3a6 (HEAD) | 0 | reproduced | 101 | true |
| H2 | HEAD `walk_suffix`: per-entry `Err(e)` arm | fc8cc3a6 (HEAD) | 0 | not_reproduced | 0 | true |
| H3 | HEAD `md_fragments`: `read_to_string` `Err(e)` (non-NotFound) arm | fc8cc3a6 (HEAD) | 0 | not_reproduced | 0 | false |
| H4 | HEAD `gather_transcripts`: `read_to_string` `Err(e)` (non-NotFound) arm | fc8cc3a6 (HEAD) | 0 | not_reproduced | 0 | false |
| H5 | HEAD `forge/main.rs` `cmd_gather`: `if !outcome.unreadable.is_empty()` body (consumer, returns `EXIT_INTAKE_INCOMPLETE`) | fc8cc3a6 (HEAD) | 0 | not_reproduced | 0 | false |
| O1 | OLD `walk_suffix`: `let Ok(entries) = read_dir(dir) else { return; }` (catch-all, includes NotFound) | 786c1ce7 (parent of 02b80c62) | 0 | reproduced | 101 | true |
| O2 | OLD `walk_suffix`: `entries.flatten()` (no arm exists; probe SYNTHESIZES one, see below) | 786c1ce7 (parent of 02b80c62) | 0 | not_reproduced | 0 | false |
| O3 | OLD `md_fragments`: `let Ok(text) = read_to_string(path) else { return Vec::new(); }` (catch-all) | 786c1ce7 (parent of 02b80c62) | 0 | not_reproduced | 0 | false |
| O4 | OLD `gather_transcripts`: `let Ok(text) = read_to_string(&f) else { continue; }` (catch-all) | 786c1ce7 (parent of 02b80c62) | 0 | not_reproduced | 0 | false |
| O5 | OLD `forge/main.rs` unreadable-intake consumer | 786c1ce7 | n/a | **not probed: branch does not exist at this revision** | n/a | n/a |

No probe returned exit 2 (undetermined). All nine wrote a result file. Both baselines
(`cargo test -p specguard` unmutated) were green, because the probe refuses to run on a
red baseline.

### Observations beyond the table (from a diagnostic re-run)

The probe does not store test output. To see *which* test reached an arm, H1, H2 and O1
were re-run with the same anchor, replacement and build command, but with the test
command changed to `bash -c "cd '<tree>' && cargo test -p specguard > '<log>' 2>&1"`.
The verdicts matched the main run: H1 reproduced/101, H2 not_reproduced/0, O1
reproduced/101. (`panic_marker_seen` in those diagnostic JSONs is false only because
the output was redirected away from the probe.) From the logs:

- **H1**: `thread 'gather::tests::unreadable_existing_source_is_surfaced_not_silently_dropped' … panicked at crates/specguard/src/forge/gather.rs:120:13: reachability-probe`.
  This is the one test that chmods a decisions dir to `000`.
- **O1**: `thread 'gather::tests::gather_walks_sources_and_bundles' … panicked at crates/specguard/src/forge/gather.rs:108:9: reachability-probe`.
  That test's fixture does not chmod anything and does not create
  `vault/AEGIS/sessions`, but `gather_obsidian` walks that directory at OLD. So the
  error that reached the catch-all `else` was **probably NotFound**. This is an
  inference from reading the fixture: the error kind was not observed. Either way, O1's
  `reproduced` does **not** show that a non-NotFound error reaches this arm.
- **H2**: `panic_marker_seen: true` but `not_reproduced` / test exit 0. The marker came
  from a replayed compiler warning, not from a panic. The log shows
  `130 |             Err(e) => panic!("reachability-probe"),` under an
  `unused_variables` warning. All test binaries reported `ok`. This is the
  "informational only" caveat in practice.
- `cargo test -p specguard` stops at the first failing test binary because
  `--no-fail-fast` is not set. In the reproduced runs, only the `specforge` bin's unit
  tests ran before the stop. This does not change the verdicts.
- The only `catch_unwind` in specguard is in `src/agent.rs`, which is not on the gather
  path. So the "catch_unwind may have swallowed the panic" text in the not_reproduced
  notes has no supporting site on these paths. That is a grep result, not a proof.

### What this measurement shows, stated narrowly

- At HEAD, the suite executes exactly one non-NotFound arm (H1, `walk_suffix`
  `read_dir`). It does not execute H2, H3 or H4. It does not execute the main.rs
  consumer branch (H5) at all: no test drives `specforge gather` into exit 8.
- At OLD, the suite executes the catch-all `read_dir` else (O1), apparently through
  NotFound. It does not execute the `.flatten()` drop (O2), the `md_fragments` else
  (O3), or the transcripts else (O4). OLD has no consumer branch.
- None of this says whether a permissive outcome is or was reachable. The
  not_reproduced rows mean the suite does not reach those arms. They do not mean the
  arms are unreachable.

## Exact commands

Environment: `. "$HOME/.cargo/env"`. Probes were run sequentially (each mutates a file
in place, then restores it byte-identically) by a scratch driver that called the
following argv lists. `<scratch>` =
`/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad`.

#### H1-walk_suffix-read_dir-Err

```json
[
  "python3",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/scripts/reachability-probe.py",
  "--file",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/crates/specguard/src/forge/gather.rs",
  "--anchor",
  "unreadable.push(format!(\"{}: {e}\", dir.display()));\n            return;",
  "--replacement",
  "panic!(\"reachability-probe\");",
  "--crate",
  "specguard",
  "--build-cmd",
  "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo build -p specguard --tests",
  "--test-cmd",
  "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo test -p specguard",
  "--out",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-results/H1-walk_suffix-read_dir-Err.json"
]
```

probe exit: `0`, probe stderr: `''`

#### H2-walk_suffix-entry-Err

```json
[
  "python3",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/scripts/reachability-probe.py",
  "--file",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/crates/specguard/src/forge/gather.rs",
  "--anchor",
  "=> unreadable.push(format!(\"{}: {e}\", dir.display())),",
  "--replacement",
  "=> panic!(\"reachability-probe\"),",
  "--crate",
  "specguard",
  "--build-cmd",
  "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo build -p specguard --tests",
  "--test-cmd",
  "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo test -p specguard",
  "--out",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-results/H2-walk_suffix-entry-Err.json"
]
```

probe exit: `0`, probe stderr: `''`

#### H3-md_fragments-read_to_string-Err

```json
[
  "python3",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/scripts/reachability-probe.py",
  "--file",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/crates/specguard/src/forge/gather.rs",
  "--anchor",
  "unreadable.push(format!(\"{}: {e}\", path.display()));\n            return Vec::new();",
  "--replacement",
  "panic!(\"reachability-probe\");",
  "--crate",
  "specguard",
  "--build-cmd",
  "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo build -p specguard --tests",
  "--test-cmd",
  "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo test -p specguard",
  "--out",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-results/H3-md_fragments-read_to_string-Err.json"
]
```

probe exit: `0`, probe stderr: `''`

#### H4-gather_transcripts-read_to_string-Err

```json
[
  "python3",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/scripts/reachability-probe.py",
  "--file",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/crates/specguard/src/forge/gather.rs",
  "--anchor",
  "unreadable.push(format!(\"{}: {e}\", f.to_string_lossy()));\n                continue;",
  "--replacement",
  "panic!(\"reachability-probe\");",
  "--crate",
  "specguard",
  "--build-cmd",
  "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo build -p specguard --tests",
  "--test-cmd",
  "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo test -p specguard",
  "--out",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-results/H4-gather_transcripts-read_to_string-Err.json"
]
```

probe exit: `0`, probe stderr: `''`

#### H5-main-unreadable-consumer

```json
[
  "python3",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/scripts/reachability-probe.py",
  "--file",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/crates/specguard/src/forge/main.rs",
  "--anchor",
  "        eprintln!(\n            \"specforge: gather — 判定不能: 次のソースは存在するが読めない (intake が不完全 → 束は書かない):\"\n        );\n        for u in &outcome.unreadable {\n            eprintln!(\"  - {u}\");\n        }\n        println!(\"{}\", gather::MARKER);\n        println!(\"needs_user: yes\");\n        println!(\"intake_incomplete: yes\");\n        println!(\"fragment_count: {}\", bundle.fragments.len());\n        return Ok(EXIT_INTAKE_INCOMPLETE);",
  "--replacement",
  "        panic!(\"reachability-probe\");",
  "--crate",
  "specguard",
  "--build-cmd",
  "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo build -p specguard --tests",
  "--test-cmd",
  "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo test -p specguard",
  "--out",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-results/H5-main-unreadable-consumer.json"
]
```

probe exit: `0`, probe stderr: `''`

#### O1-walk_suffix-read_dir-else

```json
[
  "python3",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/scripts/reachability-probe.py",
  "--file",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old/crates/specguard/src/forge/gather.rs",
  "--anchor",
  "let Ok(entries) = std::fs::read_dir(dir) else {\n        return;\n    };",
  "--replacement",
  "let Ok(entries) = std::fs::read_dir(dir) else {\n        panic!(\"reachability-probe\");\n    };",
  "--crate",
  "specguard",
  "--build-cmd",
  "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo build -p specguard --tests",
  "--test-cmd",
  "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo test -p specguard",
  "--out",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-results/O1-walk_suffix-read_dir-else.json"
]
```

probe exit: `0`, probe stderr: `''`

#### O2-walk_suffix-flatten-synth

```json
[
  "python3",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/scripts/reachability-probe.py",
  "--file",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old/crates/specguard/src/forge/gather.rs",
  "--anchor",
  "entries.flatten().collect()",
  "--replacement",
  "entries.map(|r| r.unwrap_or_else(|_| panic!(\"reachability-probe\"))).collect()",
  "--crate",
  "specguard",
  "--build-cmd",
  "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo build -p specguard --tests",
  "--test-cmd",
  "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo test -p specguard",
  "--out",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-results/O2-walk_suffix-flatten-synth.json"
]
```

probe exit: `0`, probe stderr: `''`

#### O3-md_fragments-read_to_string-else

```json
[
  "python3",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/scripts/reachability-probe.py",
  "--file",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old/crates/specguard/src/forge/gather.rs",
  "--anchor",
  "let Ok(text) = std::fs::read_to_string(path) else {\n        return Vec::new();\n    };",
  "--replacement",
  "let Ok(text) = std::fs::read_to_string(path) else {\n        panic!(\"reachability-probe\");\n    };",
  "--crate",
  "specguard",
  "--build-cmd",
  "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo build -p specguard --tests",
  "--test-cmd",
  "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo test -p specguard",
  "--out",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-results/O3-md_fragments-read_to_string-else.json"
]
```

probe exit: `0`, probe stderr: `''`

#### O4-gather_transcripts-read_to_string-else

```json
[
  "python3",
  "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/scripts/reachability-probe.py",
  "--file",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old/crates/specguard/src/forge/gather.rs",
  "--anchor",
  "let Ok(text) = std::fs::read_to_string(&f) else {\n            continue;\n        };",
  "--replacement",
  "let Ok(text) = std::fs::read_to_string(&f) else {\n            panic!(\"reachability-probe\");\n        };",
  "--crate",
  "specguard",
  "--build-cmd",
  "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo build -p specguard --tests",
  "--test-cmd",
  "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo test -p specguard",
  "--out",
  "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-results/O4-gather_transcripts-read_to_string-else.json"
]
```

probe exit: `0`, probe stderr: `''`


## Result JSON (verbatim)

#### H1-walk_suffix-read_dir-Err

```json
{
  "result": "reproduced",
  "note": "not_reproduced does not establish unreachability: it only means the mutated branch did not turn the test command red.",
  "panic_marker_seen": true,
  "crate": "specguard",
  "file": "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/crates/specguard/src/forge/gather.rs",
  "anchor": "unreadable.push(format!(\"{}: {e}\", dir.display()));\n            return;",
  "replacement": "panic!(\"reachability-probe\");",
  "rev": "fc8cc3a62e40a007e383c6e1a8cb5626470f6ac4",
  "test_cmd": "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo test -p specguard",
  "build_cmd": "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo build -p specguard --tests",
  "baseline_exit_code": 0,
  "build_exit_code": 0,
  "test_exit_code": 101
}
```

#### H2-walk_suffix-entry-Err

```json
{
  "result": "not_reproduced",
  "note": "not_reproduced does not establish unreachability: it only means the mutated branch did not turn the test command red.",
  "panic_marker_seen": true,
  "crate": "specguard",
  "file": "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/crates/specguard/src/forge/gather.rs",
  "anchor": "=> unreadable.push(format!(\"{}: {e}\", dir.display())),",
  "replacement": "=> panic!(\"reachability-probe\"),",
  "rev": "fc8cc3a62e40a007e383c6e1a8cb5626470f6ac4",
  "test_cmd": "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo test -p specguard",
  "build_cmd": "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo build -p specguard --tests",
  "baseline_exit_code": 0,
  "build_exit_code": 0,
  "test_exit_code": 0
}
```

#### H3-md_fragments-read_to_string-Err

```json
{
  "result": "not_reproduced",
  "note": "not_reproduced does not establish unreachability: it only means the mutated branch did not turn the test command red. The panic marker was not seen; catch_unwind may have swallowed the panic.",
  "panic_marker_seen": false,
  "crate": "specguard",
  "file": "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/crates/specguard/src/forge/gather.rs",
  "anchor": "unreadable.push(format!(\"{}: {e}\", path.display()));\n            return Vec::new();",
  "replacement": "panic!(\"reachability-probe\");",
  "rev": "fc8cc3a62e40a007e383c6e1a8cb5626470f6ac4",
  "test_cmd": "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo test -p specguard",
  "build_cmd": "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo build -p specguard --tests",
  "baseline_exit_code": 0,
  "build_exit_code": 0,
  "test_exit_code": 0
}
```

#### H4-gather_transcripts-read_to_string-Err

```json
{
  "result": "not_reproduced",
  "note": "not_reproduced does not establish unreachability: it only means the mutated branch did not turn the test command red. The panic marker was not seen; catch_unwind may have swallowed the panic.",
  "panic_marker_seen": false,
  "crate": "specguard",
  "file": "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/crates/specguard/src/forge/gather.rs",
  "anchor": "unreadable.push(format!(\"{}: {e}\", f.to_string_lossy()));\n                continue;",
  "replacement": "panic!(\"reachability-probe\");",
  "rev": "fc8cc3a62e40a007e383c6e1a8cb5626470f6ac4",
  "test_cmd": "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo test -p specguard",
  "build_cmd": "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo build -p specguard --tests",
  "baseline_exit_code": 0,
  "build_exit_code": 0,
  "test_exit_code": 0
}
```

#### H5-main-unreadable-consumer

```json
{
  "result": "not_reproduced",
  "note": "not_reproduced does not establish unreachability: it only means the mutated branch did not turn the test command red. The panic marker was not seen; catch_unwind may have swallowed the panic.",
  "panic_marker_seen": false,
  "crate": "specguard",
  "file": "/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch/crates/specguard/src/forge/main.rs",
  "anchor": "        eprintln!(\n            \"specforge: gather — 判定不能: 次のソースは存在するが読めない (intake が不完全 → 束は書かない):\"\n        );\n        for u in &outcome.unreadable {\n            eprintln!(\"  - {u}\");\n        }\n        println!(\"{}\", gather::MARKER);\n        println!(\"needs_user: yes\");\n        println!(\"intake_incomplete: yes\");\n        println!(\"fragment_count: {}\", bundle.fragments.len());\n        return Ok(EXIT_INTAKE_INCOMPLETE);",
  "replacement": "        panic!(\"reachability-probe\");",
  "rev": "fc8cc3a62e40a007e383c6e1a8cb5626470f6ac4",
  "test_cmd": "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo test -p specguard",
  "build_cmd": "cd '/Users/yuki/.condukt/worktrees/run-20261002-040905-21607-c3-probe-specguard-gather-branch' && cargo build -p specguard --tests",
  "baseline_exit_code": 0,
  "build_exit_code": 0,
  "test_exit_code": 0
}
```

#### O1-walk_suffix-read_dir-else

```json
{
  "result": "reproduced",
  "note": "not_reproduced does not establish unreachability: it only means the mutated branch did not turn the test command red.",
  "panic_marker_seen": true,
  "crate": "specguard",
  "file": "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old/crates/specguard/src/forge/gather.rs",
  "anchor": "let Ok(entries) = std::fs::read_dir(dir) else {\n        return;\n    };",
  "replacement": "let Ok(entries) = std::fs::read_dir(dir) else {\n        panic!(\"reachability-probe\");\n    };",
  "rev": "fc8cc3a62e40a007e383c6e1a8cb5626470f6ac4",
  "test_cmd": "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo test -p specguard",
  "build_cmd": "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo build -p specguard --tests",
  "baseline_exit_code": 0,
  "build_exit_code": 0,
  "test_exit_code": 101
}
```

#### O2-walk_suffix-flatten-synth

```json
{
  "result": "not_reproduced",
  "note": "not_reproduced does not establish unreachability: it only means the mutated branch did not turn the test command red. The panic marker was not seen; catch_unwind may have swallowed the panic.",
  "panic_marker_seen": false,
  "crate": "specguard",
  "file": "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old/crates/specguard/src/forge/gather.rs",
  "anchor": "entries.flatten().collect()",
  "replacement": "entries.map(|r| r.unwrap_or_else(|_| panic!(\"reachability-probe\"))).collect()",
  "rev": "fc8cc3a62e40a007e383c6e1a8cb5626470f6ac4",
  "test_cmd": "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo test -p specguard",
  "build_cmd": "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo build -p specguard --tests",
  "baseline_exit_code": 0,
  "build_exit_code": 0,
  "test_exit_code": 0
}
```

#### O3-md_fragments-read_to_string-else

```json
{
  "result": "not_reproduced",
  "note": "not_reproduced does not establish unreachability: it only means the mutated branch did not turn the test command red. The panic marker was not seen; catch_unwind may have swallowed the panic.",
  "panic_marker_seen": false,
  "crate": "specguard",
  "file": "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old/crates/specguard/src/forge/gather.rs",
  "anchor": "let Ok(text) = std::fs::read_to_string(path) else {\n        return Vec::new();\n    };",
  "replacement": "let Ok(text) = std::fs::read_to_string(path) else {\n        panic!(\"reachability-probe\");\n    };",
  "rev": "fc8cc3a62e40a007e383c6e1a8cb5626470f6ac4",
  "test_cmd": "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo test -p specguard",
  "build_cmd": "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo build -p specguard --tests",
  "baseline_exit_code": 0,
  "build_exit_code": 0,
  "test_exit_code": 0
}
```

#### O4-gather_transcripts-read_to_string-else

```json
{
  "result": "not_reproduced",
  "note": "not_reproduced does not establish unreachability: it only means the mutated branch did not turn the test command red. The panic marker was not seen; catch_unwind may have swallowed the panic.",
  "panic_marker_seen": false,
  "crate": "specguard",
  "file": "/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old/crates/specguard/src/forge/gather.rs",
  "anchor": "let Ok(text) = std::fs::read_to_string(&f) else {\n            continue;\n        };",
  "replacement": "let Ok(text) = std::fs::read_to_string(&f) else {\n            panic!(\"reachability-probe\");\n        };",
  "rev": "fc8cc3a62e40a007e383c6e1a8cb5626470f6ac4",
  "test_cmd": "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo test -p specguard",
  "build_cmd": "cd '/private/tmp/claude-502/-Users-yuki-src-harness/c6ed7cfd-69ee-4869-88c3-683838d5d4d1/scratchpad/c3-old' && cargo build -p specguard --tests",
  "baseline_exit_code": 0,
  "build_exit_code": 0,
  "test_exit_code": 0
}
```

