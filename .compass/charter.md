## north_star
code-RAG epic (backlog 32739700) を subscription-native に再構成した第1スライス = 決定論的 symbol/lexical コード索引 (embedding/API 非依存) の build+search コアを1本通す。動機はこの 39-crate モノレポ自身の『全読みせず (CLAUDE.md 盲目探索の禁) 右サイズにナビゲートしたい』課題。determinism-in-code appetite に沿い、抽出・索引・順位付けは全て決定論コード (AI 非依存・lexical のみ)、embedding も外部 API も使わない。lessons/retrieval store と同じ pure-core (harness-core) + CLI (retrieval-surface plugin) + fail-soft の家系で純加算に足す。SKILL 注入への配線は本スライス対象外 (slice-2 へ parked) — まず観測可能な決定論 index コアを landed させる。

## definition_of_done
- 決定論的 symbol 抽出 pure fn を harness-core に追加する: Rust ソース1ファイル (contents + path) を受け取り、宣言シンボル (pub fn/fn/struct/enum/trait/impl/mod/const/static/type/macro_rules) を行走査で抽出して Vec<Symbol{name, kind, file, line, signature}> を返す (AI 非依存・決定論・same-input→same-output)。非対象/空行はスキップし never panic。unit test で seeded source から期待シンボル (name/kind/1-indexed line) がちょうど出ることを実証する
- 決定論的 index store + lexical top-K search を追加する: シンボル列を rebuild 可能な JSONL index (1 シンボル 1 行) に永続化でき、search(&[Symbol], query, k) -> Vec<(Symbol, score)> が token-overlap で決定論順位付けする (lessons::search と同じ scoring 家系・同点は安定順)。load は fail-soft (missing index → 空 Vec・corrupt 行 skip・never panic)。unit test で seeded symbols から検索順位が期待どおり・missing index が空を返すことを実証し cargo test green
- CLI surface を retrieval-surface plugin (fugu-router 優先) に足す: `code-index build [--root <path>]` が git 追跡ファイルを走査しシンボル抽出→index JSONL を書き、`code-index search --query <q> [--k N]` が load→search→検索結果 JSON を stdout に emit する。空ストア/索引不在でも fail-soft (`[]` を出し exit 0・非0終了しない)。build→search 往復を integration test で実証 (index したシンボルが検索でヒットする)
- 純加算で既存挙動を温存する: fmt と clippy(-D warnings) clean、触った crate の cargo test green、CLI-host plugin を lockstep micro bump (Cargo.toml + plugin.json + marketplace.json + Cargo.lock)。harness-core は build-time lib なので bump 不要。既存の lessons/retrieval store・fugu-router 既存サブコマンド (lessons/procedures 等)・condukt SKILL 注入は untouched。SKILL 注入経路への配線は本スライス対象外 (slice-2 へ parked)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(大規模 repo を全読みせず決定論的 symbol index で右サイズにナビゲートできる — シンボル抽出→索引→lexical top-K 検索が AI 非依存で動き build→search 往復が観測可能) − 現状 の最大差分: コード索引が皆無(grep 実証: crates/ に code-index/Symbol 抽出/code-RAG シンボル ゼロ)。ナビは grep/Read の手作業のみで 39-crate を毎回 GLOSSARY 頼りに当たりをつけるしかない。差分を埋める最小 slice = (1)harness-core に決定論 symbol 抽出 pure fn + Symbol struct + (2)JSONL index store + lexical top-K search(fail-soft) + (3)fugu-router に code-index build/search CLI。抽出/索引/順位は純関数=AI 非依存・embedding/API 不使用(subscription-native)。size m。SKILL 注入配線は slice-2 へ parked。

## next_action

## parked

