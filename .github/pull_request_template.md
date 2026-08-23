## Summary

Describe the user-visible change and why it is needed.

## Correctness and safety

- Failure and restart behavior:
- Authorization or trust-boundary change:
- Raspberry Pi CPU, memory, wakeup, or disk-write impact:

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features --locked -- -D warnings`
- [ ] `cargo test --all-targets --all-features --locked`
- [ ] Documentation and changelog updated when applicable
- [ ] No secrets, private prompts, task IDs, or local service state included
