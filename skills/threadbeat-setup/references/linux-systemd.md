# Raspberry Pi and Linux systemd

Use a per-user service by default for a single-user device. A dedicated system account is a separate advanced choice with its own filesystem, socket, and SSH-access review.

## Planning checks

- Confirm the exact Pi model and architecture, Raspberry Pi OS or Linux release, glibc/ABI, local filesystem, free space, and clock-sync service.
- Resolve absolute versioned binary, config, state, runtime, and app-server socket paths without following unsafe symlinks.
- Generate the reviewable plan with `threadbeat --config <absolute-config-path> service plan --destination <absolute-user-unit-path> --output <absolute-plan.json>`. The CLI renders the service content internally; `service/templates/threadbeat-user.service.in` is a hardening reference and must not be installed directly.
- For a same-host Unix topology, restrict address families to `AF_UNIX`. Add `AF_INET AF_INET6` only for an approved SSH topology.
- The current CLI renderer keeps `ProtectHome=read-only` and emits explicit `ReadWritePaths` entries for the configured state and runtime directories. Verify those exact paths in the rendered unit and do not weaken `ProtectHome`. Reject the plan if the binary, config, app-server socket, or approved SSH material would be inaccessible under the effective hardening; select paths that can pass `doctor` after separately authorized activation.

## Authorization boundaries

The plan records an activation command, but `threadbeat --config <absolute-config-path> service apply --plan <absolute-plan.json> --plan-digest <reviewed-digest>` requires the state, runtime, and systemd-user parents to exist, creates only the configured state and runtime leaf directories with mode `0700`, and writes the reviewed unit with mode `0600`. It never runs daemon-reload, enables, starts, or activates the unit. Those actions and lingering are separate; none is implied by another. Never run `loginctl enable-linger` without explicit approval of that exact action and its boot-without-login consequence.

After separately authorized activation, verify unit identity and paths, file modes, effective hardening, process arguments, clock trust, OS lock, app-server connection, strict-queue allowlist, and bounded logs. Then run `threadbeat --config <absolute-config-path> ctl doctor --thread-id <originating-thread-id> --full`. A running unit is not proof of a working automation path.
