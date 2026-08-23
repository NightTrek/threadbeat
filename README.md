# Threadbeat

Threadbeat is a Rust MCP scheduler and Codex plugin for durable, same-task heartbeat automations through an existing Codex app-server daemon. It never starts an app-server itself.

The public MCP surface has six tools: `automation_update`, `automation_run`, `automation_get`, `automation_list`, `automation_runs`, and `automation_doctor`. Schedule explanation and visualization stay in the bundled skill; there is no custom preview tool.

## Build

Install Rust 1.88 or newer, then run:

```sh
cargo build --release --locked
cargo test --locked
```

Copy the release executable to `bin/threadbeat` before installing the plugin. The plugin launches `threadbeat mcp --stdio`; the separately managed service runs `threadbeat daemon`.

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

The implementation remains intentionally conservative around app-server compatibility: use `threadbeat schema check` against the exact deployed build before enabling delivery.
