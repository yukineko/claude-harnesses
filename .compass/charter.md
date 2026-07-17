## north_star
overwatch::leaseのセッション間タスクclaimをbacklog/condukt同様に排他ロック化し、TOCTOUによる二重claimを機構的に防ぐ

## definition_of_done
- lease::begin()のload_leases→is_held_by_otherチェック→save_leasesが、condukt::lock/backlog::lockと同じ排他ロック機構(O_EXCL/hardlinkベース、stale lock reap付き)で保護され、read-modify-write全体がアトミックになる
- 2セッション(プロセス)がほぼ同時に同一keyをbeginする結合テストが追加され、片方が必ずskip(exit 1、または明示的な待機後の順序どおりの成功)になり両方が成功することは無いと固定される。concurrencyテストが無い現状のギャップを埋める
- 既存のadvisory機能(scope_overlap警告・possible_duplicate近似重複警告)の既存テストが全てgreenのまま回帰しない

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
overwatch::lease::begin()はload_leases()→is_held_by_other()チェック→save_leases()というロック無しのread-modify-writeで、backlog::lock/condukt::lockが既に潰したのと同じTOCTOUクラスのバグが残っている(2セッションがほぼ同時に同一keyをbeginすると両方成功しうる)。concurrencyテストも無い(hypothesis 9c733d74で発見・記録済み)。最小right-sizeな一手は、condukt::lock(backlog::lockのhardlink+create_new方式を踏襲、stale lock reap・bounded wait・fail-soft degrade)と同じ設計をoverwatch::leaseのbegin()に適用し、2プロセス同時claimの結合テストで固定すること。

## next_action

## parked

