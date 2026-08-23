# Contributing to Threadbeat

Thank you for helping improve Threadbeat. The project favors small, reviewable changes with explicit failure semantics because it runs unattended and can write prompts into a Codex task queue.

## Development setup

Install the Rust toolchain declared in `rust-toolchain.toml`, Python 3.10 or newer, and ShellCheck. Then run:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
python3 scripts/check-release-version.py --print-current
```

Use `./scripts/build-local.sh` for a local binary. Never point tests at a real app-server or service state directory unless the test explicitly requires it and you have isolated the data.

## Pull requests

- Start with an issue for protocol changes, new MCP tools, state migrations, or expanded permissions.
- Keep commits focused and explain user-visible behavior, failure recovery, security impact, and Raspberry Pi resource impact.
- Add tests for normal behavior, restart/crash boundaries, authorization, idempotency, and malformed input where relevant.
- Preserve the six-tool MCP surface unless a reviewed contract change says otherwise. Schedule visualization belongs in the skill, not a preview tool.
- Update the changelog and implementation-status document when behavior or a release gate changes.
- Do not commit secrets, real task IDs, private prompts, access tokens, generated databases, sockets, or local service definitions.

By submitting a contribution, you agree that it is licensed under Apache License 2.0, the repository's project license.

## Release process

Maintainers run `./scripts/release.sh VERSION` locally, then a manual non-publishing release workflow on `main`. Only after all four native build jobs and the aggregate verifier pass should `./scripts/release.sh VERSION --publish` push a new, non-reused tag. A failed published tag is never moved or reused; fixes receive a new version.
