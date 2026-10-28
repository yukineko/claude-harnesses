# webqa

Playwright による Web QA を Claude Code の Stop ゲートとして強制するプラグイン（v0.1.2）。
`gate`（Stop hook）・`weaken-check`・skill `webqa-gen` がすべて実装・同梱済み。

| サブコマンド | 内容 |
|---|---|
| `webqa gate` (Stop hook) | 実装済み。下記「gate」 |
| `webqa weaken-check [--diff-file PATH]` | 実装済み。下記「weaken-check」 |

## gate

Stop hook。hook の cwd にある `webqa.toml`（書式は `webqa.example.toml`）を読み、変更されたファイル
（`git diff --name-only HEAD` ＝ staged/unstaged/削除、加えて untracked かつ ignore されていないもの）を
`[[map]]` の `sources`（globset glob）に照合する。`sources` に一致するか spec 自身が変更された map が
選ばれ、その `specs` を `<command> --reporter=json <specs...>` で実行する（cwd = root、シェルを経由しない）。
判定は `harness_core::verdict::Verdict`（Clean / Violation / Undetermined）で、Clean 以外は常に block。
パニックは `harness_core::gate::run::run_guarded` で fail-closed（block）に解決する。

| 状況 | 判定 |
|---|---|
| `webqa.toml` が無い | 許可（stderr に `not configured`）。プロジェクトが opt-in していない |
| `webqa.toml` が読めない / パース不能 / `command` 空 / `[[map]]` 無し / `max_attempts = 0` | block (undetermined) |
| git が失敗（repo でない・HEAD 無し 等） | block (undetermined) |
| 変更ファイルがどの map にも該当しない | 許可（runner は起動しない） |
| 該当 map の `specs` が空、または spec ファイルが存在しない | block (undetermined) |
| runner を起動できない / タイムアウト（`timeout_secs`, 既定 540）/ シグナル終了 | block (undetermined) |
| stdout が JSON でない、`stats` が欠落 | block (undetermined) |
| exit 0 なのに `unexpected > 0`、または exit 非 0 なのに `unexpected = 0` | block (undetermined, 不整合) |
| `unexpected > 0`（exit 非 0） | block (violation, `failed`、失敗 spec タイトル付き) |
| `flaky > 0` | block (violation, `flaky`) |
| トップレベル `errors` が非空、または `expected = 0`（何も走っていない） | block (undetermined) |
| 上記のいずれでもない（`expected > 0`, `unexpected = 0`, `flaky = 0`, exit 0） | 許可 |

block が同一 session で `max_attempts`（既定 3）回続いた次の stop は有界に許可するが、
`{"systemMessage": "... NOTHING WAS VERIFIED ..."}` を必ず出す（黙って通さない）。attempt カウンタは
`$WEBQA_STATE_DIR`、無ければ `~/.webqa` に session 別で保存し、`reset_after_secs`（既定 1800）の
無操作で 0 に戻る。session id が違えば別カウンタ。許可 = exit 0 かつ stdout 空、block = exit 0 かつ
`{"decision":"block","reason":...}`（hook プロトコル）。手動実行（stdin が hook JSON でない）は block 時に exit 1。

注意: `webqa.toml` の `command` はプロジェクトのファイルが指定するコマンドをそのまま実行する。
donegate のような trust 機構は未実装。

## weaken-check

Playwright spec への diff だけを検査し、テストを「通りやすくする」編集を検出する。対象は
`*.spec.*` / `*.test.*`（拡張子 `ts` `js` `tsx` `jsx` `mjs` `mts` `cjs` `cts`）で、旧パス・新パスのどちらかが
spec 名なら検査する（spec から非 spec 名へのリネームで回避できない）。diff は `--diff-file` か、省略時は
`git diff --cached`。

| ルール | 検出対象 |
|---|---|
| `skip-added` | `test.skip` / `describe.skip` / `.fixme` の追加 |
| `only-added` | `.only` の追加 |
| `expect-removed` | hunk 内で `expect(` が純減 |
| `wait-timeout-added` | `waitForTimeout(` の追加 |
| `timeout-raised` | `timeout: N` / `setTimeout(N)` の引き上げ。**ファイル単位**（hunk をまたいで比較）。`60 * 1000` のような整数積は評価する。削除された timeout が無くても、新規追加が 30000 ms（Playwright 既定）を超えれば検出 |
| `matcher-weakened` | `expect` が `toBeTruthy` / `toBeDefined` / `not.toBeNull` 等の弱いマッチャへ置換 |

終了コード（三値）: `0` = 問題なし / `1` = 検出あり（stdout に `パス: ルールID: 詳細` を 1 行ずつ）/
`2` = 判定不能（diff ファイルが読めない、空でない入力にパース可能な diff ヘッダが無い、
`git diff --cached` の起動失敗・非 0 終了、spec の binary diff、リテラル timeout を評価不能な式
（識別子など）で置換した場合）。判定不能を 0 に写さない。

## skill: webqa-gen

`skills/webqa-gen/SKILL.md` を同梱。仕様文または URL から、ブラウザ探索 → シナリオ列挙 →
test-first な spec 生成 → RED 観測（`tdd red`）→ 採用までを回す。
