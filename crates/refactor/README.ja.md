# refactor

肥大化したソースファイルを返済する skill のみの Claude Code プラグイン（Rust クレートも hook も
持たない）。1 コミット 1 モジュールの **純粋な移動（pure move）** で分割し、各ステップが挙動を
変えていないことを stdlib のみの証明ツールで示す。blastguard の `detect.rs`（17742 → 2402 行）の
分割で実際に使った手順である。ゲートではない（何もブロックしない）。`/refactor` skill が証明を
実行し、exit 0 以外では止まる。

- Skill: `/refactor <file>` — `skills/refactor/SKILL.md`
- ツール: `skills/refactor/scripts/refactor_proof.py`（python3・stdlib のみ）

## 証明ツール

全サブコマンド共通の終了コード: **0 = 等しいと証明**、**1 = 差分あり**（差分を表示）、
**2 = 判定不能**（git 失敗・引数不正・正規化後に空・テスト出力がパース不能）。判定不能は決して 0 にならない。

### 正規化 N（Rust、行単位、両側に適用）

空白を strip し、空行と `//!` 行を捨てる。`use ...;`（`;` までの複数行 use を含む）と `mod name;`
を、任意の `pub` / `pub(crate)` / `pub(super)` / `pub(self)` / `pub(in path)` 接頭辞と直上の属性行
（例 `#[cfg(test)]`）ごと捨てる。残る各行の先頭の可視性修飾を 1 つ除く。N は行ベースであり Rust
パーサではない（複数行文字列リテラル内の `use`/`mod` 行も捨てる）。だからテスト集合比較を併用する。

### `multiset --repo R --base B --head H -- <pathspec>...`

git pathspec に一致する全ファイルの N 行の多重集合が rev B と rev H で等しい（追加・削除・編集・
複製が無い）。ファイル数・行数の要約を出し、exit 1 では base のみ（`-`）/ head のみ（`+`）の行を出す。
どちらかの rev で一致ファイルが無ければ exit 2。行ベースなので rustfmt の折り返し変更は差分として出る。

### `ordered --repo R --commit C --from <path> --to <path>`

コミット C が `<from>` から削除した N 行（順序どおり）が、C が新規作成した `<to>` の N 行と等しく、
かつ `<from>` が use/mod 行以外を得ていない（同じ位置で N を法として自分自身に置き換わった行＝可視性
変更は「得た」に数えない）。空白を無視し括弧・カンマを独立トークンにしたトークン列で比較する。対応する
閉じ括弧で始まる行の直前の行末カンマ（リテラル・コメント内でないコード上のもの）は、意味を持ち得ない
場合だけ捨てる: `]` / `}` の前は常に、`)` の前は要素が 2 個以上のときか `(` がキーワードでない識別子・
マクロ名の直後（呼び出し・fn シグネチャ）のときだけ。折り返した 1 要素タプル `(\n x,\n)` はカンマを
残すので `(x)` への変更は exit 1。括弧追跡が確実に読めない場合もカンマを残す（exit 1 側へ倒す）。
これで rustfmt の折り返しは等しく比較される。`mod NAME { BODY }` を
削除して BODY だけの `NAME.rs`（または `NAME/mod.rs`）を作った場合も等しい。C に `<to>` が無い・C 以前に
既に在る・C 以前に `<from>` が無い・C に親が無い場合は exit 2。

### `tests record --repo R --out DIR -- <cmd...>` / `tests compare --repo R --baseline DIR [--allow-removed FILE] -- <cmd...>`

libtest 形式のコマンドを R で実行し、**stdout のみ**（stderr は分離。結果行へ割り込ませない）から
`test NAME ... ok|FAILED|ignored` を読み、成功名・失敗名をソートして `DIR/pass.txt` / `DIR/fail.txt` に
記録する。同じパスが複数のテストバイナリに在りうるので名前は多重集合で扱う。`compare` は失敗名が
baseline と等しく、成功名が baseline から `--allow-removed` 記載名を除いたものと等しいときだけ 0、
それ以外は 1（欠けた名前・新しい名前を列挙）。コマンドの終了コードは判定に使わない（既知の失敗を含む
baseline も非 0 で終わるため）。`test result:` 行が無い、または ok / FAILED / ignored 以外で終わる
`test X ...` 行があれば exit 2。

テスト: `python3 -m unittest scripts/test_refactor_proof.py`（リポジトリ直下で実行）。
