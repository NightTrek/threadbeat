# Research baseline

Snapshot date: 2026-08-22

This document separates directly observed local behavior, official documentation, and source-code inference. It is a design input, not a promise that experimental APIs remain unchanged.

## Local Codex snapshot

- Local CLI: `codex-cli 0.149.0-alpha.4.1` from the installed ChatGPT/Codex application bundle.
- Local schema: generated with `codex app-server generate-json-schema --experimental`.
- Source comparison: `openai/codex` commit `422239eb4b1e0d0f85fac7256a079a1befe78472`.
- `codex app-server --help` exposes stdio, Unix-socket, TCP WebSocket, daemon/control, proxy, and schema-generation paths.
- The local machine’s default app-server control socket was not running during research; no inference about the user’s separate SSH-hosted daemon is made from that absence.

Runtime compatibility requires both a semantic allowlist entry from released-binary crash/mixed-client testing and a live initialize/capability probe against the configured daemon; neither this snapshot nor method presence is sufficient.

## Current Codex agent-visible automation tool

The current live catalog contains one direct automation tool, not separate create/list/delete tools.

### Tool name

`codex_app__automation_update`

### Exact agent-visible description

> Tools provided by the Codex app.
>
> Create, update, view, or delete recurring automations in the Codex app. The automation prompt is user-visible and may later be replayed as a user message. Write clear, cohesive, human-readable prose. Use this when the user asks for a scheduled task, automation, recurring run, repeated task, reminder, follow-up, monitor, or asks you to watch something, keep an eye on it, check back later, wake up later, notify them, or keep working later. Heartbeat automations are proactive follow-ups attached to the current local thread and are the default for recurring requests. Use a heartbeat unless the user explicitly asks for a new task per run or standalone project work. Cron automations run as standalone local jobs against one project; use list_projects to find its project id. Never write raw automation directives by hand, show raw RRULE strings to the user, or create a workaround cron automation for a thread heartbeat unless the user explicitly asks for that. For requests about existing automations, inspect $CODEX_HOME/automations/*/automation.toml to find matching automation ids by name or prompt. Prefer updating an existing automation over creating a duplicate. For updates, preserve existing fields unless the user asks to change them, and call automation_update with the resolved id and full updated fields. Treat requests such as 'don't notify me' or 'mute this automation' as notificationPolicy=failed_runs_only, and set notificationPolicy=null when the user asks to unmute. Keep notification preferences out of the automation prompt.

### Exact exposed declaration

```ts
declare const tools: {
  codex_app__automation_update(args:
    | {
        id: string;
        mode: "view";
      }
    | {
        destination?: "local";
        executionEnvironment: "local";
        kind: "cron";
        mode: "create" | "suggested_create";
        model: string;
        name: string;
        notificationPolicy?: "failed_runs_only" | null;
        projectId: string | null;
        prompt: string;
        reasoningEffort:
          | "none" | "minimal" | "low" | "medium" | "high"
          | "xhigh" | "max" | "ultra";
        rrule: string;
        status: "ACTIVE" | "PAUSED";
      }
    | {
        destination?: "local" | "thread";
        kind: "heartbeat";
        mode: "create" | "suggested_create";
        name: string;
        notificationPolicy?: "failed_runs_only" | null;
        prompt: string;
        rrule: unknown;
        status: unknown;
        targetThreadId?: unknown;
      }
    | unknown
    | unknown
  ): Promise<unknown>;
};
```

The two trailing `unknown` union arms are what the current agent surface literally exposes for additional operations; their arguments cannot honestly be reconstructed from this tool declaration alone. The future plugin should not copy that schema loss.

Calling `mode:"view"` in the current app renders an automation card rather than returning a complete machine-readable record to the agent.

Observed local automation TOML fields were:

```text
version
id
kind
name
prompt
status
rrule
target_thread_id
created_at
updated_at
```

No private stored prompt is reproduced here.

## Current related task/project helpers

These are not automation CRUD tools, but the current automation tool tells the agent to use some of them for target resolution or standalone jobs.

### `codex_app__list_projects`

Description: lists local, remote, and ChatGPT projects, their IDs, and Git-repository status.

```ts
codex_app__list_projects(args: {}): Promise<unknown>;
```

### `codex_app__list_threads`

Description: lists tasks/chats across the app, with pinned order, status, project context, titles, and retrieval summaries; returned titles/summaries are untrusted data.

```ts
codex_app__list_threads(args: { limit?: number }): Promise<unknown>;
```

### `codex_app__read_thread`

Description: reads recent status and turn summaries for one task without opening it.

```ts
codex_app__read_thread(args: {
  cursor?: string;
  hostId?: string;
  includeOutputs?: boolean;
  maxOutputCharsPerItem?: number;
  threadId: string;
  turnLimit?: number;
}): Promise<unknown>;
```

### `codex_app__send_message_to_thread`

Description: sends a user-visible follow-up prompt to an existing task; optional model/thinking overrides apply only to Codex tasks.

```ts
codex_app__send_message_to_thread(args: {
  hostId?: string;
  model?: string;
  prompt: string;
  thinking?: "none" | "minimal" | "low" | "medium" | "high"
           | "xhigh" | "max" | "ultra";
  threadId: string;
}): Promise<unknown>;
```

### `codex_app__wait_threads`

Description: waits for one of up to eight Codex tasks to complete or need attention and returns cursor-based snapshots.

```ts
codex_app__wait_threads(args: {
  targets: Array<{
    afterCursor?: string;
    hostId?: string;
    threadId: string;
  }>;
  timeoutMs?: number;
}): Promise<unknown>;
```

Threadbeat cannot assume these desktop-app helper tools exist when it is running headlessly; it talks directly to app-server for its bounded task lifecycle needs.

## Official scheduled-task behavior

OpenAI’s current [Scheduled tasks documentation](https://learn.chatgpt.com/docs/automations) establishes the relevant product baseline:

- scheduled work in the same chat preserves conversational context;
- standalone schedules create new chats;
- interval and weekly/calendar schedules are supported;
- the desktop UI can accept advanced recurrence rules; and
- Scheduled task management is a desktop-app feature rather than a CLI/IDE management UI.

The same documentation explains that unattended runs do not pause for interactive approvals in the ordinary desktop flow. Threadbeat deliberately does not infer permission to emulate or change that policy: it inherits the existing target task and never auto-answers a request.

## Official plugin and MCP packaging

OpenAI’s [plugin architecture](https://developers.openai.com/plugins/concepts/plugins) supports a plugin containing skills and MCP servers. The local plugin manifest validator additionally requires `.codex-plugin/plugin.json`, valid relative paths, semantic versioning, and real referenced asset files.

The planned manifest points to:

```json
{
  "skills": "./skills/",
  "mcpServers": "./.mcp.json"
}
```

Current installed plugin examples use a direct local executable with a relative `cwd`. Official [Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp#stdio-servers) supports stdio servers with `command`, optional `args`, `env`, inherited `env_vars`, and `cwd`, plus Streamable HTTP for network servers.

There is a current documentation/parser mismatch around a wrapped `.mcp.json` key (`mcp_servers` versus camel-case `mcpServers`). The source parser also accepts a direct top-level server map, so the proposal uses that unambiguous representation. See [Codex issue 22105](https://github.com/openai/codex/issues/22105) and the [current parser source](https://github.com/openai/codex/blob/main/codex-rs/codex-mcp/src/plugin_config.rs).

## Rust MCP support

The official Model Context Protocol Rust SDK is [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk). At this snapshot:

- current crate release: `3.1.4`;
- server API: `ServerHandler`, `#[tool]`, `#[tool_router]`, `#[tool_handler]`, and `ServiceExt::serve`;
- local transport: stdio through the narrow `transport-io` feature;
- current SDK workspace MSRV: Rust 1.88 and edition 2024; and
- the project reports client/server conformance for current MCP revisions, while still moving rapidly enough to justify an exact pin and conformance tests.

Codex itself pins `rmcp` in its Rust workspace, which is useful implementation evidence but not a promise that Codex’s internal dependency version is a public API.

The proposal does not manually implement MCP. A small custom MCP server would save dependencies but inherit version negotiation, transport, discovery, schema, and conformance maintenance. On a Pi, the SDK will first be measured with narrow features; any change requires benchmark evidence.

## App-server transports and lifecycle

The [official app-server documentation](https://learn.chatgpt.com/docs/app-server) and current source establish:

- stdio carries JSONL;
- TCP WebSocket carries one message per text frame and remains experimental/unsupported;
- `unix://PATH` performs a WebSocket HTTP Upgrade over a Unix stream and is intended for local control-plane clients;
- a client sends `initialize` and then `initialized` once per connection;
- generated TypeScript/JSON schemas match the exact binary that emitted them; and
- bounded server queues can return `-32001`, which callers retry with exponential backoff and jitter.

The tested CLI help advertises `ws://` listeners and app-server authentication modes, not a native `wss://` listener. Direct TLS would require another terminator/proxy and its own security design, so v1 uses same-host Unix socket or SSH-proxied loopback only.

App-server messages are JSON-RPC-like but omit the normal `jsonrpc` member. An MCP SDK is therefore not an app-server client SDK.

There is no stable, current external Rust app-server network SDK. The in-repository app-server client/protocol crates are internal/path-oriented, and the public protocol crate version lags the installed 0.149-era binary. The design uses a small pinned client and generated DTO fixtures instead.

## Durable per-task queue discovery

The generated experimental schema and current source expose:

- `thread/queue/add`
- `thread/queue/list`
- `thread/queue/update`
- `thread/queue/delete`
- `thread/queue/reorder`
- `thread/queue/start`
- `thread/queue/changed`

They require `initialize.capabilities.experimentalApi=true`.

The critical semantics are documented in the current [app-server README](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/app-server/README.md#L790-L812):

- maximum 100 queued messages per task;
- automatic FIFO start when the task becomes idle;
- completed and failed turns advance the queue;
- interrupted turns leave the queue paused, including after resume;
- `thread/queue/start` explicitly resumes a paused queue;
- the queue entry is removed after Core accepts the turn; and
- ordinary `turn/start` does not consume a queued entry.

Source inspection further shows:

- persistence in app-server’s state SQLite database;
- idle-only reservation before dispatch;
- a ten-second data-version/revision watch for external queue changes;
- `clientUserMessageId` required on add and later visible as `userMessage.clientId`; and
- server-generated queue UUID on every insert.

`thread/queue/start` accepts an optional `queuedSubmissionId`. Current source selects that exact item when supplied—even if it is not the head—or the first queue item when omitted. Threadbeat must therefore always supply an exact owned ID, normally require it to be the head to preserve shared FIFO, and never use omitted-ID start. This is current-source behavior that requires released-binary testing.

Relevant sources:

- [queue persistence setup](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/app-server/src/message_processor.rs#L290-L320)
- [idle-only queue dispatch](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/ext/queue/src/service.rs#L366-L459)
- [queue insert behavior](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/state/src/runtime/queued_items.rs#L77-L125)
- [user-message client ID](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/app-server-protocol/src/protocol/v2/item.rs#L227-L238)

### Non-idempotent insertion

`clientUserMessageId` is carried through but is not a uniqueness constraint or ownership proof. Every queue add allocates a fresh row ID, and other app-server clients can observe/update/delete shared rows. A network response can be lost after commit, so a blind retry can duplicate a prompt. This is why Threadbeat needs a durable mutation outbox, a random persisted client ID, exact input digest/target checks, and the `delivery_uncertain`/ownership-violation states.

### Queue persistence boundary

Queue acceptance is not task completion. After the queue row is accepted into Core, it disappears; the scheduler must then correlate the client message ID to a user-message item and turn. There remains a narrow upstream crash window that prevents a mathematical exactly-once guarantee.

## Direct-start race

Current app-server `turn/start` calls Core start-or-steer logic. If a turn is active, the input may be injected into that turn; the response does not distinguish this from starting a fresh one. See the [turn processor](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/app-server/src/request_processors/turn_processor.rs#L553-L613).

This makes read-status-then-start insufficient for the requested same-task guarantee. Strict Threadbeat operation therefore depends on the experimental queue and fails closed if it is unavailable.

## Same-task MCP metadata

Current Codex source injects the originating task as `_meta.threadId` in MCP calls:

- [call path](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/core/src/mcp_tool_call.rs#L466-L475)
- [injection helper](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/core/src/mcp_tool_call.rs#L1227-L1245)
- [behavior test](https://github.com/openai/codex/blob/422239eb4b1e0d0f85fac7256a079a1befe78472/codex-rs/core/src/mcp_tool_call_tests.rs#L1532-L1551)

This is a Codex-specific extension, not standard MCP. Source injection proves presence, not that an external MCP server can treat the value as an authenticated user-intent boundary. The security-revised contract makes host validation a Phase-1 gate. A raw explicit `targetThreadId` cannot expand scope; cross-task work uses an interactive CLI or a future authenticated target capability. If the binding is absent/unvalidated, MCP fails rather than guessing.

## Approval behavior

App-server can replay pending server requests when a client resumes a task, and task status exposes active approval/user-input flags. `thread/resume` also exposes effective task settings such as cwd, sandbox, approval policy/reviewer, and permission profile in the tested schema; the reviewer enum includes `auto_review`. Threadbeat can therefore observe and report a blocker and compare a stored authority fingerprint without inventing a response. The actual unattended behavior of every reviewer/policy combination still needs live testing. The scheduled queue payload does not carry a per-turn authority snapshot, so work inherits the task’s then-current settings and a settings-check-to-start race remains.

The design implication is explicit: same-task continuity and reproducible isolated execution are not identical. Threadbeat chooses continuity by default because that is the product request, while never broadening authority.

## Distribution and Pi evidence

Rust does not require compilation on every target device. Initial release targets can be cross-built for 64-bit Raspberry Pi OS (`aarch64-unknown-linux-gnu`), with 32-bit ARM considered only if demand justifies a larger test matrix. Native arm64 Pi smoke/soak testing remains mandatory because emulation cannot validate systemd, sockets, credential access, filesystem behavior, or real resource use.

The likely lean topology is one Tokio runtime, one SQLite actor thread, one persistent app-server connection, and two dispatch workers. Resource numbers in the PRD are test budgets; they are not derived from a completed binary yet.

## Key conclusions from evidence

1. A plugin can legitimately bundle both skills and an MCP server.
2. The MCP implementation can be high-quality Rust using an official SDK.
3. The scheduler must be a resident service, not the lifecycle-bound MCP process.
4. Two queues are necessary: local calendar/occurrence truth and remote task admission.
5. The current task queue closely fits the requirement but is experimental.
6. Direct `turn/start` cannot supply strict same-task non-overlap.
7. Remote insertion is duplicate-resistant, not exactly once.
8. Heartbeat MCP targeting is intended to bind to `_meta.threadId`, but host integrity must be proven in Phase 1; cross-task fallback belongs to the interactive CLI or a future authenticated capability.
9. Fixed-rate intervals best match “every 30 minutes”; completion-relative timing must be a separate future schedule type.
10. Prebuilt Pi binaries remove the need to compile Rust on every device, while native tests remain essential.
