## north_star
review-queueのfindingは fix commitが着地した時点で機構的に解消できるようにする(手動 record-disposition 頼みで放置されstaleになる再発を防ぐ)

## definition_of_done
- commit message からfinding-id(例 CA-<crate>-<NNN>)を検出し、該当findingを自動でrecord-disposition(confirmed)する決定論コマンド(reconcileの類)がoverwatchに実装され、赤緑テストで固定される
- そのreconcileが人間の記憶に依存しない既存の自動化経路(continuous-audit round実行時 / pre-push hook / SessionStart等)に配線され、fail-soft(バイナリ不在・エラーでもターンを壊さない)であることがテストで固定される
- review-metrics か audit-metrics に、fix commitは存在するがdispositionされていないfinding件数(stale backlog再発の早期警告)を出す1コマンドが追加され、回帰テストで数値が固定される

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
2026-07-17の/flowセッションで発覚: review-queueの18件のai-finding(CA-backlog-001/002, CA-blastguard-004..010, CA-propguard-003..006, CA-specguard-003..005, CA-overwatch-003/004)は全てfix commit(aa0a471, e16e270, e960608, 0f8709f, 57214c2, 56a6b01, 0295058, 170d7e7他)が既に着地しcargo test全pass済みだったにも関わらず、誰もoverwatch record-dispositionを手動実行しなかったためreview-queueに『未解決』として残り続けていた。今回はrecord-disposition+compact-findingsで手動解消したが、これは対症療法(症状治療)であり、同じ放置が次のContinuous-Audit round後にも再発する。機構的なgapは: fix commitとfindingのdispositionを繋ぐ自動リンクが存在しないこと。最小right-sizeな一手は、commit message中のCA-IDを検出しrecord-dispositionを自動発火する決定論コマンドをoverwatchに追加し、既存の自動化経路(pre-push hook等)に配線すること。

## next_action

## parked

