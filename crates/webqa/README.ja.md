# webqa

Playwright による Web QA を Claude Code の Stop ゲートとして強制するプラグイン。
v0.1.0 時点で**動作するのは `weaken-check` のみ**。`gate` は未実装（t2 で実装）。

## 現状 (v0.1.0)

| サブコマンド | 状態 |
|---|---|
| `webqa weaken-check [--diff-file PATH]` | 実装済み |
| `webqa gate` (Stop hook) | **未実装 — t2 で実装**。現状は常に `{"decision":"block","reason":"webqa gate not implemented yet"}` を出力する（fail-closed。何も黙って通さない） |

## weaken-check

Playwright spec（`*.spec.*` / `*.test.*`、拡張子 `ts` `js` `tsx` `mjs`）への diff だけを検査し、
テストを「通りやすくする」編集を検出する。diff は `--diff-file` で渡すか、省略時は `git diff --cached`。

| ルール | 検出対象 |
|---|---|
| `skip-added` | `test.skip` / `describe.skip` / `.fixme` の追加 |
| `only-added` | `.only` の追加 |
| `expect-removed` | hunk 内で `expect(` が純減 |
| `wait-timeout-added` | `waitForTimeout(` の追加 |
| `timeout-raised` | `timeout: N` / `setTimeout(N)` の値の引き上げ |
| `matcher-weakened` | `expect` が `toBeTruthy` / `toBeDefined` / `not.toBeNull` 等の弱いマッチャへ置換 |

終了コード（三値）: `0` = 問題なし / `1` = 検出あり（stdout に `パス: ルールID: 詳細` を 1 行ずつ）/
`2` = 判定不能（diff ファイル読めない、空でない入力にパース可能な diff ヘッダが無い、
`git diff --cached` の起動失敗・非 0 終了）。判定不能を 0 に写さない。

## gate（予定仕様 — 未実装）

`webqa.toml` で変更された Web コードと Playwright spec の対応を宣言し、Stop 時に該当 spec を実行する。
結果は三値（pass / fail / undetermined）で、**undetermined（設定不能・実行不能など）は block** に解決する。

## skill: webqa-gen

Playwright spec の生成を支援する skill（予定）。このコミットでは skill 本体は未同梱。
