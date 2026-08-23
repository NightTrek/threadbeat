# Recovery and removal

## Recovery

Use read-only service-manager status plus `threadbeat --config <absolute-config-path> ctl doctor --thread-id <originating-thread-id> --full` before changing configuration. Report clock, storage, OS lock, facade/daemon protocol, app-server reachability, tested-build status, queue capability, blocked runs, and uncertain mutations separately.

Do not clear an OS lock, delete a WAL or database, restart repeatedly, or retry an uncertain remote mutation merely to make health green. Preserve evidence and follow the reported recovery action. If rollback is available, drain and handshake before removing the newer binary.

## Removal

Generate a removal plan before mutation. It must state:

- which schedules will be paused;
- which exact owned remote rows can be reconciled or cancelled and which remain uncertain;
- the service stop, disable, or unload actions;
- files and versioned binaries removed;
- whether plugin removal is separate; and
- state retention versus purge, including backup and secure-erasure limits on SQLite and flash.

Retain state by default until the user chooses purge. Never interpret “uninstall the plugin” as permission to purge the database or credentials. Warn when removing the plugin would leave the service running, or removing the service would leave the plugin's MCP entry unable to connect.
