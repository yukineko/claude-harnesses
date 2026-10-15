Compile-time proof that `Status` has no `NeedsRuling` / `Unconfirmed` variant
(user ruling 2026-10-15). Included as doctests from `src/lib.rs`.

Positive control (MUST compile, otherwise the compile_fail blocks prove nothing):

```rust
use backlog::status::Status;
let _ = [Status::Pending, Status::Done, Status::Failed, Status::Cancelled, Status::Claimed];
```

```compile_fail
let _ = backlog::status::Status::NeedsRuling;
```

```compile_fail
let _ = backlog::status::Status::Unconfirmed;
```
