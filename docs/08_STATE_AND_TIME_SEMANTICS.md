# Normative state and time semantics

Status: revision prompted by adversarial review; final schema remains gated on Phase 1 evidence

## Terminology

- **Schedule:** mutable user intent and recurrence rule.
- **Schedule revision:** immutable snapshot of name, prompt, rule, policies, and authority fingerprint.
- **Logical occurrence:** one intended slot from a schedule, independent of when it is delivered.
- **Run:** the lifecycle record for one logical occurrence or explicit retry/manual child.
- **Remote queued item:** a due prompt accepted into the shared Codex task queue.
- **Task lane:** Threadbeat’s local fair/serialized admission lane for all schedules targeting one task.
- **Clock trust:** whether wall time is safe enough to materialize/deliver time-based work.

## Clock-trust state machine

```text
UNTRUSTED -- trusted sync/RTC proof --> TRUSTED
TRUSTED -- correction over threshold --> QUARANTINED
QUARANTINED -- stable trusted observations + policy --> TRUSTED
any state -- explicit human override grant --> TRUSTED_OVERRIDE
```

Rules:

- No occurrence is materialized as due or sent remotely in `UNTRUSTED` or `QUARANTINED`.
- Schedule edits and previews remain available; previews are labeled using the untrusted local observation.
- Default correction quarantine threshold is a product configuration to validate on Pi; the initial test candidate is five minutes.
- A human override is audited, expires, and is bound to the observed wall time and host boot ID.
- A monotonic timer is used only after a trusted wall deadline has been computed. On wake, wall time is revalidated before delivery.
- Backward movement never recreates a logical occurrence. Forward movement applies bounded catch-up only after trust is restored.

## Logical occurrence identity

### Once

```text
(schedule_id, kind=once, declared_instant)
```

### Interval

```text
(schedule_id, kind=interval, anchor, ordinal_n)
```

where the nominal instant is exactly `anchor + n × everyMinutes`.

### Weekly

Each canonical `(weekday, local_time)` has a stable slot ID derived from the normalized schedule series. Identity is:

```text
(schedule_id, kind=weekly, slot_id, intended_local_date_time, resolution_ordinal)
```

`resolution_ordinal` distinguishes first/second fold occurrences and logical gap resolutions. Resolved UTC time is also claimed through a universal execution-exclusion key `(schedule_id, resolved_nominal_instant)` that spans revisions and schedule kinds. Two logical slots that resolve to the same instant are recorded in collision metadata but coalesce into one executable run in MVP; the preview warns. This avoids both revision duplicates and ambiguous simultaneous logical slots.

The database has both a unique logical-occurrence constraint and the universal resolved-instant exclusion across revisions. A schedule revision is stored on the row as the immutable payload/config snapshot that won materialization, not as a way to bypass uniqueness.

## Revision cutover

An update transaction records `cutover_at` and creates a new immutable revision.

- Running work remains bound to the old revision.
- Remotely queued old work remains bound to the old revision unless an explicit exact-owned deletion succeeds.
- Pre-remote future old work at or after cutover is marked `superseded`.
- The new revision computes occurrences strictly after cutover.
- If a new rule maps to an already materialized logical slot/instant, the unique constraint prevents a second run and returns a warning.
- Prompt-only edits do not rewrite already remote-queued input.
- Schedule delete is soft; it prevents new materialization and attempts exact-owned cancellation only when the authorized operation requested it.

## Catch-up and coalescing

Default heartbeat policy:

```json
{"mode":"coalesce","maxAgeMinutes":1440,"maxRuns":1}
```

Normative rules:

1. A coalesced catch-up represents the newest missed logical slot plus `missedCount`, earliest missed time, and latest missed time.
2. The replayed user prompt remains the configured prompt; catch-up metadata is app-server input metadata only if the protocol safely supports it, otherwise it is kept in Threadbeat history and a short trusted preamble may be used only if approved in the tool contract.
3. A schedule has at most one local coalesced marker.
4. A target task has at most one Threadbeat-owned remote queued item.
5. When an existing run/remote item blocks a new slot, update the local coalesced marker transactionally; do not enqueue another remote row.
6. When the blocker clears, compare the marker with the next on-grid slot:
   - if the on-grid slot is already due, one run represents the newest eligible slot and absorbs the marker;
   - otherwise deliver the marker once, then preserve the original future interval grid.
7. `all` creates distinct local occurrences up to both count and age caps, but task-lane fair scheduling permits at most one run per schedule per round so a backlog cannot starve another schedule.
8. `skip` records skipped summary metadata and advances without a remote mutation.

Examples:

| Event | Result |
|---|---|
| 10:00 run continues through 10:30; 30-minute interval | 10:30 becomes one coalesced marker. |
| Run ends 10:52; next grid slot 11:00 | Deliver one catch-up if admission is safe; grid remains 11:00, 11:30… |
| Run ends 11:03 with both 10:30 and 11:00 missed | One run represents newest 11:00 slot with `missedCount=2`; next grid is 11:30. |
| Daemon offline 8 hours under default | On trusted restart, one marker for the latest slot; older slots summarized. |
| Approval/user input blocks task | No more remote rows; local marker accumulates until the human resolves and authority still matches. |
| App-server disconnected | Due work remains local; catch-up policy is applied when connectivity and clock trust return. |
| Weekly gap with `skip` | Record skipped logical occurrence; no next-valid collision. |
| `all` backlog across three schedules | Round-robin one eligible occurrence per schedule per task-lane round. |

## Internal run states

```text
PLANNED
DUE
CLAIMED
RECONCILING
ADD_INTENT
REMOTE_QUEUED
START_INTENT
STARTED
WAITING_APPROVAL
WAITING_USER_INPUT
CANCEL_INTENT
DELETE_INTENT
SUCCEEDED
TURN_FAILED
DELIVERY_FAILED_RETRYABLE
DELIVERY_UNCERTAIN
INTERRUPTED
AUTHORITY_CHANGED
REMOTE_OWNERSHIP_VIOLATION
TASK_CLOSED
SUPERSEDED
CANCELLED
EXPIRED
SKIPPED
COALESCED
```

The public tool view maps implementation-only claim/intent states to `delivering` while retaining a machine-readable `phase` and stable exceptional state.

## Transition invariants

| Event | Preconditions/guards | Durable action before remote work | Outcome |
|---|---|---|---|
| Materialize due | trusted clock, unique logical key | insert immutable occurrence | `DUE` |
| Claim | daemon holds OS lock, current generation | record claim/generation | `CLAIMED` |
| Add | authority matches, task lane permits, queue depth safe, disk healthy | record `ADD_INTENT`, random client ID, input digest | reconcile then add |
| Add reply | ownership tuple matches | persist remote ID/time | `REMOTE_QUEUED` |
| Add transport failure after send may have begun | intent exists | no retry | `DELIVERY_UNCERTAIN` pending reconciliation |
| Queue row missing | search bounded queue/history | persist evidence | `STARTED`, terminal, or uncertain |
| Zero ownership matches | inspect transport certainty and bounded history | record evidence, no mutation | absent if never sent; otherwise uncertain |
| Multiple otherwise matching rows | multiplicity is not producer authentication | record sanitized evidence, stop | `REMOTE_OWNERSHIP_VIOLATION`/uncertain |
| External row input/ID mismatch | owned tuple fails | record sanitized evidence, stop | `REMOTE_OWNERSHIP_VIOLATION` |
| Start blocked run | exact owned row is head, task idle, no blocker, authority matches, human grant | record `START_INTENT` | one exact-ID start, never omitted-ID start |
| Ambiguous start | start frame may have committed | no retry | reconcile; otherwise `DELIVERY_UNCERTAIN` |
| Cancel remote queued | exact owned row, authorized, not started | record `DELETE_INTENT` | delete then reconcile |
| Delete reply lost | intent exists | no retry until list/history reconciliation | cancelled, too late, or uncertain |
| Explicit retry | original is retryable, exact consent grant, new request key | create immutable child with fresh random client ID and `retryOfRunId` | child `DUE`; original unchanged |
| Manual run-now | current revision/authority and exact consent grant | create nonce/request-key-bound logical child | `DUE` |
| Turn begins | correlated `userMessage.clientId` plus digest/target evidence | store turn ID/start time | `STARTED` |
| Approval/input flag | correlated task/turn | store blocker only | waiting state; never answer |
| Turn terminal | authoritative terminal event/history | store bounded status/error | `SUCCEEDED` or `TURN_FAILED` |
| Interruption | authoritative terminal event | block task lane | `INTERRUPTED` |
| Task closes/disappears | authoritative response/event | stop remote work | `TASK_CLOSED` |
| Disk/DB unhealthy | any pre-send state | enter no-new-mutations mode | local blocked state; no network send |

No remote mutation is automatically retried after a write may have begun. Every add, exact-owned delete, and exact-ID start has its own intent and reconciliation method. Threadbeat does not use remote queue update in MVP; schedule edits leave already queued input immutable.

## Shared queue ownership and fairness

- One Threadbeat-owned remote queued row per target task, regardless of schedule count.
- Local fair ordering is oldest eligible nominal time, with round-robin protection between schedules and stable ID tie-breaks.
- Observe remote queue depth before add. Initial conservative test threshold is 80 of 100; above it, keep work local. The final threshold is based on mixed-client tests.
- Never update, delete, start, or classify a row as owned from client ID alone. Threadbeat does not use remote update at all.
- Zero, multiple, or mismatched candidate rows are never authoritative success; a digest detects equality/tampering but does not authenticate the producer.
- Never reorder the shared queue.
- An external deletion/update/reorder of an owned row triggers reconciliation or ownership violation; it is not silently repaired.
- `thread/queue/start` is always called with the exact owned submission ID, never with `null`/omitted ID.
- Default policy refuses to start an owned non-head item, even though current source permits selecting it, because doing so would jump other clients’ FIFO order.

## Low-disk and database failure mode

- Refuse new schedules/occurrences and all remote mutations below a configured free-space threshold.
- Maintain a small fully allocated emergency reserve file whose only purpose is to free enough space to record a terminal health/uncertainty marker. This is mitigation, not a guarantee against device failure.
- If `SQLITE_FULL`, read-only media, fsync, checkpoint, or I/O errors occur, close admission, stop new sends, retain process memory only as diagnostic evidence, and require startup reconciliation after storage recovery.
- Report that a remote mutation may remain uncertain if durable media failed after the server accepted it.
- WAL byte limit, checkpoint backlog, DB byte cap, and free-space state are exposed in `doctor`.

## Retention

Retention is bounded by both age and bytes; the earlier bound wins.

Initial test candidates, not release promises:

- total Threadbeat state budget: 256 MiB;
- terminal run metadata: up to 90 days within that budget;
- detailed dispatch attempts: up to 30 days within that budget;
- request-key hashes/stable IDs: 30 days after last use, with a tombstone window disclosed to callers;
- soft-deleted prompt bodies: seven-day recovery window, then secure logical erasure;
- schedule audit metadata: bounded rolling history plus current/recent revision chain.

SQLite/flash cannot promise physical secure erasure of old pages/WAL. Sensitive deployments require encrypted storage and key destruction. `doctor` reports the effective retained time horizon under actual workload.

## Versioned local control protocol

The MCP facade begins every private-socket session with:

- protocol version;
- binary/plugin version;
- min/max compatible daemon version;
- feature set;
- principal and originating-task binding; and
- request/response size limits.

Incompatible facade/daemon versions fail with upgrade instructions. Upgrade drains the old daemon, preserves ambiguous intents, atomically changes a versioned absolute binary path, restarts, handshakes, and only then removes the previous binary.

## Candidate latency gates

These are measured and revised in Phase 1/7 before becoming promises:

- trusted, connected, idle task: p95 local due-to-add-intent ≤1 s; p99 ≤5 s;
- 50-item local burst across ten tasks: p95 intent creation ≤5 s and no task starvation;
- network restored: p95 reconnect/capability classification ≤30 s after reachability;
- 1,000-schedule database startup scan: p95 ≤10 s on the minimum supported Pi;
- queue-to-turn start has no Threadbeat SLO while another task turn/queue item blocks it; report measured lag separately.

## Deterministic test hooks

The implementation must inject:

- wall and monotonic clocks;
- timezone provider/version;
- scheduler step/wakeup driver;
- randomness/client-ID generator;
- database fault points at every transaction boundary;
- transport write boundaries and response loss;
- daemon generation/lock behavior; and
- fake plus patched real app-server kill points.

Model-based tests generate events and assert the transition invariants. Real process tests use pause/resume and kill, not only mocked exceptions.
