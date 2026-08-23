# Architecture

Status: proposed architecture, source snapshot 2026-08-22

## Architectural decision

Use a resident Rust scheduler with its own SQLite time queue and make the existing Codex app-server’s durable per-task queue the strict delivery adapter.

```text
Codex agent
    |
    | MCP over stdio
    v
threadbeat mcp ---------------------------+
    | private local control socket         |
    v                                      |
threadbeat daemon                          |
    |                                      |
    +-- schedule engine                    |
    +-- SQLite schedule/occurrence ledger  |
    +-- app-server client                  |
            |                              |
            | WebSocket over Unix socket   |
            | or SSH-proxied loopback      |
            v                              |
      existing Codex app-server -----------+
            |
            | thread/queue/add for due work
            v
        existing Codex task
```

The MCP process is intentionally not the scheduler. Codex clients may start and stop an MCP subprocess at any time; tying timers to that lifetime would make schedules unreliable. All MCP mutations go through the daemon, which is the single scheduler leader and database writer.

## Why there are two queues

### Threadbeat occurrence queue

This is the calendar source of truth. It owns:

- recurrence and timezone rules;
- next nominal due time;
- task and schedule revisions;
- occurrence identities;
- catch-up, coalescing, retry, and expiry;
- delivery intent and ambiguity; and
- lifecycle history and retention.

### Codex task prompt queue

This is the final admission boundary. It owns:

- durable prompts already due for one Codex task;
- FIFO ordering;
- waiting for that task to become idle; and
- atomic transition from queued prompt to active turn.

It does not own calendar schedules or missed-run policy. Future occurrences must never be preloaded into it. Current Codex caps the queue at 100 items per task; Threadbeat should normally keep no more than one pending item per schedule.

## Why not call `turn/start`

Current app-server `turn/start` uses start-or-steer behavior: if a turn is already active, the new input can be steered into it, and the response does not distinguish that from a clean new turn. A preceding status read is only a time-of-check/time-of-use race.

Current `thread/queue/add` instead persists a prompt and starts it through an idle-only path. It is therefore the only current path that meets strict same-task scheduling. It is experimental, so the backend policy is:

| Adapter | Behavior |
|---|---|
| `queue_v1` | Strict mode. Capability-gated `thread/queue/*`; required for arbitrary existing tasks. |
| `stable_direct` | Best-effort, explicit opt-in only. Unsafe for a task humans or other clients may use. Not in MVP UI. |
| `unsupported` | Pause strict schedules and explain how to restore compatibility. |

There is no silent downgrade.

Primary evidence: the [official app-server documentation](https://learn.chatgpt.com/docs/app-server), the current [queue semantics](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/app-server/README.md#L790-L812), and the [idle-only dispatch implementation](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/ext/queue/src/service.rs#L366-L459).

## Binary and plugin layout

One Rust workspace produces one release binary:

- `threadbeat daemon`: scheduler, database, and persistent app-server client.
- `threadbeat mcp --stdio`: lightweight MCP facade speaking to the daemon over a private Unix socket.
- `threadbeat ctl`: human-readable list, run, pause, resume, status, and redacted export commands.
- `threadbeat service`: install, upgrade, uninstall, start, stop, and validate systemd/launchd configuration.
- `threadbeat schema`: generate/check the exact app-server schema subset used by the adapter.

The plugin contributes:

- `.codex-plugin/plugin.json`;
- `.mcp.json` using a direct top-level server map, avoiding the currently documented/parser wrapper-key mismatch;
- `threadbeat-automations` skill;
- `threadbeat-setup` skill;
- the platform binary selected at installation; and
- logo/icon/screenshot assets.

The plugin MCP configuration launches `threadbeat mcp --stdio`; the resident service remains independent.

## Rust stack

Initial candidates, pinned exactly during implementation:

- `rmcp 3.1.4`, with `default-features = false`, `server`, `macros`, and `transport-io`;
- Tokio with a current-thread or very small multi-thread runtime and only required features;
- `tokio-tungstenite` for WebSocket framing over both TCP and `tokio::net::UnixStream`;
- `serde`/`serde_json` for deliberately small protocol DTOs;
- one `rusqlite` database actor, with bundled SQLite for reproducible releases;
- `jiff` or an equivalently measured IANA-timezone implementation for pure recurrence calculation;
- `uuid`, `tracing`, `clap`, and `secrecy`.

Do not depend on unpublished/internal Codex path crates or the stale public `codex-app-server-protocol` crate. Generate the protocol schema from each supported installed Codex binary and maintain tolerant DTOs only for the methods used.

The official Rust MCP SDK supports server macros and stdio transport; it remains fast-moving, so `Cargo.lock`, exact dependency pins, and compatibility tests are required. See the [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) and [Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp#stdio-servers).

## App-server protocol client

Codex app-server is not MCP. It is a separate JSON-RPC-like protocol whose envelopes omit the usual `"jsonrpc":"2.0"` field.

### Required connection sequence

1. Establish WebSocket framing over the selected byte stream.
2. Send `initialize` once, requesting `capabilities.experimentalApi=true` for strict queue support.
3. Receive server identification and record the sanitized user agent/platform.
4. Send `initialized`.
5. Probe `thread/queue/list` and classify the backend.
6. Resume only tasks with schedules or nonterminal occurrences.
7. Subscribe to the lifecycle events required for correlation.

The client must support:

- multiplexed request IDs mapped to bounded one-shot responders;
- server responses, notifications, and server-initiated requests;
- bounded inbound/outbound message sizes and channels;
- ping/health detection, reconnect, timeout, and cancellation;
- exponential backoff with jitter for disconnects and `-32001 Server overloaded`;
- unknown optional fields; and
- fail-closed parsing for missing IDs or unknown safety-critical states.

App-server has no negotiated semantic protocol version. Compatibility is established by runtime capability probing plus fixtures generated by `codex app-server generate-json-schema`. The daemon records the tested Codex build and schema SHA-256, but neither is a substitute for a live probe.

### Methods and events used

Connection and task lifecycle:

- `initialize`, `initialized`
- `thread/read`, `thread/resume`
- `thread/status/changed`, `thread/closed`
- `turn/started`, `turn/completed`
- `item/started` or `item/completed` for `userMessage.clientId`

Strict delivery:

- `thread/queue/add`, `thread/queue/list`, `thread/queue/start`
- `thread/queue/changed`
- optionally `thread/queue/update`, `delete`, and `reorder` for cancellation/edit reconciliation

Queue methods are currently [experimental and capability-gated](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/app-server-protocol/src/protocol/common.rs#L577-L611).

## Transport topology

Preferred order:

1. **Same host:** WebSocket HTTP Upgrade over the app-server Unix socket.
2. **Remote host:** OpenSSH carries the WebSocket byte stream to `codex app-server proxy`, or an SSH local forward reaches a loopback app-server listener.
3. **Advanced preview:** direct WSS with certificate validation and app-server capability-token or signed-bearer authentication.

Plain unauthenticated non-loopback WebSocket is forbidden. The MCP-facing transport is independent: bundled/local plugin use is stdio; a future remote MCP mode may use authenticated Streamable HTTP only after a separate threat review.

The setup routine validates an existing daemon. It does not silently bootstrap, update, or replace Codex app-server.

## Schedule representation

Internally use a closed versioned representation, not free-form RRULE:

```text
once(at)
interval(everyMinutes, anchor)
weekly(timezone, groups[{weekdays[], localTimes[]}], dstPolicy)
```

The recurrence engine is a pure function:

```text
next(rule, after_instant) -> next nominal occurrence
between(rule, start, end, caps) -> bounded nominal occurrences
```

Intervals live on the UTC elapsed timeline and use `anchor + n × duration`. Weekly rules preserve local wall-clock intent in an IANA zone. Materialized history is immutable when timezone data changes; future occurrences are recalculated.

The scheduler uses one earliest-deadline timer, capped with periodic wall-clock comparison so NTP/manual jumps are detected. It never creates one timer per schedule.

## Database model

### `app_server_profiles`

Stores endpoint type/address, secret reference, expected constraints, last sanitized server identity, capability flags, and health timestamps. Raw secrets are never stored here.

### `schedules`

Stores UUIDv7 ID, optimistic revision, name, profile, target task, prompt, normalized rule, timezone/DST/catch-up policy, enabled state, next due instant, and timestamps.

### `occurrences`

Stores deterministic ID, schedule/revision, nominal due time, intended local time, timezone/offset, deterministic client message ID, lifecycle state, claim lease, remote queue submission ID, turn ID, retry lineage, and timestamps.

Unique key: `(schedule_id, schedule_revision, nominal_due_at)`.

### `dispatch_attempts`

Append-only attempt number, request ID, connection generation, start/end, outcome, error code, and a redacted response digest.

### `schedule_events`

Append-only audit entries for create, edit, pause, resume, delete, catch-up, coalescing, and authority/capability warnings.

### `request_keys`

Maps MCP mutation request keys to a stable result so a repeated tool call does not duplicate a schedule or immediate occurrence.

### `leader_lease`

Ensures only one daemon schedules a database. A second instance exits or becomes read-only; it never competes as another leader.

SQLite starts with foreign keys, WAL, busy timeout, database/WAL mode `0600`, and `synchronous=FULL` for schedule/outbox transitions. Any lower-durability mode is an explicit advanced option backed by measurements.

## Occurrence state machine

```text
SCHEDULED -> DUE -> CLAIMED -> RECONCILING -> THREAD_QUEUED -> RUNNING
                              |                    |             |
                              |                    |             +-> SUCCEEDED
                              |                    |             +-> FAILED
                              |                    |             +-> INTERRUPTED
                              |                    |             +-> NEEDS_ATTENTION
                              |                    |
                              +-> DELIVERY_UNCERTAIN

pre-start states -> CANCELLED | EXPIRED | SKIPPED | COALESCED
```

- `THREAD_QUEUED` requires authoritative queue acceptance/reconciliation.
- `RUNNING` requires correlation to a task user message/turn, not merely a successful RPC.
- `SUCCEEDED`/`FAILED` requires `turn/completed` or reconstructed durable history.
- `INTERRUPTED` blocks automatic task-queue continuation until explicit resume.

## Due-occurrence transaction

1. The global scheduler timer reaches or passes the earliest `next_due_at`.
2. In one `BEGIN IMMEDIATE` transaction, select due schedules, materialize uniquely keyed occurrences, apply catch-up/coalescing caps, and advance schedule cursors.
3. Notify a bounded dispatcher (two global workers by default).
4. Acquire a per-task admission permit and claim the occurrence with an expiring lease.
5. Persist `RECONCILING` before network I/O.
6. Search the remote queue for `clientUserMessageId`.
7. Search task history for `userMessage.clientId` if necessary.
8. If neither contains it and the prior outcome is known not to have committed, call `thread/queue/add`.
9. Persist the returned remote submission ID and `THREAD_QUEUED`.
10. Follow task events and periodically reconcile nonterminal state.

## Idempotency and the exactly-once gap

Local scheduling is idempotent. Remote insertion currently is not: app-server allocates a fresh queue UUID for every add, while `clientUserMessageId` is preserved but not uniqueness-constrained. The implementation can therefore be duplicate-resistant but cannot prove exactly-once delivery.

The deterministic client ID is UUIDv5 over:

```text
namespace + schedule_id + schedule_revision + nominal_due_at_utc
```

On a lost response:

1. list the task queue for the client ID;
2. read recent task history for the same client ID;
3. reconcile if either proves acceptance;
4. wait a bounded grace period if handoff may still be persisting; and
5. otherwise mark `DELIVERY_UNCERTAIN` without automatic resend.

The non-idempotent insertion is visible in the [current queue-row implementation](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/state/src/runtime/queued_items.rs#L77-L104). True exactly-once behavior requires an upstream unique constraint and atomic queue-to-history handoff.

## Recovery

On daemon start:

1. acquire leader lease;
2. migrate and run SQLite integrity checks;
3. expire stale worker claims;
4. materialize bounded overdue catch-up;
5. connect, initialize, and capability-probe app-server;
6. resume tasks with nonterminal work;
7. reconcile queue and history by deterministic client ID; and
8. schedule the earliest future deadline.

On disconnect, preserve all states, reconnect with jittered backoff, and reconcile before any new mutation. Socket loss never implies that a turn failed or completed.

On clock movement, recompute against wall time without moving interval anchors. Backward jumps do not repeat a nominal occurrence because of the unique key.

## Approvals and task settings

Queued submissions currently carry prompt input and client message ID, not per-turn working-directory, model, sandbox, or approval overrides. A scheduled turn therefore inherits the task’s current settings, including later human changes.

Threadbeat:

- never changes sticky task settings merely to run an automation;
- never responds to approval or user-input server requests;
- records `needs_attention` from active flags/events;
- coalesces later occurrences while blocked; and
- reports the inherited-settings constraint in create/get/doctor responses.

This preserves user control, but it means a human-owned task is less reproducible than a dedicated automation task. That tradeoff is inherent in the requested same-task experience.

## Service architecture

### Raspberry Pi/Linux

Default: a systemd user service with lingering for a single-user device. The process runs in the foreground with `Restart=on-failure`, explicit state/runtime directories, restricted address families, `NoNewPrivileges`, filesystem protection, rate-limited journald output, and credentials loaded from systemd facilities rather than command arguments.

### macOS

Default: per-user LaunchAgent, foreground process, SIGTERM handling, Keychain-backed secrets, and a private application-support state directory.

The Rust `service` subcommand renders units/plists, prints the exact changes, applies them only after explicit approval, and then runs `doctor`. Shell wrappers may invoke platform service managers, but scheduling and MCP logic remain Rust.

## Compatibility and upgrade policy

- Define support by a tested binary matrix, not a guessed minimum version.
- Generate and hash the schema from every tested Codex binary.
- Probe queue methods after initialization on every daemon connection.
- Ignore additive fields; fail on missing required IDs and unknown safety-critical discriminators.
- If an upgrade removes strict capability, pause schedules and preserve all local state.
- Database migrations are forward-only with backup/rollback instructions; the service never auto-downgrades a database.
- Plugin upgrade is separate from Codex app-server upgrade.
