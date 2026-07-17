## north_star
「PDOタスク衝突は回避されたか」という問いに実証で答えられる状態にする: schedule.rsの並列/直列判定を実際のworker/merge込みend-to-endで検証し、backlog lock heartbeatのTTL耐性を検証し、touched_filesのrepo-relative契約を強制し、fugu-routerのtoken削減効果を計測可能にする

## definition_of_done
- condukt::schedule.rsの並列/直列判定について、実際にworker/merge込みのend-to-end capstoneテストが追加され、意図的に衝突させたタスクが直列化されて両方正しく着地することが確認される(backlog 415718c9)
- backlog::lockのheartbeatがTTL(30分)超過によるstale reapを防ぐことを、lockファイルのheartbeat_atを直接過去日時に書き換える手法で時間を待たずに検証する回帰テストが追加される(backlog cbb429c0)
- condukt::schedule.rsのtouched_filesがrepo-relative規約を守っているかのsanity checkが追加され、絶対パスや`..`混入を拒否または警告する(backlog a91f2b35)
- fugu-routerのepisodeにsuggested_model/route_basis/tokens_input/tokens_outputを記録する配線が追加され、hypothesis f5f9522aを将来close可能にする最低限のデータ収集が始まる(backlog 030a2f1e)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
4件のbacklog項目(415718c9優先度p1, cbb429c0優先度p2, a91f2b35, 030a2f1e)が既に積まれ準備済み。最小right-sizeな一手は優先度順に処理すること: まず415718c9(schedule.rsのcapstone E2Eテスト)から着手する。

## next_action

## parked

