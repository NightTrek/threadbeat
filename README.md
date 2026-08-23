# Threadbeat

Threadbeat is a Rust MCP scheduler and Codex plugin for durable, same-task heartbeat automations through an existing Codex app-server daemon. It never starts an app-server itself.

The public MCP surface has six tools: `automation_update`, `automation_run`, `automation_get`, `automation_list`, `automation_runs`, and `automation_doctor`. Schedule explanation and visualization stay in the bundled skill; there is no custom preview tool.

## Packaged binary or source build

Prefer a signed, checksummed packaged binary that exactly matches the target OS, architecture, and libc. Put that executable at `bin/threadbeat` in the plugin bundle; the plugin runs `threadbeat mcp --stdio`, while the separately managed service runs the same binary as `threadbeat --config <absolute-config-path> daemon`.

For a source build, use the repository-pinned Rust toolchain and lockfile:

```sh
cargo build --release --locked
cargo test --locked
```

The result is `target/release/threadbeat`; copying it into `bin/` or a versioned service path is a separate, explicit packaging step. On Raspberry Pi, prefer a packaged 64-bit ARM binary when that exact Pi OS/ABI target is published. A native source build installs a full Rust toolchain and can require substantial time, memory, temporary storage, and SD-card writes; on a constrained Pi, build in pinned CI or on a compatible host and still run a native smoke check on the Pi. Do not assume a host-built or cross-built binary matches the Pi's libc and CPU baseline without verification.

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

Apply only writes the service definition. It prints an activation command but never runs it. Loading, enabling, starting, lingering, or bootstrapping the service requires separate approval. Once the daemon is running, use a real originating task ID for diagnostics or a temporary activation grant:

```sh
threadbeat --config /absolute/path/config.toml ctl doctor --thread-id <thread-id> --full
threadbeat --config /absolute/path/config.toml ctl authorize --thread-id <thread-id> --minutes 15
threadbeat --config /absolute/path/config.toml schema check
```

## Safety defaults

- Delivery is disabled until the operator configures an existing app-server endpoint and explicitly enables it.
- Strict compatibility rejects unverified app-server builds.
- A new active heartbeat requires a short-lived task grant; `suggested_create` is stored paused.
- Service installation is plan-and-apply and never enables or starts the service automatically.
- Prompts, state, and the private control socket are stored with owner-only permissions.

## Design and operations documents

- [`docs/00_REVIEW_PACKET.md`](docs/00_REVIEW_PACKET.md) — executive proposal and decision gate
- [`docs/01_PRODUCT_REQUIREMENTS.md`](docs/01_PRODUCT_REQUIREMENTS.md) — product requirements and acceptance criteria
- [`docs/02_ARCHITECTURE.md`](docs/02_ARCHITECTURE.md) — system architecture and failure semantics
- [`docs/03_MCP_TOOL_CONTRACT.md`](docs/03_MCP_TOOL_CONTRACT.md) — exact proposed agent-visible tools
- [`docs/04_SECURITY_PERFORMANCE.md`](docs/04_SECURITY_PERFORMANCE.md) — security model and Raspberry Pi budgets
- [`docs/05_IMPLEMENTATION_PLAN.md`](docs/05_IMPLEMENTATION_PLAN.md) — phased build and verification plan
- [`docs/06_RESEARCH_BASELINE.md`](docs/06_RESEARCH_BASELINE.md) — current Codex behavior and primary-source evidence
- [`docs/07_ADVERSARIAL_REVIEW.md`](docs/07_ADVERSARIAL_REVIEW.md) — independent security and Pi reliability findings
- [`docs/08_STATE_AND_TIME_SEMANTICS.md`](docs/08_STATE_AND_TIME_SEMANTICS.md) — revised normative crash, clock, and coalescing behavior
- [`docs/09_SKILL_AND_PLUGIN_DESIGN.md`](docs/09_SKILL_AND_PLUGIN_DESIGN.md) — plugin manifest, skills, setup, distribution, and image brief

The implementation remains intentionally conservative around app-server compatibility: use the config-qualified `schema check` command above against the exact deployed build before enabling delivery.
