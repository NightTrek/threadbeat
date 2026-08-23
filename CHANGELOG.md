# Changelog

All notable changes are recorded here. Threadbeat follows [Semantic Versioning](https://semver.org/) and uses prerelease identifiers while production gates remain open.

## Unreleased

## 0.1.0-alpha.1 - 2026-08-23

### Added

- Rust daemon, SQLite scheduler, local control protocol, and existing-app-server queue adapter.
- Six-tool MCP surface for automation mutation, run control, reads, history, and diagnostics.
- Once, anchored interval, and IANA-timezone weekly schedules with bounded catch-up.
- Codex plugin manifest, automation/setup skills, service plan/apply templates, and brand assets.
- Native x86-64 and ARM64 release pipeline for Linux and macOS with normalized archive metadata, target-filtered Cargo dependency SBOMs, runtime notices, dependency licenses, checksums, and GitHub provenance.

### Security

- Delivery is disabled by default and exact app-server builds are denied until verified.
- Active creation and risky manual controls require a task-scoped grant; remote queue mutations enforce originating-task ownership.
- Service setup is content-addressed and never activates a service implicitly.

### Known limitations

- No app-server build is trusted out of the box.
- Queued runs are not yet reconciled through terminal completion.
- Full task-settings authority fingerprinting and transactional interrupted-request recovery remain future work.
- Native Raspberry Pi performance, resource, and soak gates are not yet complete.
