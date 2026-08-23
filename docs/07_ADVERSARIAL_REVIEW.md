# Adversarial review and disposition

Review date: 2026-08-22

Status: historical review record. Dispositions describe target controls unless `10_IMPLEMENTATION_STATUS.md` confirms their implementation.

Reviewers:

- independent security/privacy adversary;
- independent Raspberry Pi performance/reliability adversary; and
- primary-agent source verification against the local generated schema and `openai/codex` snapshot.

Both independent reviewers read the full initial packet and returned **NO-SHIP for full MVP implementation**. They allow only a bounded, disposable Phase-1 compatibility/failure-semantics spike plus isolated pure schedule/database prototypes. The original proposal remains visible in Git history at `78889fe`; remediation is a follow-up rather than a rewritten review record.

## Product manager versus software architect debate

Before the independent reviews, the product and architecture roles were explicitly asked to challenge each other. Their material disagreements and resolution were:

| Question | Initial product position | Architecture challenge | Resolution before/after adversarial review |
|---|---|---|---|
| Meaning of “every 30 minutes” | Initially completion-relative to avoid backlog | Users expect a fixed wall-time grid; completion-relative silently drifts | PM reversed: anchored fixed-rate is default; `fixedDelay` may be a separate future type. |
| Delivery path | Direct turn start was initially considered | `turn/start` can steer into an active turn; only current queue path has idle-atomic admission | Strict mode requires capability-gated `thread/queue/*`; no silent direct fallback. |
| One queue or two | Scheduler queue was the main product abstraction | App-server already has a durable per-task delivery queue, but no calendar semantics | Two named queues: Threadbeat owns time/occurrences; app-server owns due-prompt admission. |
| Missed intervals | Replay could preserve every nominal event | A Pi restart or long run can produce a storm | Default heartbeat coalesces to one latest catch-up within 24 hours; `all` is bounded/advanced. |
| Tool count | First draft proposed many CRUD operations, then one polymorphic mutation tool for parity | A small catalog helps selection, but reads/diagnostics deserve their own shape | Final implementation has six tools: the agent explains the schedule itself, calls `automation_update` directly, and keeps risky run control separate. |
| Confirmation | Universal two-phase confirmation was considered | Clear reversible schedule requests should not require ceremony | PM dropped universal confirmation; security review replaced ceremony with a real policy: host/CLI-verifiable grants by default, with explicit current-task trusted-agent opt-in. |
| Stable fallback | A read-then-start fallback looked possible | It is a TOCTOU race and can steer into human work | Best-effort direct mode is not exposed in MVP. |
| Exactly once | Desired as a scheduler quality goal | Upstream add is non-idempotent and queue-to-history handoff has a crash gap | Advertise durable, reconciled, duplicate-resistant delivery only; uncertainty stops replay. |
| Resource targets | Tight Pi targets should be product requirements | Dependency and storage costs are not yet measured | Keep targets as test gates, not promises; Phase 1/7 measure and revise honestly. |

The largest unresolved product/architecture tradeoff is now authority, not recurrence: same-task continuity inherits mutable task settings, while dedicated scheduler-owned tasks are safer but do not satisfy the desired default experience.

## Blockers and disposition

### B1: no enforceable caller/task authorization boundary

Finding: any MCP caller could supply an arbitrary target task, globally read schedules/prompts, and create persistent work. A tool call plus prose about explicit intent is not proof of human authorization.

Disposition: **accepted with a two-mode consent design**.

- MCP v1 must be scoped to a Phase-1-proven host-validated originating task from `_meta.threadId`; until then it is a candidate binding, not an established fact.
- A raw caller-supplied `targetThreadId` cannot grant cross-task access.
- Cross-task targeting and global administration are CLI-only until a host supplies an authenticated scoped target capability.
- MCP list/get/runs are scoped to the originating task and service principal.
- Default activation policy is host-confirmed: active create, run-now, uncertain replay, interruption start, and destructive pending-work deletion require a server-verifiable consent grant delivered by supported host elicitation/approval or recorded by interactive `threadbeat --config <path> ctl authorize --thread-id <id> [--minutes <1-1440>]`.
- A model-set Boolean is never consent.
- If the host cannot carry a verifiable grant, creation is paused/suggested by default.
- A personal-use `trusted_agent_current_task` policy may be enabled explicitly during setup. It accepts the host-authorized tool call as consent but remains current-task-only and clearly documents prompt-injection risk.

Phase 1 must establish which consent/elicitation metadata the target Codex MCP client actually supports before the public mutation schema is frozen.

### B2: inherited task authority can drift after scheduling

Finding: queued work inherits the task’s current cwd, sandbox, approval policy/reviewer, permission profile, and instructions. Those may become more permissive after creation; `auto_review` can resolve approvals without the human. Checking only when a schedule is created is insufficient.

Disposition: **accepted, with an acknowledged upstream atomicity gap**.

- Activation records an authority fingerprint: normalized cwd, sandbox, approval policy, approval reviewer, permission-profile identity/provenance, and relevant instruction-source identity when exposed.
- Before queue add and explicit queue start, Threadbeat reads effective settings and compares them with the authorized fingerprint.
- Missing or changed fields pause delivery as `authority_changed` until reauthorized.
- Unattended schedules reject `approvalsReviewer=auto_review` unless the human grant is bound to that exact fingerprint.
- While an owned item waits remotely, Threadbeat watches settings and tries to delete it on drift.

This still has a race: current app-server does not atomically bind a queued prompt to an authority snapshot. Same-task strict safety therefore cannot be guaranteed against a settings change between the last check and automatic start. Phase 1 must measure the race, and the product must choose one of:

1. accept this documented inherited-authority risk for same-task continuity;
2. require dedicated scheduler-owned tasks for stronger isolation; or
3. wait for upstream per-queued-turn authority snapshots/conditional admission.

No document may claim that Threadbeat can fully prevent authority drift with the current API.

### B3: shared queue ownership is forgeable if correlation uses a predictable ID alone

Finding: another app-server client can observe/copy a client ID or mutate/delete a queued row. Matching a predictable UUIDv5 client ID alone can falsely accept an attacker’s prompt as Threadbeat’s.

Disposition: **accepted**.

- Each occurrence receives a cryptographically random 128-bit client message ID, generated and durably stored before network I/O.
- Ownership tuple is `(profile, target task, occurrence, random client ID, canonical input digest, remote submission ID when known)`.
- Reconciliation verifies client ID, exact canonical input digest, and target. Any mismatch is `remote_ownership_violation`, not success.
- Threadbeat updates/deletes/starts only a row matching the complete owned tuple.
- Threadbeat never reorders the shared queue.
- Every remote mutation—not only add—uses a durable intent and ambiguity protocol.

### B4: interruption is task-wide, not schedule-local

Finding: the app-server task queue is shared across schedules, humans, and other clients. A schedule-level “resume after interruption” can misrepresent or disturb shared state.

Disposition: **accepted with a source correction**.

The reviewer stated `thread/queue/start` could not target a queue item. Current source actually defines optional `queuedSubmissionId` and selects that exact row when provided. This avoids accidentally starting a different row, but it can still bypass the shared FIFO order and the interruption blocker remains task-wide.

- Remove automation-scoped `resume_after_interruption`.
- Offer a risk-separated `start_blocked_run` only for an exact run and exact owned remote submission.
- Never call queue start automatically or with an omitted submission ID.
- By default, start only if that owned row is the current queue head, the task is idle, no approval/input blocker exists, authority matches, and a valid human consent grant is present.
- Otherwise report the shared queue blocker and require the user to resume/order work in Codex.
- All schedules targeting that task remain locally blocked/coalescing until the task-level condition clears.

### B5: an expiring database leader lease does not fence an old network writer

Finding: a paused/stale leader can still have an in-flight non-idempotent queue mutation after another process assumes a lease.

Disposition: **accepted**.

- The database must reside on a local filesystem; network/shared filesystems are unsupported.
- Hold an exclusive OS file lock for the entire daemon lifetime. A second process does not take over while the first still owns it.
- Record a monotonically increasing daemon generation in claims and mutation intents.
- On process replacement, quarantine every prior-generation in-flight mutation and reconcile before any new remote mutation.
- Test SIGSTOP/SIGCONT, process kill at frame boundaries, graceful drain, supervisor restart, and already-sent frames that may commit after local death.

No client-side fence can stop a request already accepted by a remote server; conservative ambiguity remains necessary.

### B6: an untrusted Pi boot clock can cause “early” execution

Finding: a Pi without a trustworthy RTC can boot far in the future, materialize work, then correct backward after NTP. Unique IDs prevent duplication but not premature dispatch.

Disposition: **accepted**.

- Delivery is disabled while clock state is `untrusted` or `quarantined`.
- Default Pi trust source is confirmed system time synchronization; an explicit hardware-RTC or human override policy may be configured.
- Large corrections cross a configurable threshold into quarantine rather than merely creating a log entry.
- Persist last trusted wall/monotonic observations for diagnostics, while stating they cannot prove an initially bad future clock.
- Test offline/no-RTC boot, delayed sync, step, slew, suspend/resume, and manual jumps.

### B7: occurrence identity collapses DST collisions and can duplicate across revisions

Finding: two logical weekly slots may resolve to one UTC instant under `next_valid`; fold-both needs an ordinal; revision in the unique key permits an old and new revision to emit the same logical slot.

Disposition: **accepted**.

- Canonical weekly slots receive stable IDs.
- Occurrence identity includes schedule, logical slot, intended local datetime, and resolution ordinal; resolved UTC is an attribute, not the sole identity.
- The unique logical-occurrence constraint spans schedule revisions.
- Revision updates have an atomic cutover instant. New revisions cannot rematerialize an existing logical slot.
- Pre-remote old work is superseded; remotely queued/running old work remains bound to its immutable prompt/revision unless an explicit owned-row cancellation succeeds.
- Colliding resolved instants remain distinct logical occurrences and are serialized/coalesced according to policy.

### B8: queue-to-history recovery is unproven on a released binary

Finding: the local alpha/source snapshot shows the desired methods, but method presence does not prove persistence, idle admission, interruption, or client-ID recovery across every crash point. Queue deletion precedes a fully proven durable history correlation.

Disposition: **accepted; this is the implementation gate**.

- Only a disposable Phase-1 spike may begin.
- Test an actual released binary through kill points before/after insert, response, idle reservation, Core acceptance, queue deletion, user-message persistence, turn start, and terminal persistence.
- Define history pagination/retention bounds and when absence can ever be considered authoritative.
- Treat post-send failure as ambiguous unless transport evidence proves the frame was never accepted.
- Publish compatibility results and observed uncertainty before persistent schema/tool/service implementation.

## High findings and resulting design changes

| Finding | Change |
|---|---|
| Per-schedule remote cap can fill a shared 100-item task queue | One Threadbeat-owned remote-pending item per target task; conservative external-depth threshold; local fair queue. |
| Coalescing was prose, not a transition contract | Add normative examples and event/guard/action tables in `08_STATE_AND_TIME_SEMANTICS.md`. |
| State machine omitted cancel races, low disk, closure, external mutation | Add explicit internal states, mutation intents, terminality, and uncertainty outcomes. |
| App-server availability is outside Threadbeat ownership | Promise schedule durability and reconnect after the separately managed app-server returns, not app-server uptime. |
| Disk full after remote acceptance can erase acknowledgment | Free-space admission threshold, preallocated emergency reserve, no-new-mutations mode, startup reconciliation. Residual media-failure ambiguity remains. |
| Retention was inconsistent with stress workload | Bound by bytes and rows, report effective time horizon, define prompt/request-key/audit erasure. |
| Capability probing is semantically weak | Maintain a tested released-binary allowlist; unknown builds are blocked. |
| MCP facade and daemon may be different versions | Alpha rejects a fixed protocol-version mismatch; feature negotiation and drain/restart/rollback rules remain target design. |
| Resource budget omitted latency | Add dispatch/recovery SLO candidates and measure before promising. |
| Linux target lacked ABI matrix | Freeze Pi model/OS/glibc/filesystem matrix after the dependency spike; test oldest target natively. |
| Full prompts were returned/stored too broadly | MCP returns hash/length/redacted preview by default; full-body access is separately scoped; no prompt/result copy in idempotency rows. |
| Same-UID socket protection was overstated | Treat same-UID compromise as full compromise; dedicated account is the stronger mode; verify peer UID, inode, and parent directories. |
| WSS was not a native tested app-server mode | Remove direct WSS from v1. Support same-host UDS and SSH-proxied app-server only. |
| Artifact lifecycle was incomplete | Sign whole bundle, pinned trust root/provenance, versioned absolute paths, plan digest, atomic apply, downgrade protection, safe uninstall. |
| Polymorphic tool mixed incompatible risk annotations | Keep ordinary schedule mutation in `automation_update`, separate run control/risky recovery, and render the schedule conversationally without a preview tool. |
| Boolean acknowledgments were model-forgeable | Replace with server-verifiable host/CLI consent grants. |
| Request-key semantics were vague | Scope by principal/operation, version canonicalization, store hash plus stable IDs, define expiry/tombstones. |
| Input/log display spoofing | Normalize printable names; reject controls, terminal escapes, and bidi overrides; never interpolate raw untrusted values into trusted advice. |

## Tool-catalog decision after review

The PM favored one polymorphic mutation tool to mirror current Codex and reduce tool-choice ambiguity. The package keeps ordinary schedule changes in that tool, removes the separate preview tool, and retains a distinct run-control tool for higher-risk immediate and recovery operations.

Revised catalog:

1. `automation_update` — create/suggested-create/update/pause/resume/soft-delete after the agent explains the normalized schedule.
2. `automation_run` — run-now/cancel/retry/start-blocked, with stricter consent policy.
3. `automation_get` — current-task metadata, prompt redacted by default.
4. `automation_list` — current-task summaries.
5. `automation_runs` — current-task lifecycle history.
6. `automation_doctor` — sanitized read-only health.

Purge, cross-task/global administration, endpoint/profile changes, service changes, and full secret-bearing diagnostics remain CLI-only.

## Required Phase-1 spikes

1. Released app-server kill-point and history-correlation matrix.
2. Mixed-client shared-queue ownership, interruption, exact-ID start, external update/tampering, delete/start ambiguity, duplicate-match, and race matrix. Threadbeat does not use remote queue update in MVP.
3. Effective authority-field completeness, change observation, auto-review behavior, and check-to-start race measurement.
4. MCP host identity, `_meta.threadId`, elicitation/approval, and server-verifiable consent mechanisms.
5. Native Pi clock-trust campaign.
6. SQLite power/disk-full/WAL/checkpoint campaign and exclusive-lock pause/takeover tests.
7. Narrow `rmcp` stdio oversized-frame test; wrap with a bounded transport if the pinned SDK buffers unbounded lines.
8. Dependency/resource spike and minimum supported Pi ABI decision.
9. Secure SSH forced-command/proxy validation.
10. Plugin/service removal behavior and availability of pre-removal hooks.

## Closure review after remediation

Both adversaries re-read the revised packet.

Security closure:

- caller/task authorization: **partially closed**, pending proof that the host can integrity-bind `_meta.threadId` and issue a server-verifiable consent grant;
- inherited authority drift: **open**, because current queue items do not atomically carry an authority snapshot;
- shared-queue ownership: **partially closed**, with random IDs/digests/exact-ID operations specified but duplicate identical rows and released behavior still requiring tests;
- unsafe tool semantics, request-key privacy, and supply-chain design: **closed in design**;
- prompt at-rest exposure, SSH confinement, removal hooks, resource ceilings, and remote-mutation recovery: **partially closed/pending evidence**.

Reliability closure:

- stale leader fencing: **closed in design** by lifetime OS lock plus generation quarantine;
- per-task capacity/fairness, coalescing, app-server uptime boundary, retention, and local control-version skew: **closed in design**;
- interruption, clock trust, state machine, low-disk behavior, latency, and occurrence identity: **partially closed/pending native or released-binary evidence**;
- queue-to-history recovery, semantic build support, and minimum Linux ABI: **open gates**.

The closure pass also found and this revision fixes several contradictions: `_meta.threadId` is now called a candidate binding until proven; duplicate matching rows are never authoritative; remote queue update is removed from MVP rather than left without an intent state; public lifecycle vocabulary is centralized in the tool contract; retry uses a verifiable consent grant rather than an acknowledgment Boolean; v1 SSH requires a constrained forced-command account/key; cross-revision execution uses a universal resolved-instant exclusion; and database recovery orders integrity check/backup before migration.

## Gate verdict

The architecture is promising enough for evidence-gathering, not for implementation of persistent user data or unattended execution. Phases 2–6 remain blocked until B1–B8 have either:

- a tested, normative solution; or
- an explicitly accepted product limitation that is accurately reflected in tools, setup, and release claims.
