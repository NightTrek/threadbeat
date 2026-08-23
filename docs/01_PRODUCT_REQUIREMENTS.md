# Product requirements

Status: revised requirements; persistent MVP implementation is gated on the Phase-1 evidence in the adversarial review

## Product statement

Threadbeat lets a Codex agent create and manage durable scheduled follow-ups in the same task without depending on the Codex desktop application. It runs as a small resident service, connects to an already-running Codex app-server, and exposes an MCP surface plus agent guidance as one installable plugin.

The primary experience is conversational:

> Keep working on this every 30 minutes in this task.

> Run this at 10:00 and 12:00 every Monday, Tuesday, Wednesday, and Thursday.

The agent should translate those requests into normalized schedules, state the next run and timezone, and then manage them without showing raw recurrence syntax.

## Goals

1. Match the useful behavior of Codex desktop heartbeats: repeated proactive messages in the originating task, with task context preserved.
2. Work headlessly on a Raspberry Pi or other host, using an existing app-server daemon rather than spawning Codex for each run.
3. Preserve schedules and run state across daemon, app-server, host, and network restarts.
4. Make time semantics predictable: anchored intervals, explicit timezone, documented DST behavior, and bounded missed-event catch-up.
5. Keep the idle resource footprint small enough for a Pi and the SD-card write rate bounded.
6. Fail conservatively around approvals, ambiguous delivery, protocol incompatibility, and authority changes.
7. Package the MCP server, operating skill, setup skill, manifest, and visual assets as one plugin.

## Non-goals

- Reimplementing a model runtime or app-server.
- Guaranteeing an exact model start time or exactly-once external side effects.
- Running simultaneous turns inside one Codex task.
- Automatically accepting approvals or expanding filesystem/network access.
- Managing arbitrary remote endpoints or credentials through agent-callable tools.
- A graphical schedule manager in v1.
- Standalone per-run project automations in v1; the focus is same-task heartbeat behavior.

## Primary user

A technical Codex user who keeps a Codex app-server running locally or on a remote machine, reaches it over Unix socket or SSH, and wants conversational recurring work to continue when the desktop UI is absent. The initial deployment target is 64-bit Raspberry Pi OS.

## Core user journeys

### Create an interval heartbeat

1. The user asks for work every 30 minutes.
2. The skill chooses `heartbeat`, the current task, an anchored 30-minute interval, coalesced catch-up, and non-overlap.
3. The MCP validates and normalizes the request.
4. The response names the task, cadence, timezone if relevant, next nominal run, and catch-up behavior.
5. The daemon materializes each occurrence only when due and sends it to the app-server task queue.

### Create a weekly schedule

1. The user gives weekdays and one or more local times.
2. If timezone is not reliably available from configured context, the agent asks for it once.
3. The preview returns the exact weekday/time Cartesian product, upcoming UTC instants, and DST policy.
4. It is created active when the configured host/CLI consent policy is satisfied; otherwise it is saved paused with an exact approval path.

### Restart after missed work

1. The daemon starts, validates its SQLite database, and calculates missed nominal occurrences.
2. For a heartbeat, it creates at most one coalesced catch-up if the newest miss is no more than 24 hours old.
3. It reconciles the persisted random client ID, exact input digest, target, and any known remote ID against the app-server queue and bounded task history before making a network mutation.
4. It never produces a restart replay storm.

### Encounter an approval or interruption

1. A turn requests approval, user input, or is interrupted.
2. Threadbeat records `needs_attention` or `interrupted`, does not answer on the user’s behalf, and prevents additional heartbeats from piling up.
3. Later due occurrences coalesce.
4. The user resolves the task. An exact blocked run may be started only through the consent/ownership/head checks; Threadbeat then continues without shifting the original interval anchor.

### Inspect or change an automation

The user can list schedules, read normalized configuration and next run, see lifecycle timestamps and blockers, patch a schedule with optimistic revision protection, pause/resume, soft-delete, trigger now, cancel a not-yet-started occurrence, or explicitly retry an uncertain/failed occurrence.

## Functional requirements

### FR-0: caller scope and consent

- MCP operations must be scoped to a validated service principal and originating Codex task; Phase 1 must prove the host can supply that integrity boundary.
- `_meta.threadId` is the target binding, not a convenience that a raw argument may override.
- Cross-task targeting and global administration are CLI-only unless the host supplies an authenticated scoped target capability.
- Active creation, run-now, uncertain retry, blocked-run start, and destructive pending-work cancellation require a server-verifiable host/CLI consent grant under the default policy.
- If no supported human-routed grant mechanism exists, MCP creates a paused/suggested automation.
- A setup-time `trusted_agent_current_task` policy may accept agent tool calls as authorization only for the originating task; its prompt-injection risk is explicit.
- Names, prompts, titles, and errors returned by tools are untrusted data, never administrative instructions.

### FR-1: same-task targeting

- `heartbeat` is the default automation kind.
- The intended MCP target comes from a host-validated Codex `_meta.threadId` extension; until Phase 1 proves its authenticity, mutations remain blocked/paused.
- A caller-supplied raw task ID cannot expand scope. A daemon-issued/host-authenticated target capability may be added later.
- If the originating-task binding is absent, MCP creation fails and directs the user to the scoped CLI flow. Recent-task guessing is forbidden.
- The resolved target ID is returned and stored.

### FR-2: schedule types

MVP supports a closed tagged union:

- `once`: one RFC 3339 instant;
- `interval`: a whole-minute elapsed duration, anchored to activation or an explicit RFC 3339 instant; and
- `weekly`: one or more weekday/time groups in an IANA timezone.

“Every N minutes” is fixed-rate: occurrences are `anchor + n × interval`. Completion time never changes the future grid. A completion-relative `fixedDelay` type may be added later but is never inferred from ordinary interval language.

### FR-3: weekly expressions

- One group can express multiple weekdays and multiple times as their Cartesian product.
- Multiple groups can express irregular combinations.
- Local times are returned in normalized 24-hour form.
- Duplicate slots are rejected or deduplicated with an explicit warning.
- Raw RRULE is neither an input nor a user-facing output in MVP.

### FR-4: timezone and DST

- Weekly schedules require an IANA timezone such as `America/Los_Angeles`, never only `PDT` or `UTC-7`.
- Spring-forward default: skip a nonexistent local time.
- Fall-back default: run once at the first matching instant.
- The preview shows upcoming local and UTC instants and warns when one encounters a transition.
- Materialized occurrences retain intended local time, resolved UTC time, offset, timezone, and resolver version; future unmaterialized occurrences may reflect tzdb updates.

### FR-5: durable occurrence ledger

- Every logical occurrence has a stable identity that survives revision cutover and distinguishes weekly slot, intended local datetime, and DST resolution ordinal; resolved UTC alone is not identity.
- Each occurrence receives a cryptographically random persisted client message ID before network I/O.
- Schedule mutation, occurrence creation, delivery intent, remote acknowledgment, lifecycle timestamps, and terminal outcome are durable.
- A unique database constraint prevents duplicate logical occurrences across revisions.
- A universal per-schedule resolved-instant claim spans revisions/kinds; multiple logical weekly slots resolving to one instant are warned and coalesced into one executable MVP run.
- A single daemon holds an exclusive local-filesystem OS lock and is the scheduler leader/SQLite writer; shared/network database filesystems are unsupported.

### FR-6: strict delivery

- Normal delivery uses capability-gated `thread/queue/add` on the existing app-server.
- Only due work enters the app-server queue; future work remains local.
- At most one Threadbeat-owned remote-pending item per target task is allowed.
- All schedules targeting a task share a serialized admission lane.
- Threadbeat never reorders the shared task queue and only mutates exact rows whose complete ownership tuple matches.
- If the strict queue capability is unavailable or disappears, strict schedules pause and diagnostics explain why.
- A racy direct `turn/start` mode is not silently substituted.
- Runtime support requires a semantically tested released-build allowlist; an unknown build is blocked or explicit preview-only, even if the methods exist.

### FR-7: duplicate resistance

- A random persisted `clientUserMessageId`, canonical input digest, target, occurrence, and known remote submission ID form the ownership tuple.
- Before every queue mutation, Threadbeat checks the remote queue and bounded task history for the full tuple.
- A response lost after remote acceptance triggers reconciliation, not a blind retry.
- If neither queue nor history proves the outcome after a bounded grace period, state becomes `delivery_uncertain` and automatic replay stops.
- Add, exact-owned delete, and exact-ID start each have a durable mutation intent and their own ambiguity procedure. Remote queue update is not used in MVP; edits never rewrite already queued input.
- An input/client-ID mismatch is `remote_ownership_violation`, never accepted as success.
- Zero matches remain absent/uncertain according to transport evidence; multiple otherwise matching rows are always ownership ambiguity/violation, never authoritative success.
- Documentation says “durable and duplicate-resistant,” not “exactly once.”

### FR-8: catch-up and overlap

- Heartbeat default is `coalesce`, `maxAge=24h`, `maxRuns=1`.
- No occurrence is materialized as due or sent until the Pi clock is trusted and the nominal due instant has passed.
- If a previous turn is running or remotely queued, new due slots coalesce into one catch-up marker.
- A catch-up does not move the fixed-rate anchor.
- `skip` and bounded `all` are supported advanced policies; `all` has both age and count caps.
- Simultaneous same-task turns are unsupported; true concurrency would require fork-per-run in a later release.

### FR-9: lifecycle truth

The product distinguishes:

- `scheduled`
- `due`
- `delivering`
- `thread_queued`
- `running`
- `waiting_approval`
- `waiting_user_input`
- `succeeded`
- `turn_failed`
- `delivery_failed`
- `interrupted`
- `authority_changed`
- `remote_ownership_violation`
- `task_closed`
- `cancelled`
- `coalesced`
- `delivery_uncertain`
- `superseded`
- `expired`
- `skipped`

It never says a run started merely because queue insertion succeeded. Each record may include `nominalDueAt`, `enqueuedAt`, `startedAt`, `completedAt`, `dispatchLagMs`, and `startLagMs`.

### FR-10: approvals and authority

- Threadbeat never answers app-server approval or user-input requests.
- Queued turns inherit the target task’s current model, working directory, sandbox, and approval settings; this constraint is disclosed at creation and status.
- Activation stores an authorized fingerprint of effective cwd, sandbox, approval policy/reviewer, permission profile/provenance, and exposed instruction identity.
- Threadbeat compares current settings before add/start and pauses as `authority_changed` when fields are missing or differ.
- `approvalsReviewer=auto_review` is refused for unattended work unless human consent is bound to that exact fingerprint.
- The current queue cannot atomically bind a prompt to an authority snapshot; a settings-change race while remotely queued remains an explicit product limitation, not a solved guarantee.
- Endpoint, auth, and credential configuration are CLI/setup concerns, never MCP arguments.

### FR-11: service lifecycle

- Raspberry Pi/Linux setup supports a hardened systemd user service; a dedicated system user is an advanced option.
- macOS setup supports a per-user LaunchAgent.
- The daemon remains in the foreground and lets the service manager own restarts.
- Setup discovers and validates the existing app-server but does not launch or install it by default.
- Threadbeat promises to retain work and reconnect after that separately managed dependency returns; it does not promise app-server uptime or boot ordering.
- Install, upgrade, service removal, credential changes, and remote exposure require explicit user authorization.

### FR-12: distribution

- Signed/checksummed whole-plugin release artifacts target `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, and `x86_64-apple-darwin` initially.
- A source-build path with a pinned MSRV is available.
- The release binary records the Codex version/schema digest against which it was tested.
- The exact runtime is capability-probed and must appear in a semantic test allowlist; version strings or method presence alone do not enable strict delivery.
- The minimum Pi model/OS/glibc/filesystem matrix is frozen after the dependency spike and tested natively on its oldest member.

### FR-13: observability and privacy

- Logs are structured and redact prompt bodies, tokens, raw auth headers, and full sensitive error payloads by default.
- Status exposes connectivity, queue capability, database health, scheduler lag, clock warning, blocked count, and uncertain count.
- Prompt text is stored because replay requires it; MCP reads return a hash, byte length, and redacted preview by default.
- Full prompt access is a separately scoped sensitive operation; global prompt reads are CLI-only.
- Sensitive deployments may use envelope encryption with an independently protected key; a key on the same unencrypted SD card is not theft protection.
- Result bodies are not duplicated into Threadbeat’s database in MVP; IDs, timestamps, status, and a bounded error summary are enough.
- Request-key rows contain only versioned request hashes and stable IDs, not full prompts/results.
- Prompt, soft-delete, WAL/backup, request-key, audit, and run retention are bounded by both age and bytes; `doctor` reports the effective window.
- Core dumps are disabled and panic/support paths are redaction-tested.

## Quality attributes and acceptance criteria

### Correctness

- Property tests show no interval drift across retries, long runs, restarts, or clock corrections.
- DST tests cover gaps, folds, leap years, and a 30-minute transition zone.
- Crash-injection tests at every state transition never create two local occurrences for the same logical slot across revisions.
- Lost-response integration tests never auto-send a second remote prompt without evidence or explicit retry.
- Mixed-client tests never classify or mutate another producer’s queue row as owned.

### Reliability

- A Pi reboot with missed schedules produces the documented bounded catch-up.
- After the separately managed app-server returns, network/app-server restarts recover without operator action unless state is uncertain, interrupted, incompatible, authority-changed, clock-quarantined, storage-unhealthy, or waiting for approval/input.
- A seven-day pre-release soak has no growing memory, file descriptor, task, queue, WAL byte trend, or checkpoint backlog.

### Performance budget

- Idle RSS target: ≤25 MiB; release blocker: >40 MiB on a 64-bit Pi.
- Idle CPU target: <0.2% average; release blocker: ≥0.5% over one hour.
- Scheduler wakeups: one global timer plus health/reconnect activity, not one timer per automation.
- Default database architecture: one connection actor, WAL, bounded retention, and no polling scan of full task history.
- SD-card write amplification is measured during a 30-day-equivalent workload before release.
- Candidate latency gates include p95 local due-to-intent ≤1 second when trusted/connected/idle and bounded startup/reconnect/burst recovery; they become promises only after native Pi measurement.

These are budgets to validate, not unmeasured promises.

### Security

- Local daemon control socket is `0600` in a private runtime directory.
- Peer UID, socket inode/type/owner, and parent-directory ownership are verified; same-UID compromise is treated as full compromise unless a dedicated account is used.
- No unauthenticated non-loopback listener passes acceptance.
- Secrets never appear in process arguments, world-readable files, MCP output, or default logs.
- Service hardening and dependency/security scans pass the release gate.
- An agent cannot redirect the daemon to an arbitrary URL or credential reference.

### Compatibility

- Every supported Codex release passes an initialize, capability, resume, queue, restart, and history-correlation matrix using a real binary.
- Unsupported or changed queue behavior pauses strict schedules and gives an actionable `doctor` result.
- Unknown optional response fields are tolerated; missing identifiers or unknown safety-critical states fail closed.

## Product analytics without surveillance

No network telemetry is required. Local counters are sufficient:

- active/paused schedule count;
- dispatch and start lag percentiles;
- catch-up/coalescing count;
- reconnect and overload count;
- uncertain/interrupted/needs-attention count; and
- process RSS, CPU, DB, WAL, and log size.

The user may export a redacted diagnostics bundle explicitly.

## Release definition

MVP is complete only when the plugin installs on a clean 64-bit Pi, connects to a separately running supported app-server, creates a same-task 30-minute heartbeat and a multi-weekday/multi-time schedule, survives a reboot and missed occurrence, reports a deliberately induced approval without answering it, and passes the documented resource/security/compatibility gates.
