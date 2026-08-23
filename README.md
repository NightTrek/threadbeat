# Threadbeat

![Threadbeat logo](assets/composer-icon.png)

[![CI](https://github.com/NightTrek/threadbeat/actions/workflows/ci.yml/badge.svg)](https://github.com/NightTrek/threadbeat/actions/workflows/ci.yml)
[![Release](https://github.com/NightTrek/threadbeat/actions/workflows/release.yml/badge.svg)](https://github.com/NightTrek/threadbeat/actions/workflows/release.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Threadbeat is a Rust MCP scheduler and Codex plugin for durable, same-task heartbeat automations through an existing Codex app-server daemon. It never starts an app-server itself.

Threadbeat is an independent, unofficial project. It is not affiliated with, endorsed by, or supported by OpenAI. OpenAI, Codex, and related marks belong to their respective owners.

The public MCP surface has six tools: `automation_update`, `automation_run`, `automation_get`, `automation_list`, `automation_runs`, and `automation_doctor`. Schedule explanation and visualization stay in the bundled skill; there is no custom preview tool.

> **Alpha:** Threadbeat is available for testing and review, not production reliance. Delivery is disabled by default, no app-server build is trusted by default, and native Raspberry Pi soak/resource validation remains open.

## Release bundles

Each GitHub prerelease is built natively on four hosted runners:

| Bundle target | Build environment | Intended runtime |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Ubuntu 22.04 x64 | 64-bit x86 Linux with glibc 2.35 or newer |
| `aarch64-unknown-linux-gnu` | Ubuntu 22.04 ARM64 | 64-bit ARM Linux, including compatible Raspberry Pi OS installs |
| `x86_64-apple-darwin` | macOS Intel | Intel Macs on macOS 11 or newer |
| `aarch64-apple-darwin` | macOS Apple silicon | Apple silicon Macs on macOS 11 or newer |

The ARM64 Linux runner validates the architecture and ABI, but it is not Raspberry Pi hardware. Keep the alpha boundary until the native Pi test gate is complete.

Download the matching archive, `SHA256SUMS`, and provenance bundle from [Releases](https://github.com/NightTrek/threadbeat/releases). Verify the checksum and GitHub attestation before extracting:

```sh
gh release download v0.1.0-alpha.1 --repo NightTrek/threadbeat
sha256sum --check SHA256SUMS --ignore-missing
gh attestation verify threadbeat-plugin-v0.1.0-alpha.1-aarch64-unknown-linux-gnu.tar.gz \
  --repo NightTrek/threadbeat
tar -xzf threadbeat-plugin-v0.1.0-alpha.1-aarch64-unknown-linux-gnu.tar.gz
```

On stock macOS, use `shasum -a 256 -c SHA256SUMS` in place of `sha256sum`.

Every archive has one `threadbeat/` root with the plugin, source, target binary, setup skill, service templates, a target-filtered Cargo dependency CycloneDX SBOM, build metadata, locked dependency license files, and separate Rust/SQLite runtime notices. Archive paths, ordering, ownership, modes, and timestamps are normalized; the project does not claim bit-for-bit reproducible binaries across mutable hosted runner images.

## Packaged binary or source build

Prefer a checksummed, GitHub-attested packaged binary that exactly matches the target OS, architecture, and libc. Put that executable at `bin/threadbeat` in the plugin bundle; the plugin runs `threadbeat mcp --stdio`, while the separately managed service runs the same binary as `threadbeat --config <absolute-config-path> daemon`.

For a source build, use the repository wrapper. It uses the pinned lockfile, builds the release binary, installs it atomically as `bin/threadbeat` with mode `0755`, and prints the packaged binary's version:

```sh
./scripts/build-local.sh
cargo test --locked
```

The wrapper's deployable result is `bin/threadbeat`; copying it to a versioned service path remains a separate, explicit packaging step. On Raspberry Pi, prefer a packaged 64-bit ARM binary when that exact Pi OS/ABI target is published. A native source build installs a full Rust toolchain and can require substantial time, memory, temporary storage, and SD-card writes; on a constrained Pi, build in pinned CI or on a compatible host and still run a native smoke check on the Pi. Do not assume a host-built or cross-built binary matches the Pi's libc and CPU baseline without verification.

From a Git source checkout, maintainers can run the core local checks and native package build with:

```sh
./scripts/release.sh 0.1.0-alpha.1
```

`./scripts/release.sh VERSION --publish` creates and pushes a new, non-reused version tag after the local checks. That tag triggers the native four-platform workflow; a GitHub prerelease is created only after validation, all builds, archive verification, checksum generation, and attestation succeed. Manual `workflow_dispatch` runs the same build, verification, and attestation path without publishing.

## Service plan and diagnostics

Use absolute paths. Planning writes reviewable JSON and does not install or activate a service:

```sh
threadbeat --config /absolute/path/config.toml service plan \
  --destination /absolute/path/threadbeat.service \
  --output /absolute/path/threadbeat-service-plan.json
```

After reviewing the plan and authorizing its exact embedded digest, apply that exact file:

```sh
threadbeat --config /absolute/path/config.toml service apply \
  --plan /absolute/path/threadbeat-service-plan.json \
  --plan-digest <reviewed-digest>
```

Apply reconstructs the canonical plan and creates exactly the configured private state and runtime leaf directories with mode `0700` plus the service definition with mode `0600`. Their parent directories must already exist; apply will not create unreviewed ancestors. It prints an activation command but never runs it. Apply never loads, enables, starts, or bootstraps the service; those operations and lingering require separate approval. Once the daemon is running, use a real originating task ID for diagnostics or a temporary activation grant:

```sh
threadbeat --config /absolute/path/config.toml ctl doctor --thread-id <thread-id> --full
threadbeat --config /absolute/path/config.toml ctl authorize --thread-id <thread-id> --minutes 15
threadbeat --config /absolute/path/config.toml schema check
```

## Safety defaults

- Delivery is disabled until the operator configures an existing app-server endpoint and explicitly enables it.
- Strict compatibility rejects unverified app-server builds.
- A new active heartbeat requires a short-lived task grant; `suggested_create` is stored paused.
- Service apply creates only the configured `0700` state/runtime directories and `0600` service definition; it never loads, enables, starts, or bootstraps the service.
- Prompts, state, and the private control socket are stored with owner-only permissions.

## Alpha boundary

The local scheduler and six-tool MCP surface are implemented, but v0.1 is not a production release. No app-server build is trusted by default; queued-to-terminal lifecycle reconciliation, complete task-settings authority fingerprinting, Raspberry Pi hardware validation, and native Pi resource/soak evidence remain production gates. Generic ARM64 archives are checksummed and GitHub-attested, but that is not evidence of native Pi behavior. An interrupted pending mutation fails closed as `REQUEST_STATUS_UNCERTAIN` and must be inspected rather than replayed. See `docs/10_IMPLEMENTATION_STATUS.md` for the implemented-versus-pending matrix and a comparison with the current Codex app automation tool.

## Open source

Threadbeat is licensed under [Apache-2.0](LICENSE). Contributions are welcome under [CONTRIBUTING.md](CONTRIBUTING.md). Report vulnerabilities privately using [GitHub Security Advisories](https://github.com/NightTrek/threadbeat/security/advisories/new), as described in [SECURITY.md](SECURITY.md). Brand asset provenance and hashes are recorded in [ASSETS.md](ASSETS.md), and release history is in [CHANGELOG.md](CHANGELOG.md).

## Design and operations documents

- [`docs/00_REVIEW_PACKET.md`](docs/00_REVIEW_PACKET.md) — executive proposal and decision gate
- [`docs/01_PRODUCT_REQUIREMENTS.md`](docs/01_PRODUCT_REQUIREMENTS.md) — product requirements and acceptance criteria
- [`docs/02_ARCHITECTURE.md`](docs/02_ARCHITECTURE.md) — system architecture and failure semantics
- [`docs/03_MCP_TOOL_CONTRACT.md`](docs/03_MCP_TOOL_CONTRACT.md) — exact implemented agent-visible tools
- [`docs/04_SECURITY_PERFORMANCE.md`](docs/04_SECURITY_PERFORMANCE.md) — security model and Raspberry Pi budgets
- [`docs/05_IMPLEMENTATION_PLAN.md`](docs/05_IMPLEMENTATION_PLAN.md) — phased build and verification plan
- [`docs/06_RESEARCH_BASELINE.md`](docs/06_RESEARCH_BASELINE.md) — current Codex behavior and primary-source evidence
- [`docs/07_ADVERSARIAL_REVIEW.md`](docs/07_ADVERSARIAL_REVIEW.md) — independent security and Pi reliability findings
- [`docs/08_STATE_AND_TIME_SEMANTICS.md`](docs/08_STATE_AND_TIME_SEMANTICS.md) — revised normative crash, clock, and coalescing behavior
- [`docs/09_SKILL_AND_PLUGIN_DESIGN.md`](docs/09_SKILL_AND_PLUGIN_DESIGN.md) — plugin manifest, skills, setup, distribution, and image brief
- [`docs/10_IMPLEMENTATION_STATUS.md`](docs/10_IMPLEMENTATION_STATUS.md) — implemented alpha surface, known limits, and Codex app comparison

The implementation remains intentionally conservative around app-server compatibility: use the config-qualified `schema check` command above against the exact deployed build before enabling delivery.

The macOS alpha archives are not Apple Developer ID signed or notarized. GitHub artifact attestation proves workflow provenance for the published archive digest; it is a different trust mechanism from platform code signing.
