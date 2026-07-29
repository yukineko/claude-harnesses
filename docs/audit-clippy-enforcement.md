# 監査: clippy deny lint はどこで強制されているか

測定点: `6302d166`（作業ツリー clean）／測定日: 2026-07-29
対象 backlog: `7ecf3797`（local ゲートでの強制）／派生: `b19aad51`（被覆の拡大）

<!-- doc-claim-exempt: 本文書は測定点 6302d166 における歴史的スナップショットである。逐語引用と行番号はその時点のもので、後続の変更（特に .githooks/pre-commit への配線）で必ず drift する。renumber ではなく snapshot として凍結する運用は docs/autoflow-verdict-audit.md と同じ。 -->

## 要旨

このリポジトリは 3 つの clippy deny lint（`unwrap_used` / `expect_used` / `panic`）を
`[workspace.lints.clippy]` に宣言している。しかし **宣言と強制は別物**であり、監査の結論は次の 2 点:

1. **commit を止める点が存在しない。** clippy を走らせる local な仕組みは donegate ただ一つで、
   それは **Stop hook であって git hook ではない**。`git commit` を一切 intercept しない。
2. **宣言そのものが 42 crate 中 8 crate にしか掛かっていない。** 判定を持つ 4 crate を含む
   34 crate は、どの経路でも `unwrap` / `expect` を検査されない。

前者が `7ecf3797`、後者が `b19aad51` の対象である。

## 1. 宣言の実態

`Cargo.toml:68-87` が 3 つの lint を deny 宣言する:

```toml
[workspace.lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
...
panic = "deny"
```

**workspace lints を opt-in している crate は 8 個**（`[lints]` セクションに `workspace = true` を持つもの）:
blastguard / fetchguard / mutategate / overwatch / propguard / specguard / stuckguard / taintguard。

`Cargo.toml:71-82` のコメントは「15 個の手貼り `#![deny(clippy::panic)]` のうち 10 個を集約し
5 個を残した」と書く。**この記述は測定点で正確に成立している**（検証済み）。残る 5 個:

| ファイル | 行 |
|---|---|
| `crates/donegate/src/main.rs` | 9 |
| `crates/schemaguard/src/main.rs` | 3 |
| `crates/schemaguard/src/lib.rs` | 6 |
| `crates/reviewgate/src/main.rs` | 10 |
| `crates/budgetguard/src/main.rs` | 6 |

散文が実挙動と一致している数少ない例なので、記録として残す（CLAUDE.md 第4節）。

### 被覆の穴（`b19aad51`）

crate 総数は 42。opt-in 済みは 8。**残り 34 は `unwrap_used` / `expect_used` の強制がゼロ**。
特に重大なのは、上表の 4 crate（budgetguard / donegate / reviewgate / schemaguard）が
**判定を持つゲートでありながら** `[lints]` セクションを一切持たないことである。
手貼りの `#![deny(clippy::panic)]` が禁じるのは**明示的な `panic!` マクロだけ**で、
`.unwrap()` / `.expect()` は clippy の既定 allow のまま通る。

**判定を持つコードにおける `unwrap` は判定不能そのもの**であり、CLAUDE.md 第3節が名指しする対象である。
さらに `harness-core` も未被覆で、これは全プラグインバイナリに静的リンクされる共有ライブラリなので
影響範囲が最大になる。

## 2. 強制の実態 — 走る場所は 1 箇所だけ

`cargo clippy` を実際に起動する local な箇所は `donegate.toml:35-36` のみ:

```toml
[[check]]
name = "clippy"
cmd = '. "$HOME/.cargo/env" 2>/dev/null; cargo clippy --workspace --all-targets -- -D warnings'
when_changed = ["**/*.rs", "Cargo.toml", "Cargo.lock"]
```

`.githooks/pre-commit` と `.githooks/pre-push` は **cargo を一切起動しない**（両ファイルへの
`clippy` grep は空）。`CONTRIBUTING.md` の clippy 行は**人間向けの手動チェックリスト**であり
自動化されていない。CI は存在しない（`.github/` 不在。CLAUDE.md 第7節の GHA 全面禁止による）。

## 3. 構造的な欠落 — Stop hook は commit を止めない

**これが本項目の核心である。** donegate は `crates/donegate/src/main.rs:6` が自ら述べるとおり
Stop hook であり、`git commit` を intercept しない。したがって:

- agent が turn の途中で `git commit` を実行すれば、clippy 違反があっても **無条件に成功する**。
- donegate にできるのは、その **後**の turn 終了を止めることだけ。
- しかもその阻止は `donegate.toml:23` の `max_attempts = 3` で上限がある。
  `main.rs:203-219` は上限超過時に **exit 0 で stop を許可**する。違反はツリーに残り、
  既に作られた commit はそのまま残る。

**完全に trust され正しく設定された donegate であっても、悪い commit が作られること自体は防げない。**
防げるのは「その turn を終えること」だけで、それも有限回で諦める。

## 4. 沈黙する迂回路

判定不能や無効化が **hook モードでは無言**である点が、この欠落を見えにくくしている:

| 迂回路 | 根拠 | hook モードでの出力 |
|---|---|---|
| untrusted clone | `config.rs:105-117` が project config を無視し、`checks` が空になる | 無言（`main.rs:154-166` の "nothing to do" は interactive 時のみ） |
| `DONEGATE_DISABLE` | `config.rs:176-180` → `main.rs:148-155` | 完全に無言 |
| `.donegate-skip` | `gate/run.rs:150-162` が読んで削除、`main.rs:170-181` が exit 0 | 1 行出力 |
| `max_attempts` 超過 | `main.rs:203-219` | 1 行出力して許可 |

`donegate.toml:19-20` は「`donegate trust` を打たないとこのファイルは silently does nothing」と
明記しており、**この記述はソースと一致している**（`config.rs:105-117` で検証）。
つまり **fresh clone では clippy 強制が既定で存在しない**。

なお timeout は fail-closed（`gate/runner.rs` が `passed = false` にする）であり、迂回路ではない。

## 5. 違反が commit に到達する経路（確定リスト）

以下はすべてソースで検証済み。推測は末尾 1 件のみで、その旨を明記する。

1. **budgetguard / donegate / reviewgate / schemaguard の `.unwrap()` は、どの invocation でも
   永久に検出されない**（opt-in が無いため。`--workspace` で回しても同じ）。
2. 他の 30 crate（`harness-core` 含む）も同様に検出されない。
3. **fresh / untrusted clone では、opt-in 済み crate の違反ですら検査されない**。
4. **trust 済みでも `git commit` は素通りする**（第3節）。
5. 3 回連続で止めた後は donegate が諦めて許可する。
6. `DONEGATE_DISABLE=1` で無言に全無効化。
7. `.donegate-skip` で当該 stop をスキップ。
8. `git commit --no-verify` は `.githooks/pre-commit` を迂回する。今日の clippy に対しては
   moot（pre-commit が clippy を走らせていないため）だが、**新設するゲートには効く**。
9. **確定**（当初は推測だったが本監査で実測した）: Claude Code のセッション外（人間や script が
   直接 `git commit`）では donegate は一切関与しない。donegate は plugin 機構で配線されており、
   `~/.claude/plugins/cache/yukineko/donegate/0.1.21/hooks/hooks.json` が登録するのは
   **`Stop` フック 1 本のみ**である:

   ```json
   { "hooks": { "Stop": [ { "hooks": [
       { "type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/bin/donegate gate", "timeout": 600 }
   ] } ] } }
   ```

   `PreToolUse` も git hook も登録していない。したがって donegate が発火しうるのは
   **Claude Code のセッションが turn を終えようとする瞬間だけ**であり、
   `git commit` という操作そのものとは完全に無関係である。第3節の結論を裏付ける一次証拠。

## 6. 新ゲートの設計入力

### `run()` ヘルパの契約（`.githooks/pre-commit:112-138`）

新ゲートはこの形に乗るだけで契約を継承できる:

- スキャナのファイルが無い → `rc=1`（**判定不能を block に倒す**）
- exit 2 → `UNDETERMINED` と表示して block
- その他の非0 → block
- exit 0 → 何もしない

`rc` は **scanner ごとにリセットされない**（`rc=0` は一度だけ設定され、全 `run` 呼び出しで累積する）。

### 差分の取得元 — 選定元と検査対象を一致させる

`.githooks/pre-commit` 自身に `git diff` は無い（`git write-tree` は index の証跡用で、
どの scanner を走らせるかの scoping には使っていない。全 scanner が無条件に走る）。

**clippy はディスク上の作業ツリーをコンパイルする**ので、選定元も作業ツリーでなければならない。
`scripts/test-changed-crates.sh:56` が既に正しくこれを行っている:

```sh
diff_out="$(git diff --name-only HEAD -- 2>/dev/null)" || diff_rc=$?
```

さらに同スクリプトは `git ls-files --others --exclude-standard`（未追跡）と union を取る。
**未追跡ファイルを含めないと、新規追加された `.rs` の違反が丸ごと漏れる。**
`--cached` を使うと「違反を未 stage で残し clean な変更だけを stage する」ことでゲートが素通りする
（不変条件: 差分の取得元 ＝ 検査する内容）。

### crate 解決

`scripts/test-changed-crates.sh:83-99` が `crates/<dir>/` を抽出し、各 dir の
`Cargo.toml` の `[package]` セクションから**実際の package 名**を解決する
（`[[bin]]` からではない。ディレクトリ名と package 名は一致するとは限らない）。この模型を再利用する。

### 既存のフックテストは何にも配線されていない

`scripts/test_precommit_hook.py` / `test_prepush_hook.py` / `test_git_hook_coverage.py` の 3 本は
いずれも **どこからも実行されていない**（Makefile 無し、donegate.toml 未登録、hook 未参照）。
backlog `c3a98510` が追跡中。**新しいゲートを配線せずに置くことは、この欠陥を新しく作ることになる。**

環境隔離の正典は `scripts/test_precommit_hook.py:90-155` の `HookHarness`:
mkdtemp した使い捨て repo、stub scanner、必要なバイナリだけを symlink した PATH、完全に制御された env。
`cargo` 不在を偽装するにはこの PATH scoping を使う。

#### 実測: `test_precommit_hook.py` は既に赤い（配線タスクへの制約）

測定点 `6302d166`（クリーンな main）で `python3 scripts/test_precommit_hook.py` を実行すると
**`FAILED (failures=2)`** になる。原因は、このテストが pre-commit の scanner 一覧を
**厳密な inventory として assert** しており、その一覧が既に古いこと。

**実測（数え直した値）**: `scripts/test_precommit_hook.py:53-60` の `EXPECTED_SCANNERS` は
**6 本**、`.githooks/pre-commit` の `run ` 行は **10 本**。したがって乖離は **4 本**である:

- `check-claudemd-claims.py`
- `check-hardcoded-secret.py`
- `check-raw-io-ratchet.py`
- `check-worktree-isolation.py`

> **訂正の記録**: 本節の初版は「3 本」と書き、続けて「新しい scanner を足すと 4 本になる」と
> 予測していた。どちらも誤り。テスト失敗出力の diff 断片を目視で数えて `check-claudemd-claims.py`
> を落としたのが原因で、**乖離は追加前から既に 4 本**である。独立検証者が
> `EXPECTED_SCANNERS` と `run ` 行を直接数えて指摘した。**目視で数えた値を測定値として書かない**
> というのが、この監査自身から出た教訓である（CLAUDE.md 第2節: 判断は予測であって事実ではない）。

つまり **scanner を 4 本追加する間、誰もこのテストを更新しなかった**。
そして**どこからも実行されていない**ため、誰も気づかなかった。これは `c3a98510`
（フックテストが何にも配線されていない）が指す欠陥の**実害が観測された初めての例**である。

**配線タスクへの制約**: 新しい scanner を 1 行足すと乖離は **5 本**になる。
配線と同じコミットで `test_precommit_hook.py` の期待一覧を実態に合わせて更新すること。
これは「テストを通すために緩める」ではない — 一覧が実態を追跡することがこのテストの目的であり、
追跡させることが修正である。ただし**既に赤かったという事実を隠さない**こと（測定値を上に残す理由）。

## 7. 本項目でやらないこと

- **4 crate への `[lints] workspace = true` 追加はスコープ外。** `crates/` を触ると version の
  lockstep bump と rollout が必要になり、この一手を超える。`b19aad51` へ分離した。
- `donegate.toml` の workspace 全体スキャンの scoping は別項目（`15eb3f94`）。
- CI 化は**不採用**。CLAUDE.md 第7節により、block/allow の権限は local に保持する。
