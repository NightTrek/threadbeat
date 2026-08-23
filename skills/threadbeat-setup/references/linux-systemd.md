# Raspberry Pi and Linux systemd

Use a per-user service by default for a single-user device. A dedicated system account is a separate advanced choice with its own filesystem, socket, and SSH-access review.

## Planning checks

- Confirm the exact Pi model and architecture, Raspberry Pi OS or Linux release, glibc/ABI, local filesystem, free space, and clock-sync service.
- Resolve absolute versioned binary, config, state, runtime, and app-server socket paths without following unsafe symlinks.
- Render the reference template at `service/templates/threadbeat-user.service.in`; do not install the template itself.
- For a same-host Unix topology, restrict address families to `AF_UNIX`. Add `AF_INET AF_INET6` only for an approved SSH topology.
- Do not add `ProtectHome=true` blindly: it can hide the user-owned app-server socket or SSH material. Render the narrowest paths that pass `doctor`.

## Authorization boundaries

`service plan` must separately disclose unit installation, daemon-reload, enable, start, and lingering. None is implied by another. Never run `loginctl enable-linger` without explicit approval of that exact action and its boot-without-login consequence.

After an approved apply, verify unit identity and paths, file modes, effective hardening, process arguments, clock trust, OS lock, app-server connection, strict-queue allowlist, and bounded logs. A running unit is not proof of a working automation path.
