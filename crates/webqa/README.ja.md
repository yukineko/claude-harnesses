# webqa

Playwright による Web QA を Claude Code の Stop ゲートとして強制するプラグイン（v0.1.3）。
`gate`（Stop hook）・`trust`・`weaken-check`・skill `webqa-gen` がすべて実装・同梱済み。

| サブコマンド | 内容 |
|---|---|
| `webqa gate` (Stop hook) | 実装済み。下記「gate」 |
| `webqa trust` | 実装済み。下記「trust」 |
| `webqa weaken-check [--diff-file PATH]` | 実装済み。下記「weaken-check」 |

## gate

Stop hook。project root（hook の cwd の `git rev-parse --show-toplevel`。repository 外なら cwd 自身）に
ある `webqa.toml`（書式は `webqa.example.toml`）を読み、変更されたファイル
（`git diff --name-only HEAD` ＝ staged/unstaged/削除、加えて untracked かつ ignore されていないもの）を
`[[map]]` の `sources`（globset glob）に照合する。`sources` に一致するか spec 自身が変更された map が
選ばれ、その `specs` を `<command> --reporter=json <specs...>` で実行する（cwd = root、シェルを経由しない）。
判定は `harness_core::verdict::Verdict`（Clean / Violation / Undetermined）で、Clean 以外は常に block。
パニックは `harness_core::gate::run::run_guarded` で fail-closed（block）に解決する。

| 状況 | 判定 |
|---|---|
| `webqa.toml` が無い（repository 外の cwd を含む） | 許可（stderr に `not configured`）。プロジェクトが opt-in していない |
| `webqa.toml` が trust されていない / trust 後に内容が変わった / trust store が読めない | block（reason に `untrusted`）。runner は起動しない |
| `webqa.toml` が読めない / パース不能 / `command` 空 / `[[map]]` 無し / `max_attempts = 0` / `timeout_secs > 540` | block (undetermined) |
| repository 外の cwd に `webqa.toml` がある、`git rev-parse --show-toplevel` が「repository でない」以外で失敗、HEAD 無し 等の git 失敗 | block (undetermined) |
| 変更ファイルがどの map にも該当しない | 許可（runner は起動しない） |
| 該当 map の `specs` が空、または spec ファイルが存在しない | block (undetermined) |
| runner を起動できない / タイムアウト（`timeout_secs`, 既定 540）/ シグナル終了 | block (undetermined) |
| stdout が JSON でない、`stats` が欠落 | block (undetermined) |
| exit 0 なのに `unexpected > 0`、または exit 非 0 なのに `unexpected = 0` | block (undetermined, 不整合) |
| `unexpected > 0`（exit 非 0） | block (violation, `failed`、失敗 spec タイトル付き) |
| `flaky > 0` | block (violation, `flaky`) |
| トップレベル `errors` が非空、または `expected = 0`（何も走っていない） | block (undetermined) |
| 選択した spec のどれかが report の `suites[].file`（ネストした suite も再帰。`/` 境界の suffix 一致）に無い、または skip 以外の test が 1 件も無い | block (undetermined、欠けた spec 名付き) |
| 上記のいずれでもない（`expected > 0`, `unexpected = 0`, `flaky = 0`, exit 0、全 spec が結果に出現） | 許可 |

block が同一 session で `max_attempts`（既定 3）回続いた次の stop は有界に許可するが、
`{"systemMessage": "... NOTHING WAS VERIFIED ..."}` を必ず出す（黙って通さない）。attempt カウンタは
`$WEBQA_STATE_DIR`、無ければ `~/.webqa` に session 別で保存し、`reset_after_secs`（既定 1800）の
無操作で 0 に戻る。session id が違えば別カウンタ。許可 = exit 0 かつ stdout 空、block = exit 0 かつ
`{"decision":"block","reason":...}`（hook プロトコル）。手動実行（stdin が hook JSON でない）は block 時に exit 1。

`timeout_secs` の上限は 540（hooks.json の Stop hook timeout 600 秒より下。超えると runner の
タイムアウトを観測できないため undetermined）。下限は従来どおり 1 秒に切り上げ。

## trust

`webqa.toml` の `command` はプロジェクトのファイルが指定するコマンドをそのまま実行するため、
ユーザーが承認した内容だけを実行する。`webqa trust`（cwd から gate と同じ方法で root を解決）が
`<root>/webqa.toml` の sha256 を canonical な root をキーに `$WEBQA_STATE_DIR`（無ければ `~/.webqa`）の
`trust.json` へ記録する。gate は現在の内容の hash が記録と一致したときだけ runner を起動する。
記録が無い・hash が変わった・store が読めない／パース不能は、すべて untrusted として block
（trusted には倒さない）。config を変えたら `webqa trust` を再実行する。壊れた store は上書きせず
`webqa trust` 自体が失敗する。

## weaken-check

Playwright spec への diff だけを検査し、テストを「通りやすくする」編集を検出する。対象は
`*.spec.*` / `*.test.*`（拡張子 `ts` `js` `tsx` `jsx` `mjs` `mts` `cjs` `cts`）で、旧パス・新パスのどちらかが
spec 名なら検査する（spec から非 spec 名へのリネームで回避できない）。`---`/`+++` ヘッダのパスは git が
付ける末尾 TAB の手前までを読み、C-quote されたパス（`"a/x\"y.spec.ts"`）はデコードする（不正な quote は判定不能）。diff は `--diff-file` か、省略時は
`git diff --cached`。

| ルール | 検出対象 |
|---|---|
| `skip-added` | `test.skip` / `describe.skip` / `.fixme` の追加 |
| `slow-added` | `test.slow()` の追加（timeout を 3 倍にする） |
| `only-added` | `.only` の追加 |
| `expect-removed` | hunk 内で `expect(` が純減 |
| `wait-timeout-added` | `waitForTimeout(` の追加 |
| `timeout-raised` | `timeout: N` / `setTimeout(N)` の引き上げ。**ファイル単位**（hunk をまたいで比較）。`60_000`・`60 * 1000`・`(5 * 60_000)` のような整数（積・括弧）は評価する。`0` は timeout 無効化なので無限大として扱う（正の値を `0` に置換すれば検出）。削除された timeout が無くても、新規追加が 30000 ms（Playwright 既定）を超えれば検出 |
| `matcher-weakened` | `expect` が `toBeTruthy` / `toBeDefined` / `not.toBeNull` 等の弱いマッチャへ置換 |

終了コード（三値）: `0` = 問題なし / `1` = 検出あり（stdout に `パス: ルールID: 詳細` を 1 行ずつ）/
`2` = 判定不能（diff ファイルが読めない、空でない入力にパース可能な diff ヘッダが無い、
`git diff --cached` の起動失敗・非 0 終了、spec の binary diff、リテラル timeout を評価不能な式
（識別子・括弧式・演算子始まり・空など。`setTimeout(` のコールバックだけは timeout とみなさない）で置換した場合）。判定不能を 0 に写さない。

## skill: webqa-gen

`skills/webqa-gen/SKILL.md` を同梱。仕様文または URL から、ブラウザ探索 → シナリオ列挙 →
test-first な spec 生成 → RED 観測（`tdd red`）→ 採用までを回す。
