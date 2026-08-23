# Threadbeat architecture review packet

Status: proposal for review, 2026-08-22

## Recommendation

Build one lean Rust binary with separate `daemon`, `mcp`, `ctl`, `service`, and `schema` modes, packaged in a Codex plugin with two skills:

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

This is preferable to calling `turn/start`: current `turn/start` can steer input into an already-active turn, while `thread/queue/add` waits durably and starts atomically when the task is idle. The important constraint is that the thread queue is currently experimental. Strict same-thread scheduling therefore pauses if that capability disappears; it never silently downgrades to a racy path.

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
| Permissions | Inherit the task; never broaden them |
| Calendar timezone | Explicit IANA timezone |
| DST spring gap | Skip the nonexistent local occurrence |
| DST fall fold | Run once at the first occurrence |
| App-server topology | Same-host Unix socket; otherwise loopback through SSH |
| Remote fallback | No direct `turn/start` fallback in strict mode |

“Exact schedule” means that nominal due times are calculated exactly and never dispatched early. It does not promise that model execution begins at that exact second: downtime, a busy task, approvals, or connectivity can delay it. Each run reports nominal due, enqueued, started, and completed timestamps separately.

## Five name candidates

1. **Threadbeat** — recommended; communicates recurring life in one task without claiming an OpenAI trademark.
2. **Dbox Automations** — preserves the user’s suggested direction; distinctive but less self-explanatory.
3. **Turnkeeper** — emphasizes serialized, durable turn delivery.
4. **Wakewire** — emphasizes remote wakeups and daemon connectivity.
5. **Thread Relay** — clear and descriptive, though less ownable as a brand.

The repository stays at `codex-automations-plugin` until a name is chosen, avoiding churn in package IDs and service names.

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
│   ├── logo-light.png
│   ├── logo-dark.png
│   └── screenshots/
└── LICENSE
```

Prebuilt, checksummed binaries are the default distribution path for Raspberry Pi 64-bit, Linux x86-64, and Apple Silicon/Intel macOS. Building on the device remains a supported fallback, not a requirement. Release builds commit `Cargo.lock`, pin the Rust MCP SDK, and receive a native arm64 Pi smoke test.

## What the MVP includes

- Same-task heartbeat schedules.
- One-shot, anchored interval, and weekly local-time schedules.
- Multiple weekdays and multiple times, including “10:00 and 12:00 Monday through Thursday.”
- Durable restart recovery and bounded catch-up.
- Pause, resume, edit, soft-delete, run-now, run history, retry, and diagnostics.
- Existing app-server daemon discovery and connection; Threadbeat does not own or start Codex by default.
- systemd user-service installation for Raspberry Pi/Linux and a launchd LaunchAgent on macOS.
- Strict service/socket permissions, redacted logs, and no agent-callable endpoint reconfiguration.

## Explicit non-goals for MVP

- Replacing Codex app-server or running a new Codex instance per event.
- Exposing raw RRULE strings to users or agents.
- Simultaneous turns in the same task.
- Auto-approving tool calls or fabricating answers to user-input requests.
- Exactly-once execution claims; the upstream queue does not yet offer an idempotent insert.
- Arbitrary file, image, or audio attachments in scheduled prompts.
- A custom graphical schedule editor.
- Direct unauthenticated network listeners.

## Decisions requested at the review gate

The architecture has opinionated defaults. Four decisions are worth resolving before code:

1. **Name:** accept `Threadbeat`, choose another candidate, or supply a new name.
2. **Experimental dependency:** accept that strict same-thread delivery is a technical preview until `thread/queue/*` stabilizes.
3. **Calendar catch-up:** accept one coalesced catch-up within 24 hours for both interval and weekly heartbeats, or make weekly schedules skip by default.
4. **Supported remote path:** make SSH-to-loopback the only supported remote topology in v1, or also support direct authenticated WSS as an advanced preview.

Everything else in this packet is proposed as a default rather than another setup question.

## Image plan

Image generation belongs immediately after the name decision so the marks do not need to be discarded. The proposed asset set is:

- a simple pulse-plus-thread composer icon that remains legible at 16–32 px;
- coordinated light and dark square logos;
- a schedule-preview screenshot showing weekday/time normalization;
- a run-lifecycle screenshot separating scheduled, thread-queued, running, and completed states; and
- a daemon-health screenshot showing the Pi, SSH/Unix-socket path, and app-server capability status.

All images will be generated without OpenAI/Codex trade dress unless branding permission is established, then reviewed at actual manifest sizes.

## Evidence boundary

The design is based on the locally installed Codex `0.149.0-alpha.4.1`, its generated JSON schema, and a source snapshot at `openai/codex` commit `422239eb4b1e0d0f85fac7256a079a1befe78472`. The exact installed daemon is capability-probed at runtime; a source date or guessed version is never treated as compatibility proof.

See the remaining documents for requirements, exact tool schemas, failure semantics, threats, budgets, test gates, and primary-source links.
