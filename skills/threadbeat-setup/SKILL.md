---
name: threadbeat-setup
description: Plan, install, validate, upgrade, roll back, diagnose, or remove the Threadbeat daemon and MCP facade on Raspberry Pi/Linux or macOS. Use for systemd, launchd, Unix-socket, or constrained SSH-proxy setup. Do not use for ordinary schedule creation or run management.
---

# Threadbeat Setup

Treat setup as a plan/apply workflow. A request to install Threadbeat does not silently authorize enabling user lingering, changing SSH configuration, installing or starting Codex app-server, opening a listener, or purging state.

## Establish the deployment

Inspect the OS, architecture, libc/ABI, filesystem, free space, clock synchronization, service manager, installed Threadbeat/Codex versions, and existing app-server endpoint. V1 supports:

- same-host app-server over its Unix socket; or
- a constrained OpenSSH proxy to a separately managed app-server.

Threadbeat does not install, start, update, replace, or guarantee the uptime of Codex app-server. Stop with an actionable dependency check if the existing app-server is absent or unsupported.

Read the relevant platform reference before proposing changes:

- [references/linux-systemd.md](references/linux-systemd.md)
- [references/macos-launchd.md](references/macos-launchd.md)
- [references/ssh-proxy.md](references/ssh-proxy.md)
- [references/recovery-and-removal.md](references/recovery-and-removal.md)

## Plan before apply

1. Prefer a signed prebuilt bundle for an explicitly supported target. Offer the pinned source build only as a fallback, with its RAM, disk, toolchain, and time requirements.
2. Run `threadbeat --config <absolute-config-path> service plan --destination <absolute-service-file> --output <absolute-plan.json>`. Add `--binary <absolute-versioned-binary>` when the currently running executable is not the intended service binary. Platform selection is automatic; use `--platform systemd-user` or `--platform launchd-agent` only when deliberately overriding it. The JSON plan must show the exact binary, config, service file content and mode, activation/removal commands, and digest.
3. Present the saved plan, its digest, and its side effects. Do not run apply until the user explicitly authorizes that exact plan and digest.
4. Apply only with `threadbeat --config <absolute-config-path> service apply --plan <absolute-plan.json> --plan-digest <reviewed-digest>`. If the plan changes, stop and present the new digest. Apply writes the service definition but does not load, enable, start, or bootstrap it.
5. Treat the printed activation command as a new mutation requiring separate approval. Once the daemon is actually running, run `threadbeat --config <absolute-config-path> ctl doctor --thread-id <originating-thread-id> --full`. Distinguish files written, service activation, daemon availability, app-server connectivity, and compatibility rather than treating apply as end-to-end success.

For an active-operation grant, use `threadbeat --config <absolute-config-path> ctl authorize --thread-id <originating-thread-id> --minutes <1-1440>` only after the user requests it. The grant is temporary and task-scoped; it is not implied by service setup.

Never run `loginctl enable-linger`, modify `authorized_keys` or SSH daemon policy or known-host trust, create a dedicated account, load a LaunchAgent, enable or start a unit, or replace a binary unless that exact action appears in the approved plan. Never put a secret in a command argument or service template.

## Upgrade, rollback, and removal

An upgrade must drain work, preserve ambiguous intents, verify the signed bundle, use an absolute versioned binary path, restart and handshake, and keep the prior binary until verification succeeds.

Before uninstalling, generate a removal plan that pauses schedules, reconciles exact owned remote items, reports uncertainty, stops or disables only the approved service, and asks whether state should be retained or purged. Plugin removal alone may leave the independently managed service running.
