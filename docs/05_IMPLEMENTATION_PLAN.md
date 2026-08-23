# Implementation plan

Status: proposed sequence after architecture approval

## Delivery strategy

Build risk-first. The first code is not the recurrence engine or UI assets; it is a small compatibility spike proving that the exact supported app-server can accept, persist, start, and correlate a deterministic queued message in an existing task. If that fails, the product promise changes before a large scheduler is built.

Each phase ends with a reviewable commit series and an explicit exit gate. Main remains buildable; commits should be small enough to explain one design choice or invariant.

## Phase 0: review and freeze v1 decisions

Decide:

- product name and stable plugin/service/binary identifiers;
- acceptance of the experimental strict queue dependency;
- weekly catch-up default;
- supported remote topology for v1; and
- whether standalone project jobs/notifications are required in MVP or explicitly post-MVP.

Outputs:

- accepted ADRs for queue adapter, interval semantics, ambiguity policy, and task authority inheritance;
- versioned tool contract;
- threat model sign-off;
- release targets and license choice.

Exit gate: no unresolved decision changes persistent schema, authority, or advertised guarantee.

## Phase 1: repository and protocol compatibility spike

### Work

- Create the Rust workspace and pin toolchain/MSRV.
- Add formatting, Clippy, unit-test, audit, deny, and lockfile CI.
- Generate JSON schema from the exact installed/supported Codex binaries.
- Extract minimal tolerant DTOs for initialize, task resume/read, queue add/list/start, task status, user-message client ID, and turn lifecycle.
- Implement WebSocket framing over Unix socket.
- Implement one optional SSH-proxy byte-stream adapter.
- Add a fake app-server harness that records/reorders/drops frames.
- Queue a deterministic marker into a disposable existing task, observe it start only when idle, and correlate `userMessage.clientId` through completion.
- Prove restart persistence of the app-server queue.
- Deliberately lose the `queue/add` response and measure what queue/history evidence remains.
- Deliberately remove experimental capability and prove fail-closed behavior.

### Commits

Examples of intended granularity:

1. `build: initialize pinned Rust workspace`
2. `test: add app-server protocol fixture harness`
3. `feat: initialize app-server over Unix WebSocket`
4. `feat: probe strict thread queue capability`
5. `test: correlate queued client message to completed turn`
6. `docs: record compatibility spike results`

### Exit gate

- Strict queue behavior works against at least one released Codex binary, not only a source build/alpha.
- The exact version/schema digest is recorded.
- No direct-start race is presented as strict.
- Ambiguous-delivery behavior matches the documented conservative policy.

If no released binary has the necessary queue capability, ship status remains technical preview or implementation pauses for an upstream API decision.

## Phase 2: pure schedule engine

### Work

- Implement versioned `once`, anchored `interval`, and structured `weekly` rules.
- Normalize/sort/deduplicate weekly groups.
- Implement IANA timezone resolution and configurable gap/fold behavior.
- Implement pure next/between functions with strict expansion caps.
- Implement catch-up, coalescing, expiry, and fixed-rate anchor preservation.
- Add property and golden tests covering leap years, month/year boundaries, multiple zones, DST gaps/folds, Lord Howe’s half-hour transition, forward/backward wall-clock jumps, and tzdb changes.

### Exit gate

- The schedule engine has no I/O or daemon dependencies.
- Property tests show no interval drift and no duplicate nominal instant.
- The user’s M–Th at 10:00 and 12:00 example normalizes to eight correct weekly slots.

## Phase 3: SQLite ledger and scheduler daemon

### Work

- Add forward-only migrations for profiles, schedules, occurrences, attempts, events, request keys, and leader lease.
- Implement single DB actor, permissions, WAL, busy timeout, integrity checks, and critical full-synchronous transitions.
- Implement optimistic schedule revisions and request-key idempotency.
- Implement one-deadline scheduler, bounded due-batch materialization, worker claims, per-task lanes, and two-worker dispatcher.
- Implement state machine guards so invalid transitions cannot be persisted.
- Implement startup recovery, claim expiry, clock-jump audit, and retention.
- Add deterministic crash injection after every state transition.

### Exit gate

- Kill/power-loss simulations cannot create two local occurrences for one nominal key.
- A second daemon cannot schedule the same database.
- A Pi-sized workload meets preliminary database/write budgets before app-server traffic is added.

## Phase 4: strict app-server delivery adapter

### Work

- Connect the scheduler outbox to `queue_v1`.
- Reconcile deterministic client IDs in queue and task history before mutations.
- Correlate remote queue submission, user message, turn ID, terminal event, interruption, approval, and user-input state.
- Implement bounded overload/reconnect backoff.
- Implement `delivery_uncertain`, explicit retry lineage, and duplicate-risk acknowledgment.
- Implement queue pause/resume semantics after interruption.
- Prevent future preloading and enforce one remote-pending heartbeat per automation.
- Record sanitized protocol diagnostics and capability changes.

### Exit gate

- Lost responses, reordered/duplicate notifications, app-server restarts, queue saturation, interruption, approval waits, and history gaps pass the fake-server suite.
- A real two-client test proves that strict delivery does not steer into an active human turn.
- Loss of queue capability pauses work and never invokes `turn/start` silently.

## Phase 5: MCP surface and CLI

### Work

- Implement the five tools in `03_MCP_TOOL_CONTRACT.md` using exactly pinned `rmcp` features.
- Generate input and output schemas from Rust types and snapshot them.
- Propagate `_meta.threadId` only as a Codex convenience; retain explicit target fallback.
- Enforce quotas, validation, idempotency keys, optimistic revisions, and stable error codes.
- Implement structured content plus concise human-readable summaries.
- Implement the same service API in `threadbeat ctl` for recovery when no agent is available.
- Add tool-selection fixtures for common natural-language requests and adversarial stored data.

### Exit gate

- JSON Schema snapshots match the approved contract.
- Repeated tool mutations are idempotent.
- Read tools cannot mutate state.
- No tool can alter endpoint, credentials, service, or permission profile.
- An agent can create and verify both required schedule examples without raw RRULE.

## Phase 6: service management and plugin packaging

### Work

- Implement `service plan` to render exact systemd/launchd changes without applying them.
- Implement explicitly authorized install/upgrade/uninstall and rollback.
- Validate clock, paths, file modes, existing app-server reachability, initialize, strict queue capability, target task read/resume, and database durability.
- Package `.codex-plugin/plugin.json` and direct-map `.mcp.json`.
- Write `threadbeat-automations` skill with minimal workflow/tool guidance.
- Write `threadbeat-setup` skill with install, topology selection, doctor, upgrade, rollback, and removal steps.
- Create prebuilt release selection and verified source-build fallback.
- Generate/review composer icon, light/dark logo, and three screenshots after the name is frozen.
- Run plugin and skill validation/evaluation tooling.

### Exit gate

- Clean Pi install reaches a running, enabled, and verified service.
- Disabling/removing the plugin does not orphan an unexplained service; setup documentation distinguishes plugin removal from service removal.
- Upgrade preserves schedules and can roll back the binary if migration has not advanced incompatibly.
- Manifest references exist, paths are relative and portable, images render at target sizes, and no secret is packaged.

## Phase 7: adversarial security and Pi performance validation

### Security work

- Attempt cross-user and same-user socket access.
- Test malicious automation names/prompts/errors against later list/get interactions.
- Test endpoint/path/credential injection through every MCP string.
- Verify secret redaction in normal, debug, panic, and diagnostics paths.
- Verify SSH host-key/certificate/auth failures are closed, legible, and non-fallback.
- Review systemd/launchd plans for privilege and path overreach.
- Run RustSec, cargo-deny, SBOM, provenance, and artifact-signature checks.

### Performance/reliability work

- Native arm64 Pi benchmark with 1,000 active schedules and a 50-item due burst.
- Measure idle/active RSS, CPU, FDs, timer count, DB/WAL/log growth, and bytes written per occurrence.
- Seven-day minimum soak for release candidate; 30-day target for stable claim.
- Reboot, app-server restart, SSH flap, DNS/Tailscale change, NTP loss/correction, low disk, WAL recovery, and log rotation tests.
- Compare current-thread vs small multi-thread Tokio runtime and bundled vs system SQLite only with evidence.

### Exit gate

All security requirements and release-blocker budgets in `04_SECURITY_PERFORMANCE.md` pass or the design/budget is revised honestly before release.

## Phase 8: release and operations

### Work

- Build native/cross artifacts, then run native smoke tests.
- Publish checksums, signatures, SBOM, provenance, compatibility matrix, schema digests, and release notes.
- Mark experimental strict-queue dependency prominently.
- Document backup, restore, redacted support bundle, and upstream break-glass procedure.
- Track upstream stabilization/idempotency work and add tagged Codex versions only after the full matrix passes.

### Exit gate

The release definition in the PRD passes on a clean Pi and one macOS host. No claim exceeds the tested evidence.

## Test matrix

| Layer | Required tests |
|---|---|
| Schedule | examples, property tests, fuzzing, DST/leap/clock golden cases |
| Database | migration, power-loss, unique keys, leases, corruption, retention |
| Protocol DTO | generated-schema diff, additive fields, missing IDs, unknown states |
| Fake app-server | overload, disconnect, lost response, reordering, duplication, interruption, approvals |
| Real app-server | each supported binary × UDS/SSH × restart × active-task race |
| MCP | schema snapshots, conformance, idempotency, tool-selection scenarios |
| Plugin/skills | manifest validation, clean install, setup rollback, skill evaluation |
| Pi | native smoke, resource burst, soak, SD writes, reboot, clock/network faults |

## Commit and review policy

- Commit after each coherent invariant or evidence milestone; do not combine protocol, scheduler, and packaging changes in one commit.
- Reference the relevant requirement/ADR in commit bodies when behavior changes.
- Keep generated schema/fixtures separate from hand-written logic.
- Never rewrite or squash away a review finding until the review gate is accepted; follow-up commits show remediation.
- Before each phase merge: format, Clippy with warnings denied, unit/integration tests, dependency checks, `git diff --check`, and a concise evidence note.

## Likely upstream contributions

The cleanest long-term improvements belong upstream in Codex:

1. make `thread/queue/*` stable and formally capability-described;
2. add an idempotency uniqueness constraint on `(threadId, clientUserMessageId)`;
3. make queue-row-to-durable-user-message handoff atomically recoverable;
4. expose per-queued-turn settings where safe; and
5. publish/version an external Rust app-server protocol client.

Threadbeat must not wait on those changes to prototype, but it must not claim guarantees they are required to provide.
