# Threadbeat v0.1 implementation status

Status date: 2026-08-23

Threadbeat is a buildable alpha with safe defaults. The scheduler, MCP facade, local control protocol, SQLite store, strict app-server queue adapter, service planner, plugin manifest, skills, and brand assets are implemented. Delivery is disabled by default and remains blocked until the operator configures an existing app-server and adds exact-build semantic-test evidence.

## Agent-facing tools

| Tool | What it does |
|---|---|
| `automation_update` | Suggested-create, active create, update, pause, resume, or soft-delete one heartbeat in the originating task. It returns the normalized schedule and next five occurrences. |
| `automation_run` | Run now, cancel one exact pending run, explicitly retry a failed or uncertain run, or start one exact owned blocked queue head. |
| `automation_get` | Read one task-scoped automation, revision, normalized schedule, authority state, blocker, and latest run. |
| `automation_list` | List and filter automations in the originating task with bounded pagination. |
| `automation_runs` | Read bounded task-scoped run history and distinguish local due, remote queued, running, terminal, blocked, ownership-violating, and uncertain states. |
| `automation_doctor` | Read sanitized daemon, SQLite, clock, delivery, app-server compatibility, authority, and scheduler diagnostics. |

There is no `automation_preview` tool. Schedule explanation is handled by the skill, while `automation_update` returns authoritative normalized occurrences.

## What is implemented

- One-shot, fixed-rate anchored interval, and IANA-timezone weekly schedules with multiple weekday/time groups.
- Explicit DST gap/fold policy and stable logical occurrence identity.
- Bounded `skip`, `coalesce`, and `all` restart catch-up.
- A resident Tokio daemon with one earliest polling loop, a bounded Unix-socket control protocol, a lifetime single-instance lock, and owner-only state/runtime paths.
- SQLite WAL storage, schema migration, optimistic revisions, one-use task grants, row/age quotas, and terminal-history pruning.
- Fail-closed request idempotency: successes and deterministic failures are cached with compare-and-set completion; an interrupted pending mutation is never blindly replayed.
- Existing-app-server transports for loopback WebSocket, Unix-socket WebSocket, and bounded JSONL through a fixed proxy command.
- Capability and exact-build gating around `thread/queue/add`, `list`, `delete`, and exact-ID `start`; no `turn/start`, queue update, queue reorder, or app-server startup path exists.
- Exact remote-row ownership checks using target task, persisted random client message ID, queue submission ID, and canonical input digest.
- Content-addressed service plan/apply for systemd user services and launchd agents. Apply creates two reviewed `0700` leaf directories and one `0600` service file, and never activates it.
- A Codex plugin manifest, two skills, four generated image assets, pinned Rust dependencies/toolchain, CI, and local build/setup wrappers.

## Known alpha limits

These are release blockers for a production claim, not hidden assumptions:

1. No app-server build is trusted out of the box. A real released binary must pass the queue persistence, idle-start, crash-point, ownership, and shared-client semantic matrix before its exact identity is added to `verifiedBuilds`.
2. The adapter records authoritative queue acceptance, but the daemon does not yet subscribe to app-server task/turn events or reconcile queued work through terminal completion. `thread_queued` is therefore a truthful last-known state, not proof that the automation ran.
3. The current authority fingerprint binds the originating task identity, not the complete effective sandbox, approval reviewer, permission profile, model, working directory, or instruction snapshot. The upstream queue also cannot atomically bind those settings to a queued prompt.
4. A daemon crash between a local domain mutation and request-result completion leaves that request key `REQUEST_STATUS_UNCERTAIN`. Replay is deliberately blocked; the operator or agent must inspect automation/run state before using a new key. Full transactional request recovery is future work.
5. Remote add/delete/start ambiguity is retained as uncertain and never blindly replayed, but bounded queue-plus-history lifecycle reconciliation is not complete.
6. `automation_doctor` reports database/WAL bytes but does not yet measure filesystem free space or an effective retention horizon.
7. The repository now builds checksum-verified, GitHub-attested generic Linux/macOS x86-64 and ARM64 bundles. Raspberry Pi native smoke results, RSS/CPU/write-amplification measurements, multi-day Pi soak evidence, and any Pi-specific support claim are not yet produced.
8. Service apply requires the three parent directories (state parent, runtime parent, and platform service-file parent) to exist. It will not create unreviewed ancestors.

## Comparison with the current Codex app automation tool

The locally observed Codex app surface on 2026-08-23 exposes one `codex_app__automation_update` tool. It creates, updates, views, or deletes app-managed automations and recommends heartbeat as the recurring default. It also supports a separate project-scoped `cron` kind.

| Area | Codex app automation | Threadbeat v0.1 |
|---|---|---|
| Agent tool shape | One polymorphic `automation_update` tool | Six risk-separated MCP tools |
| Recurring default | Same-task heartbeat | Same-task heartbeat only |
| Other execution kind | Project-scoped cron with model and reasoning controls | Not included |
| Schedule input | App-owned RRULE representation | Structured once/interval/weekly input; no raw RRULE |
| Runtime owner | Codex desktop app | Independent Rust service |
| Delivery | App-internal task automation path | Existing app-server experimental task queue |
| Catch-up policy | App-managed | Explicit skip/coalesce/all with bounds |
| Run control/history | Not exposed as separate automation tools | Explicit run control and lifecycle reads |
| Diagnostics | App-managed | Explicit read-only doctor tool |
| Notifications | `failed_runs_only` policy is supported | Not implemented |
| Safety boundary | Codex app host context | Originating-task MCP metadata, local grant, exact-build gate, and queue ownership checks |
| Headless use | Requires the app automation runtime | Designed to run without the desktop app, but still requires a separately running app-server |

Threadbeat intentionally does not claim wire compatibility with the Codex app’s private automation storage. It mirrors the product behavior that matters for headless same-task heartbeats while exposing the extra operational controls an independent service needs.
