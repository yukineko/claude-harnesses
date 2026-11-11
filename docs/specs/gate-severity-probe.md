> GitHub #1552 の契約。RED テスト (`scripts/test_gate_severity_probe.py` /
> `scripts/test_gate_severity_probe_b.py`) が実装前に固定し、実装 (`scripts/gate-severity-probe.py`) が満たす。

# gate-severity-probe 仕様

## 概要

`scripts/gate-severity-probe.py` は、pre-commit の各 gate について「壊れたら何が漏れるか」を測る。
ユーザー定義 (2026-11-06) の 3 指標を gate ごとに出す。time-to-detect は対象外。

CLI: `python3 scripts/gate-severity-probe.py --json [--repo PATH] [--gate NAME]... [--commit-layers-only]
[--cargo-target-dir DIR] [--layer-timeout SECS]`

- `--repo PATH` — 対象 (caller) repo。省略時は cwd。読むだけ（`rev-parse`）。
- `--gate NAME` — 繰り返し可。指定した gate だけ測る。未知の名前は exit 2（空結果や clean に見せない）。
  gate 名は `.githooks/pre-commit` が表示するラベル (`launcher-exec-bit` 等) と `test-weakening`（commit-msg）。
- `--commit-layers-only` — pre-push と cargo-test を走らせない。走らせなかった層は `undetermined_layers` に
  入り、他層が止めない違反の `leak` は `"undetermined"`（benign な値に畳まない）。
- `--layer-timeout` — pre-push / cargo-test 1 回の上限秒（既定 1800）。timeout は undetermined。
- 成功時は exit 0 で stdout に JSON のみ。probe 自体が走れない（未知 gate・git repo でない・clone 失敗）ときは
  exit 2 で JSON を出さない。

gate 一覧は clone 側 `.githooks/pre-commit` の `run <scanner> <label> <args>` 行 + `test-weakening`。
今 hook が走らせている gate だけを測る（ユーザー裁定 2026-11-11。hook から外した gate は一覧から消える。
例: `main-tree-guard` は `432f3a40` で撤去済みなので測らない）。
fixture の無い新 gate は `fires: "undetermined"`（理由付き）で現れる。

## 出力

```json
{"gates": [{"gate": "launcher-exec-bit", "fires": true, "caught_by": ["pre-push"], "leak": null}]}
```

| key | 値 | 意味 |
|---|---|---|
| `fires` | `true` / `false` / `"undetermined"` | gate 単体で注入した代表違反を今ブロックするか |
| `caught_by` | 文字列のリスト | その gate **だけ**を無効化したとき、なお違反を止める他層（他の pre-commit gate、`commit-msg`、`pre-push`、`cargo-test`）。無効化した gate 自身は含めない |
| `leak` | `"local-commit"` / `"push"` / `"rollout"` / `"undetermined"` / `null` | `caught_by` が空のとき違反が到達する最遠点。他層が止めるなら `null` |
| `security_relevant` | bool | 人間宣言の一覧 `SECURITY_RELEVANT`（2026-11-06: injectguard, secret-guard）に入るか。推論しない |
| `undetermined_layers` | 文字列のリスト | control で既に止まる・timeout・起動不能・未実行の層。`caught_by` には入れない |
| `layer`, `fixture`, `fires_detail`, `reason`, `layer_notes`, `secs` | — | 補助情報 |

`leak` の決め方（`caught_by` が空のとき）: `fires` か層のどれかが undetermined ⇒ `"undetermined"`。それ以外で
fixture が plugin crate か harness-core の配下（`rollout-plugins.sh` が配布する）⇒ `"rollout"`、他は `"push"`。
probe は常に pre-push まで走らせるので `"local-commit"` は現状出さない（pre-push に届かない層構成のための予約値）。

不変条件:

1. `caught_by` が非空 ⇒ `leak` は `null`。`caught_by` が空 ⇒ `leak` は名前付きの水準（`null` ではない）。
   `leak` はその gate を外したときの到達点なので `fires` に依存しない。
   （ユーザー裁定 2026-11-11 で確定。`test_gate_severity_probe.py` も bf0d8359 でこれに合わせた。）
2. **undetermined は畳まない。** 判定不能 ⇒ `fires == "undetermined"`、他層も止めなければ `leak == "undetermined"`
   （他層が実測で止めたなら、漏れないことは観測済みなので 1. により `null`）。`false` や `null` に潰さない（CLAUDE.md §3）。
   判定不能の例: 違反を注入しない baseline 実行ですでにその gate がブロックする
   （scanner 欠落・crash 等）— ブロックを注入違反に帰属できない。fixture の anchor が消えて作れない場合も同じ。
3. gate ごとに代表 fixture を 1 つ持つ。`launcher-exec-bit` は `crates/<c>/bin/<name>` を index 上 100644 にする。
   plugin crate を触る fixture は version 系 gate 以外で lockstep bump を同梱する（bump-on-change が
   「bump すれば通る」違反を見かけ上捕まえるのを防ぐ）。
各層の測り方: pre-commit gate は scanner を hook と同じ引数で 1 本ずつ走らせる（1 gate を外した hook と等価）。
`commit-msg` は `check-test-weakening.py --pending-msg`。`pre-push` は fixture を commit して clone の
`.githooks/pre-push` をその 1 commit に走らせ、同じ crate dir を触る neutral commit を control にする。
`cargo-test` は fixture が触る crate だけ `cargo test -p`（test-changed-crates.sh と同じ範囲。full suite は
nightly 専用）で、clean HEAD を control にする。crate を触らない fixture はこの層では観測対象が無い。

## 隔離（ユーザー裁定 2026-11-08）

- 注入と実行は **probe 自身が作って消す使い捨て worktree でのみ**行う。caller の作業ツリー・index・HEAD・branch は変えない
  （staged / untracked の in-flight 変更を含め byte 単位で保存）。
- 終了時に自分が作った worktree と管理 dir を残さない。
- caller の他 worktree を読まない。他 worktree の状態は結果に影響しない。唯一の例外は probe 自身が作る使い捨て worktree。
- 変更自身の範囲だけを判定する（whole-repo チェックをしない）。
- 実装: caller の HEAD を temp dir へ `git clone --shared` した私有 clone（自前の .git・index・hooks dir・
  bypass ledger・pass 記録）と、その中の linked worktree 1 つ。caller の common dir には何も書かない。
  clone の `main` は判定対象 commit を指す（test-weakening の merge-base 範囲を fixture だけにするため）。
  caller の staged / untracked 変更は測定対象外（判定するのは commit 済み HEAD）。

## テスト

`scripts/test_gate_severity_probe_b.py` — 契約の各性質（形、fires、caught_by/leak の整合、`--gate` filter と未知 gate、
undetermined の非畳み込み、caller 不変、使い捨て worktree の除去と sibling 非参照）を 1 テスト 1 性質で固定する
（`launcher-exec-bit` だけを使い捨て clone 上で測る）。`scripts/test_gate_severity_probe.py` は実 repo で全 gate・全層を
1 回走らせ、全 gate の網羅・schema・既知発火 gate（bump-on-change / file-growth / fail-open-diff / secret-guard）の
`fires == true`・caller 不変を固定する。
