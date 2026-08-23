# Codex Automations Plugin

Design-stage repository for a Rust MCP scheduler and Codex plugin that deliver durable, same-thread automations through an existing Codex app-server daemon.

The working product name is **Threadbeat**. The name remains provisional until the architecture review is approved.

## Review documents

- [`docs/00_REVIEW_PACKET.md`](docs/00_REVIEW_PACKET.md) — executive proposal and decision gate
- [`docs/01_PRODUCT_REQUIREMENTS.md`](docs/01_PRODUCT_REQUIREMENTS.md) — product requirements and acceptance criteria
- [`docs/02_ARCHITECTURE.md`](docs/02_ARCHITECTURE.md) — system architecture and failure semantics
- [`docs/03_MCP_TOOL_CONTRACT.md`](docs/03_MCP_TOOL_CONTRACT.md) — exact proposed agent-visible tools
- [`docs/04_SECURITY_PERFORMANCE.md`](docs/04_SECURITY_PERFORMANCE.md) — security model and Raspberry Pi budgets
- [`docs/05_IMPLEMENTATION_PLAN.md`](docs/05_IMPLEMENTATION_PLAN.md) — phased build and verification plan
- [`docs/06_RESEARCH_BASELINE.md`](docs/06_RESEARCH_BASELINE.md) — current Codex behavior and primary-source evidence
- [`docs/07_ADVERSARIAL_REVIEW.md`](docs/07_ADVERSARIAL_REVIEW.md) — independent security and Pi reliability findings
- [`docs/08_STATE_AND_TIME_SEMANTICS.md`](docs/08_STATE_AND_TIME_SEMANTICS.md) — revised normative crash, clock, and coalescing behavior

No production implementation has been started. Independent review currently authorizes only a disposable compatibility/failure-semantics spike, not the persistent MVP.
