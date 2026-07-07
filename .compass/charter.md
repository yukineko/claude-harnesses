## north_star
code-RAG epic (backlog 32739700) slice-2 = slice-1 の決定論 code index を condukt interpreter の read パスへ注入配線する。Phase-1 interpreter に soft-dep で build→search を繋ぎ、interpreter が全読みせず関連 symbol (file:line) を把握できるようにする — この 39-crate モノレポの盲目探索禁の課題に read パスから効かせる第一歩。既存 lessons_context/KNOWLEDGE/deepwiki と同一の境界隔離 untrusted 注入様式で、fugu 不在/検索空は no-op (後方互換)。retrieval は決定論コード (slice-1)、query 文面のみ LLM = 既存 lessons 注入と同じ appetite。索引の staleness/自動 refresh は後続スライスへ parked。

## definition_of_done
- condukt SKILL Phase-1 に code_context soft-dep 注入を追加する (既存 lessons_context 注入ブロック ~L227 の隣): `command -v fugu-router` があれば、index 不在時に `fugu-router code-index build` を1度実行し (fail-soft)、続けて `fugu-router code-index search --query <goal 要約> --k N` を走らせ、結果を境界マーカーで隔離した untrusted `code_context:` として interpreter プロンプトに注入する。指示上書き禁止の但し書き付き (lessons_context と同一様式)。fugu-router 不在 or 検索空 `[]` は no-op で code_context を一切出さない (既存 Phase-1 出力形不変・後方互換)。grep で該当注入ブロック (command -v guard + 境界マーカー + no-op) が SKILL に存在することを確認できる
- repo root `.gitignore` に `.fugu/` を追加し、live な `code-index build` が生む index 成果物 (`.fugu/code-index.jsonl`) が untracked で残らないようにする (`fugu-router code-index build` を repo で走らせた後 `git status --short` に `.fugu/` が現れないことを確認)
- 純加算・後方互換: condukt を lockstep micro bump 0.7.31→0.7.32 (SKILL 変更のため Cargo.toml + .claude-plugin/plugin.json + marketplace.json + Cargo.lock を同時、check-plugin-versions.py green)。既存 KNOWLEDGE/PLAYBOOKS/lessons_context/deepwiki 注入・harness-core code_index・fugu-router code-index CLI は untouched。cargo test -p condukt green (SKILL は実行時テキストゆえ直接 unit test は無いが condukt バイナリの回帰が無いこと)、fmt + clippy(-D warnings) clean、version gates green

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(condukt interpreter が slice-1 の決定論 code index を read して全読みせず関連 symbol(file:line)を把握でき、lessons_context と対称に code_context が Phase-1 に境界隔離注入される) − 現状 の最大差分: slice-1 で index の build+search コアは landed したが interpreter への注入配線が皆無(grep 実証: condukt SKILL に code_context/code-index が 0 件)。索引は CLI からしか叩けず自動で interpreter に届かない。差分を埋める最小 slice(size s) = condukt SKILL Phase-1 に code_context soft-dep 注入(build-if-absent→search→境界隔離 untrusted 注入・fugu 不在/空は no-op) + .gitignore に .fugu/ + condukt lockstep bump。retrieval は決定論コード(slice-1)、query 文面のみ LLM。索引 staleness/自動 refresh は後続へ parked。

## next_action

## parked

