> **REVIEW-NEEDED** — GitHub #1552 の契約 draft。実装前に RED テスト (`scripts/test_gate_severity_probe.py`) が固定した。

# gate-severity-probe 仕様

## 概要

`scripts/gate-severity-probe.py` は、pre-commit の各 gate について「壊れたら何が漏れるか」を測る。
ユーザー定義 (2026-11-06) の 3 指標を gate ごとに出す。time-to-detect は対象外。

CLI: `python3 scripts/gate-severity-probe.py --json [--repo PATH] [--gate NAME]...`

- `--repo PATH` — 対象 (caller) repo。省略時は cwd。
- `--gate NAME` — 繰り返し可。指定した gate だけ測る。未知の名前は非 0 終了（空結果や clean に見せない）。
  gate 名は `.githooks/pre-commit` が表示するラベル (`launcher-exec-bit` 等)。
- 成功時は exit 0 で stdout に JSON のみ。

## 出力

```json
{"gates": [{"gate": "launcher-exec-bit", "fires": true, "caught_by": ["pre-push"], "leak": null}]}
```

| key | 値 | 意味 |
|---|---|---|
| `fires` | `true` / `false` / `"undetermined"` | gate 単体で注入した代表違反を今ブロックするか |
| `caught_by` | 文字列のリスト | その gate **だけ**を無効化したとき、なお違反を止める他層（他の pre-commit gate、`commit-msg`、`pre-push`、`cargo-test`）。無効化した gate 自身は含めない |
| `leak` | `"local-commit"` / `"push"` / `"rollout"` / `"undetermined"` / `null` | `caught_by` が空のとき違反が到達する最遠点。他層が止めるなら `null` |

不変条件:

1. `caught_by` が非空 ⇒ `leak` は `null`。`caught_by` が空 ⇒ `leak` は名前付きの水準（`null` ではない）。
2. **undetermined は畳まない。** 判定不能 ⇒ `fires == "undetermined"` かつ `leak == "undetermined"`。
   `false` や `null` に潰さない（CLAUDE.md §3）。
   判定不能の例: 違反を注入しない baseline 実行ですでにその gate がブロックする
   （scanner 欠落・crash 等）— ブロックを注入違反に帰属できない。
3. gate ごとに代表 fixture を 1 つ持つ。`launcher-exec-bit` は `crates/<c>/bin/<name>` を index 上 100644 にする。

## 隔離（ユーザー裁定 2026-11-08）

- 注入と実行は **probe 自身が作って消す使い捨て worktree でのみ**行う。caller の作業ツリー・index・HEAD・branch は変えない
  （staged / untracked の in-flight 変更を含め byte 単位で保存）。
- 終了時に自分が作った worktree と管理 dir を残さない。
- caller の他 worktree を読まない。他 worktree の状態は結果に影響しない。唯一の例外は probe 自身が作る使い捨て worktree。
- 変更自身の範囲だけを判定する（whole-repo チェックをしない）。

## テスト

`scripts/test_gate_severity_probe.py` — 契約の各性質（形、fires、caught_by/leak の整合、`--gate` filter と未知 gate、
undetermined の非畳み込み、caller 不変、使い捨て worktree の除去と sibling 非参照）を 1 テスト 1 性質で固定する。
実装前は strict xfail (`RED: GitHub #1552`)。実装時に marker を外す。
