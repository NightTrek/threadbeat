# Threadbeat architecture review packet

Status: v0.1 alpha implemented with delivery disabled by default, 2026-08-23. See `10_IMPLEMENTATION_STATUS.md` for the verified implementation boundary and remaining release gates.

## Implemented recommendation

Threadbeat is one lean Rust binary with separate `daemon`, `mcp`, `ctl`, `service`, and `schema` modes, packaged in a Codex plugin with two skills:

1. an automation skill that teaches the agent when and how to create same-thread heartbeats; and
2. a setup skill that installs, validates, upgrades, and removes the resident service.

The daemon must use two deliberately separate durable queues:

```text
Threadbeat time queue                 Codex delivery queue
---------------------                 --------------------
schedule + timezone                   due prompt only
        |                                      |
materialized occurrence                         |
        |                                      |
SQLite run/outbox ledger -- thread/queue/add -> existing task
                                               |
                                      starts when task is idle
```

Threadbeat owns calendar truth, recurrence, catch-up, revisions, retries, and history. The existing Codex app-server owns final FIFO admission into a task. Future calendar events are never preloaded into Codex.

This is preferable to calling `turn/start`: current `turn/start` can steer input into an already-active turn, while current source makes `thread/queue/add` the candidate idle-gated admission path. Its queue-to-history crash semantics are still unproved on a released binary and the API is experimental. Strict same-thread scheduling therefore remains blocked until Phase 1, pauses if tested capability disappears, and never silently downgrades to a racy path.

## Product defaults

| Decision | Default |
|---|---|
| Automation kind | `heartbeat`, attached to the calling task |
| Bare “set up a heartbeat” cadence | Every 30 minutes |
| “Every N minutes” semantics | Fixed-rate from activation or supplied anchor; no completion drift |
| Busy task | Never overlap; coalesce later occurrences |
| Restart catch-up | One latest missed occurrence, at most 24 hours old |
| Interrupted turn | Pause delivery and report `needs_attention` |
| Ambiguous queue write | Report `delivery_uncertain`; do not blindly replay |
| Approval/user input | Never answer automatically |
| Permissions | Record and recheck an authorized task-settings fingerprint; pause on drift |
| Calendar timezone | Explicit IANA timezone |
| DST spring gap | Skip the nonexistent local occurrence |
| DST fall fold | Run once at the first occurrence |
| App-server topology | Same-host Unix socket; otherwise loopback through SSH |
| Remote fallback | No direct `turn/start` fallback in strict mode |
| MCP scope | Originating task only; cross-task administration is CLI-only |
| Consent | Host/CLI-verifiable grant by default; current-task trusted-agent mode is explicit opt-in |
| Remote queue depth | At most one Threadbeat-owned item per target task |

“Exact schedule” means that nominal due times are calculated exactly and are not dispatched before a trusted clock reaches them. It does not promise that model execution begins at that exact second: clock quarantine, downtime, a busy task, approvals, or connectivity can delay it. Each run reports nominal due, enqueued, started, and completed timestamps separately.

## Five name candidates

1. **Threadbeat** — recommended; communicates recurring life in one task without claiming an OpenAI trademark.
2. **Dbox Automations** — preserves the user’s suggested direction; distinctive but less self-explanatory.
3. **Turnkeeper** — emphasizes serialized, durable turn delivery.
4. **Wakewire** — emphasizes remote wakeups and daemon connectivity.
5. **Thread Relay** — clear and descriptive, though less ownable as a brand.

The product and package name is now **Threadbeat**. The repository retains its original planning-era directory name, `codex-automations-plugin`.

## MVP package

```text
threadbeat/
├── .codex-plugin/plugin.json
├── .mcp.json
├── skills/
│   ├── threadbeat-automations/SKILL.md
│   └── threadbeat-setup/SKILL.md
├── bin/
│   └── threadbeat
├── assets/
│   ├── composer-icon.png
│   ├── logo.png
│   ├── logo-dark.png
│   └── threadbeat-master.png
└── LICENSE
```

The repository includes a native four-runner pipeline for checksummed, GitHub-attested whole-plugin archives covering generic ARM64/x86-64 Linux and Apple Silicon/Intel macOS. Only artifacts attached to a successfully completed GitHub prerelease are considered produced. Generic ARM64 CI is not Raspberry Pi hardware evidence; native Pi smoke, resource, and soak validation remains a production gate.

## MVP target

This is the target product contract. Items not yet complete are called out in `10_IMPLEMENTATION_STATUS.md` and must not be inferred from this list as verified runtime behavior.

- Same-task heartbeat schedules.
- One-shot, anchored interval, and weekly local-time schedules.
- Multiple weekdays and multiple times, including “10:00 and 12:00 Monday through Thursday.”
- Durable local restart recovery and bounded catch-up after clock trust and the separately managed app-server return.
- Pause, resume, edit, soft-delete, run-now, run history, retry, and diagnostics.
- Existing app-server daemon discovery and connection; Threadbeat does not own or start Codex by default.
- systemd user-service installation for Raspberry Pi/Linux and a launchd LaunchAgent on macOS.
- Strict service/socket permissions, redacted logs, and no agent-callable endpoint reconfiguration.
- Current-task MCP scoping, authority-drift checks, queue-row ownership digests, and conservative ambiguity handling.

## Agent tool surface

The implemented package exposes six risk-separated tools. Schedule normalization is explained or visualized conversationally before the agent calls the mutation tool; there is no separate preview tool:

1. `automation_update`
2. `automation_run`
3. `automation_get`
4. `automation_list`
5. `automation_runs`
6. `automation_doctor`

The current Codex app exposes one direct `codex_app__automation_update` tool; its exact description and declaration are captured in `06_RESEARCH_BASELINE.md`. Threadbeat retains `automation_update` for schedule changes and separates high-risk immediate/recovery operations into `automation_run` so MCP annotations and consent policy remain accurate. Complete implemented descriptions, arguments, results, annotations, errors, and shared types are in `03_MCP_TOOL_CONTRACT.md`.

## Explicit non-goals for MVP

- Replacing Codex app-server or running a new Codex instance per event.
- Exposing raw RRULE strings to users or agents.
- Simultaneous turns in the same task.
- Auto-approving tool calls or fabricating answers to user-input requests.
- Exactly-once execution claims; the upstream queue does not yet offer an idempotent insert.
- Arbitrary file, image, or audio attachments in scheduled prompts.
- A custom graphical schedule editor.
- Direct unauthenticated network listeners.
- Direct WSS app-server access in v1; the tested CLI exposes WebSocket but not native TLS termination.
- A promise that Threadbeat starts or keeps the separately managed Codex app-server alive.

## Decisions remaining at the release gate

The architecture has opinionated defaults. Five decisions are worth resolving before production enablement and distribution:

1. **Name:** `Threadbeat` is selected for the alpha and public repository; any future rename requires a new compatibility and migration decision.
2. **Experimental dependency:** accept that strict same-thread delivery remains a technical preview until a released Codex build passes the queue crash/semantics matrix and `thread/queue/*` stabilizes.
3. **Inherited authority:** accept the documented same-task settings race, require dedicated scheduler-owned tasks, or wait for an upstream authority-snapshot API.
4. **Consent policy:** keep host/CLI-confirmed activation as the secure default, with `trusted_agent_current_task` available only as a setup-time personal-use opt-in.
5. **Calendar catch-up:** accept one coalesced catch-up within 24 hours for both interval and weekly heartbeats, or make weekly schedules skip by default.

SSH-to-loopback is now the only v1 remote path; direct WSS is deferred. The remaining items are alpha defaults and explicit release decisions, not blockers to inspecting or building the local package.

## Image assets

The alpha includes an original pulse-plus-thread mark without OpenAI/Codex trade dress in `assets/threadbeat-master.png`, `assets/logo.png`, `assets/logo-dark.png`, and `assets/composer-icon.png`. The two manifest logos intentionally share the same transparent source so they remain consistent across themes.

Schedule, lifecycle, and daemon-health screenshots remain future release collateral; they must be captured from verified runtime state rather than mocked as evidence.

## Evidence boundary

The design is based on the locally installed Codex `0.149.0-alpha.4.1`, its generated JSON schema, and a source snapshot at `openai/codex` commit `422239eb4b1e0d0f85fac7256a079a1befe78472`. This is insufficient for a production compatibility or support claim. A disposable Phase-1 spike must prove queue persistence, ownership, authority fields, and queue-to-history recovery on an actual released binary; supported production builds then come from a semantic compatibility allowlist, not a method probe alone.

See the remaining documents for requirements, exact tool schemas, failure semantics, threats, budgets, test gates, and primary-source links.
