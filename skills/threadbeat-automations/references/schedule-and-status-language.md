# Schedule and status language

Read this reference when translating calendar language, explaining several time slots, or reporting a nonterminal or exceptional run.

## Schedule rendering

Explain the schedule in ordinary language before calling `automation_update`.

- `once`: show the RFC 3339 instant in the user's intended local zone when known and in UTC.
- `interval`: show the whole-minute interval and that future slots stay on `anchor + n × interval`; a late run does not move the grid.
- `weekly`: show the IANA timezone, normalized weekday/time Cartesian product, and DST policy. Use a compact table when it improves clarity.

Default weekly DST behavior is to skip a nonexistent spring-forward local time and run once at the first matching fall-back instant. Do not imply that model execution begins at the nominal second; a busy task, clock quarantine, app-server outage, approval, or queue backlog can delay it.

Example summary:

| Field | Normalized value |
|---|---|
| Days | Monday–Thursday |
| Times | 10:00, 12:00 |
| Timezone | America/Los_Angeles |
| DST | Skip spring gap; first fall-fold instant |
| Catch-up | One latest miss, at most 24 hours old |

## Tool routing

- `automation_update`: create/suggest, edit, pause, resume, or soft-delete a schedule.
- `automation_run`: run now, cancel, retry, or start one exact blocked owned run.
- `automation_get`: fetch one current-task automation and its revision.
- `automation_list`: find current-task automations and avoid duplicates.
- `automation_runs`: inspect truthful run lifecycle/history.
- `automation_doctor`: diagnose daemon, clock, storage, authority, app-server, or compatibility health.

## Truthful lifecycle language

- `scheduled`: recorded locally for a future nominal instant.
- `due` / `delivering`: due locally or in the guarded delivery path; not yet accepted remotely.
- `thread_queued`: accepted into the Codex task queue; it has not necessarily started.
- `running`: correlated to an actual turn start.
- `waiting_approval` / `waiting_user_input`: requires the user; Threadbeat does not answer.
- `succeeded` / `turn_failed`: terminal turn result.
- `delivery_failed`: delivery did not commit and may be explicitly retried.
- `delivery_uncertain`: a remote mutation may have committed; never call it failed or retry automatically.
- `interrupted`, `authority_changed`, `remote_ownership_violation`, or `task_closed`: report the exact blocker and required user action.
- `coalesced`, `superseded`, `expired`, or `skipped`: no separate execution should be implied.

Always distinguish nominal due, enqueued, started, and completed timestamps when available.
