# Architecture

Status: proposed architecture, source snapshot 2026-08-22

## Architectural decision

Use a resident Rust scheduler with its own SQLite time queue and treat the existing Codex app-server’s experimental per-task queue as the candidate strict delivery adapter, enabled only after released-binary semantic/crash evidence.

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

The MCP process is intentionally not the scheduler. Codex clients may start and stop an MCP subprocess at any time; tying timers to that lifetime would make schedules unreliable. All MCP calls begin with a versioned principal/originating-task handshake and go through the daemon, which holds an exclusive OS lock as the single scheduler leader and database writer.

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

It does not own calendar schedules or missed-run policy. Future occurrences must never be preloaded into it. Current Codex caps the shared queue at 100 items per task; Threadbeat keeps at most one owned remote-pending item per target task, regardless of schedule count, and never reorders another producer’s work.

## Why not call `turn/start`

Current app-server `turn/start` uses start-or-steer behavior: if a turn is already active, the new input can be steered into it, and the response does not distinguish that from a clean new turn. A preceding status read is only a time-of-check/time-of-use race.

Current source makes `thread/queue/add` the only candidate that persists a prompt and submits through an idle-only path. It could meet strict same-task admission, but released-binary crash/semantic tests have not proved the full guarantee. It is experimental, so the backend policy is:

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

The private control protocol is versioned and negotiates compatible daemon/facade versions, features, principal/originating-task binding, and frame limits. A plugin upgrade cannot silently talk to an incompatible old daemon.

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

App-server has no negotiated semantic protocol version. Compatibility requires both a semantically tested released-build allowlist and a live capability probe, plus fixtures generated by `codex app-server generate-json-schema`. Method presence/schema shape alone cannot prove FIFO, persistence, interruption, or crash recovery. Unknown builds are blocked by default or available only through an explicit preview override.

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
- exact-owned `thread/queue/delete` for cancellation reconciliation

Threadbeat never calls `thread/queue/update` or `reorder`, never starts with an omitted queue-item ID, and never mutates a row whose complete ownership tuple does not match. Schedule edits do not rewrite already queued prompts.

Queue methods are currently [experimental and capability-gated](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/app-server-protocol/src/protocol/common.rs#L577-L611).

## Transport topology

Preferred order:

1. **Same host:** WebSocket HTTP Upgrade over the app-server Unix socket.
2. **Remote host:** a dedicated/constrained OpenSSH key/account carries the WebSocket byte stream to the forced `codex app-server proxy` command, with no PTY, agent/X11 forwarding, arbitrary commands, or general port forwarding. A broad local-forward/general shell configuration is developer preview only, not supported v1.
Direct WSS is not in v1: the tested CLI exposes WebSocket listeners but no native TLS terminator, so a secure direct topology would add an unreviewed proxy/certificate/auth component. Plain unauthenticated non-loopback WebSocket is forbidden. The MCP-facing transport is independent: bundled/local plugin use is stdio; a future remote MCP mode may use authenticated Streamable HTTP only after a separate threat review.

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

The scheduler uses one earliest-deadline timer, capped with periodic wall-clock comparison so NTP/manual jumps are detected. It never creates one timer per schedule. No due materialization or delivery occurs until system time is trusted; large corrections quarantine scheduling as defined in `08_STATE_AND_TIME_SEMANTICS.md`.

## Database model

### `app_server_profiles`

Stores endpoint type/address, secret reference, expected constraints, last sanitized server identity, capability flags, and health timestamps. Raw secrets are never stored here.

### `schedules`

Stores UUIDv7 ID, optimistic revision, name, profile, target task, prompt, normalized rule, timezone/DST/catch-up policy, enabled state, next due instant, and timestamps.

### `occurrences`

Stores stable logical occurrence ID, materialized schedule revision, stable weekly slot/intended local datetime/resolution ordinal where applicable, resolved nominal due time, timezone/offset, cryptographically random persisted client message ID, canonical input digest, lifecycle state, daemon generation/claim, remote queue submission ID, turn ID, retry lineage, and timestamps.

The unique logical-occurrence key spans schedule revisions; resolved UTC alone is not identity. Exact keys and revision cutover are defined in `08_STATE_AND_TIME_SEMANTICS.md`.

### `dispatch_attempts`

Append-only attempt number, request ID, connection generation, start/end, outcome, error code, and a redacted response digest.

### `schedule_events`

Append-only audit entries for create, edit, pause, resume, delete, catch-up, coalescing, and authority/capability warnings.

### `request_keys`

Maps MCP mutation request keys to a stable result so a repeated tool call does not duplicate a schedule or immediate occurrence.

### `daemon_generations`

Records monotonically increasing process generations and shutdown/takeover evidence. An exclusive OS file lock, held for the entire daemon lifetime, is the actual same-host single-instance authority. The database must be on a local filesystem; shared/network filesystems are unsupported. Prior-generation in-flight remote intents are quarantined and reconciled before new mutations.

SQLite starts with foreign keys, WAL, busy timeout, database/WAL mode `0600`, and `synchronous=FULL` for schedule/outbox transitions. Any lower-durability mode is an explicit advanced option backed by measurements. A free-space gate, bounded WAL/checkpoint plan, byte-based retention, and allocated emergency reserve put the daemon into no-new-mutations mode before ordinary disk exhaustion; media failure after remote acceptance remains potentially uncertain.

## Occurrence state machine

The full event/guard/action table is normative in `08_STATE_AND_TIME_SEMANTICS.md`. Its main path is:

```text
PLANNED -> DUE -> CLAIMED -> RECONCILING -> ADD_INTENT -> REMOTE_QUEUED -> STARTED
                                              |                             |
                                              +-> DELIVERY_UNCERTAIN         +-> SUCCEEDED
                                                                            +-> TURN_FAILED
                                                                            +-> INTERRUPTED
                                                                            +-> waiting/authority states
```

Uppercase names in this section are internal implementation states. The only public run-state vocabulary is the exact lowercase `RunState` union in `03_MCP_TOOL_CONTRACT.md`; internal claim/intent states map to public `delivering` plus a `phase`.

- Internal `REMOTE_QUEUED` maps to public `thread_queued` only after authoritative queue acceptance/reconciliation.
- Internal `STARTED` maps to public `running` only after correlation to a task user message/turn, not merely a successful RPC.
- Public `succeeded` or `turn_failed` requires `turn/completed` or reconstructed durable history.
- Every remote add, exact-owned delete, and exact-ID start has its own durable intent and ambiguity outcome; remote queue update is unused.
- Client ID alone is not ownership; target, canonical input digest, occurrence, and known submission ID must match.
- Zero matches are handled from transport evidence; multiple otherwise matching rows are `remote_ownership_violation`/uncertainty, never proof of success.
- `INTERRUPTED` blocks automatic task-queue continuation until explicit resume.

## Due-occurrence transaction

1. The global scheduler timer reaches or passes the earliest `next_due_at`.
2. In one `BEGIN IMMEDIATE` transaction, select due schedules, materialize uniquely keyed occurrences, apply catch-up/coalescing caps, and advance schedule cursors.
3. Notify a bounded dispatcher (two global workers by default).
4. Acquire the fair per-task admission lane; require trusted clock, healthy storage, tested backend, current-task consent, matching authority fingerprint, safe external queue depth, and no existing Threadbeat-owned remote item for that task.
5. Claim under the current daemon generation and persist `RECONCILING` plus a random client ID and canonical input digest before network I/O.
6. Search the remote queue/history for the complete ownership tuple.
7. If neither contains it and transport evidence says no earlier write may have committed, persist `ADD_INTENT` and call `thread/queue/add`.
8. Persist the returned remote submission ID and internal `REMOTE_QUEUED` (public `thread_queued`).
9. Follow task events and periodically reconcile nonterminal state.

## Idempotency and the exactly-once gap

Local scheduling is idempotent. Remote insertion currently is not: app-server allocates a fresh queue UUID for every add, while `clientUserMessageId` is preserved but not uniqueness-constrained. The implementation can therefore be duplicate-resistant but cannot prove exactly-once delivery.

The client ID is a cryptographically random 128-bit value, generated and durably stored with the occurrence before network I/O. Predictable IDs are avoided because the queue is visible to other app-server clients. Reconciliation requires the full ownership tuple and treats any input-digest mismatch as a security incident.

On a lost response:

1. list the task queue for client ID plus exact canonical input digest and target;
2. read bounded task history for the same ownership evidence;
3. reconcile if either proves acceptance;
4. wait a bounded grace period if handoff may still be persisting; and
5. otherwise mark `DELIVERY_UNCERTAIN` without automatic resend.

The non-idempotent insertion is visible in the [current queue-row implementation](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/state/src/runtime/queued_items.rs#L77-L104). True exactly-once behavior requires an upstream unique constraint and atomic queue-to-history handoff.

## Recovery

On daemon start:

1. acquire the exclusive local OS lock and record a new daemon generation;
2. run a pre-migration integrity check;
3. take a consistent database/WAL backup;
4. apply transactional migrations and run a post-migration integrity check;
5. quarantine prior-generation in-flight mutations and reconcile them before new sends;
6. establish clock trust; materialize bounded overdue catch-up only afterward;
7. connect, initialize, and classify the tested app-server;
8. resume tasks with nonterminal work;
9. reconcile queue and history by complete ownership tuple; and
10. schedule the earliest future deadline.

On disconnect, preserve all states, reconnect with jittered backoff, and reconcile before any new mutation. Socket loss never implies that a turn failed or completed.

On clock movement, recompute against wall time without moving interval anchors. Backward jumps do not repeat a nominal occurrence because of the unique key.

## Approvals and task settings

Queued submissions currently carry prompt input and client message ID, not per-turn working-directory, model, sandbox, approval reviewer/policy, permission profile, or instruction snapshot. A scheduled turn therefore inherits the task’s current settings, including later human changes.

Threadbeat:

- records an authority fingerprint at human authorization and compares exposed effective settings before every add/start;
- pauses on missing or changed authority fields and refuses unapproved `auto_review`;
- watches queued work for later settings drift and attempts exact-owned cancellation;
- never changes sticky task settings merely to run an automation;
- never responds to approval or user-input server requests;
- records `needs_attention` from active flags/events;
- coalesces later occurrences while blocked; and
- reports the inherited-settings constraint and fingerprint state in preview/get/doctor responses.

There remains an upstream race between the final settings check and automatic queued-item start because queue admission does not atomically bind an authority snapshot. A human-owned task is less reproducible and less strongly isolated than a dedicated automation task. The user must explicitly accept this limitation, choose dedicated tasks, or wait for upstream support; Threadbeat cannot claim it has eliminated the race.

## Service architecture

### Raspberry Pi/Linux

Default: a systemd user service with lingering for a single-user device. The process runs in the foreground with `Restart=on-failure`, explicit state/runtime directories, empty capabilities, restricted address families, `NoNewPrivileges`, core dumps disabled, narrowly bound app-server/SSH paths, rate-limited redacted journald output, and credentials loaded from systemd facilities rather than command arguments. Same-UID compromise is considered full compromise; a dedicated account is the stronger deployment mode.

### macOS

Default: per-user LaunchAgent, foreground process, SIGTERM handling, Keychain-backed secrets, and a private application-support state directory.

The Rust `service` subcommand renders units/plists, prints the exact changes, applies them only after explicit approval, and then runs `doctor`. Shell wrappers may invoke platform service managers, but scheduling and MCP logic remain Rust.

## Compatibility and upgrade policy

- Define support by a semantic tested released-binary allowlist, not a guessed minimum version or method probe.
- Generate and hash the schema from every tested Codex binary.
- Probe queue methods after initialization on every daemon connection, but do not treat that as semantic proof.
- Ignore additive fields; fail on missing required IDs and unknown safety-critical discriminators.
- If an upgrade removes strict capability, pause schedules and preserve all local state.
- Database migrations are forward-only with backup/rollback instructions; the service never auto-downgrades a database.
- Plugin upgrade is separate from Codex app-server upgrade.
