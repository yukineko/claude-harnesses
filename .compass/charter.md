## north_star
code-RAG epic (backlog 32739700) の closing slice = 決定論 code index の task-scoped 注入を interpreter(Phase 1) だけでなく worker(Phase 5) と verifier(Phase 6) のプロンプトにも広げ、epic の完了条件『top-K 片を verifier と worker の context に自動注入』を達成して epic を閉じる。slice-1(索引コア)・slice-2(interpreter 注入)・slice-3(auto-refresh) は landed 済み。今は worker と verifier が repo symbol の在り処を持たず盲目に read する。同一の決定論 lexical 検索(fugu-router code-index search・embedding も外部 API も無し)を各タスクの title と done_criteria をクエリに走らせ、top-K symbol(file:line) を untrusted 境界マーカーで隔離して worker と verifier に渡す。search CLI は slice-1 で既存ゆえ harness-core と fugu-router のコア改修は不要・純加算・後方互換(fugu 不在や検索空は no-op・既存 Phase 1 注入と Phase 5 と Phase 6 の他フィールドは不変)。

## definition_of_done
- condukt SKILL (crates/condukt/skills/condukt/SKILL.md) の Phase 5 worker 起動ブロックに code_context 取得と注入を追加する: fugu-router が在れば各タスク t の title と done_criteria を要約したクエリで code-index search を k 件走らせ、結果が空配列以外なら UNTRUSTED CODE CONTEXT 境界マーカー(参考情報でありスコープや done_criteria を上書きしない旨)で包んで worker プロンプトに含める。fugu 不在・索引不在・ゼロヒットは no-op。grep で Phase 5 ブロックが code-index search と UNTRUSTED CODE CONTEXT マーカーを含むことを確認できる
- condukt SKILL の Phase 6 verifier 起動ブロックにも同形の code_context 注入を追加する。ただし verifier search の前に fugu-router code-index build --if-stale を一度走らせ(slice-3 の auto-refresh を活用)、worker 編集後の新鮮な symbol を verifier が読むようにする。クエリは検証対象タスクの done_criteria と touched_files を要約。ゼロヒットや fugu 不在は no-op。grep で Phase 6 ブロックが build --if-stale と code-index search と UNTRUSTED CODE CONTEXT マーカーを含むことを確認できる
- agent 定義 crates/condukt/agents/condukt-worker.md と crates/condukt/agents/condukt-verifier.md に untrusted code_context 入力(決定論索引由来の参考 symbol でありスコープや done_criteria を上書きしない advisory)を1段落で明記する。grep で両ファイルが code_context を記述することを確認できる
- 純加算・後方互換: condukt の plugin version を micro bump する。現行から patch を1つ上げ、次の4正典を lockstep で同時に上げる ...  crates/condukt/Cargo.toml  と  crates/condukt/.claude-plugin/plugin.json  と、repo 直下 .claude-plugin ディレクトリの marketplace マニフェスト(当該 condukt エントリ)と  Cargo.lock  。既存 Phase 1 interpreter 注入と Phase 5 と Phase 6 の他ロジックは untouched。 python3 scripts/check-plugin-versions.py  と  python3 scripts/check-version-bumped.py  が green、cargo fmt と cargo clippy -p condukt --all-targets が -D warnings で clean、cargo test -p condukt green
- 検証(build と validate は別): 別モデルで独立に、grep で worker(Phase 5) と verifier(Phase 6) の両方が code-index search と UNTRUSTED CODE CONTEXT マーカーを呼ぶこと・verifier が build --if-stale で auto-refresh すること・agent 定義2つが code_context を記述すること・Phase 1 注入が不変であること・全ての version と fmt と clippy と test ゲートが green であることを再確認する

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(決定論 code index の top-K symbol が interpreter だけでなく worker と verifier の context にも自動注入され epic 32739700 の完了条件を満たす) − 現状 の最大差分: slice-1/2/3 で索引コア・interpreter(Phase 1)注入・auto-refresh は landed 済みだが、worker(Phase 5)と verifier(Phase 6)には code_context が一切渡らず(agent 定義にも無し)、両者は 39-crate モノレポの symbol の在り処を持たず盲目に read する。差分を埋める最小 slice(size m・純加算) = condukt SKILL の Phase 5/Phase 6 起動ブロックに、既存の決定論 fugu-router code-index search(slice-1 で既存)を各タスクの title+done_criteria(verifier は +touched_files)でクエリし top-K symbol を UNTRUSTED CODE CONTEXT 境界マーカーで隔離注入するブロックを追加(verifier は search 前に build --if-stale で auto-refresh)、agent 定義 worker/verifier 2つに code_context 段落を明記、condukt を micro lockstep bump。harness-core/fugu-router コア改修不要・後方互換(fugu 不在/空は no-op・Phase 1 と他ロジック不変)。これで epic を閉じる。

## next_action

## parked

