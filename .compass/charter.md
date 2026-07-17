## north_star
PDO並列実行の安全性を仕上げる: 本セッションで判明した cross-session排他機構(backlog lock/condukt claim/overwatch lease/hypothesis store lock/discovery store)の残存ギャップを閉じ、機能衝突drift・token浪費をさらに削減する

## definition_of_done
- /flow のStep 3-4ループが backlog lock heartbeat を定期実行するよう配線され、30分超のセッションでもロックが自動失効(reap)されないことをテストか観察で確認できる
- condukt::schedule.rs の衝突検知(Serial/Gated/shared-glob判定)に偽陽性/偽陰性が無いか調査し、結果(問題なし or 修正commit)が記録される
- fugu-router のモデル割当が実際にtoken/コストを削減しているかを実測し、hypothesis(186f7a66等)のvalidate/rejectか計測レポートとして記録される

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
PDO並列安全性のうち、backlog lockのheartbeat機構は本セッションのfork作業でcommit 0f7edbfとして実装済みだが、/flowスキルのStep 3-4ループは condukt state heartbeat のみを呼び、新設された backlog lock heartbeat を呼んでいない。そのため30分超の長時間セッションではロックが自動失効(reap)されうるギャップが残る。最小right-sizeな一手は、/flowスキル(Step 3-4)に backlog lock heartbeat --session-id <SESSION_ID> の呼び出しを追加し(fail-soft、condukt state heartbeatと同じ呼び出し規約)、長時間ループでロックが維持されることを確認すること(backlog item c9225fec)。schedule.rsの衝突検知偽陽性/偽陰性調査とfugu-routerのtoken削減効果実測は、より調査/計測コストが高く別サイクルへ持ち越す。

## next_action

## parked

