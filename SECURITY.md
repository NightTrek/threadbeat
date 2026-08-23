# Security policy

Threadbeat controls scheduled prompts and writes to an existing Codex app-server queue, so security reports are taken seriously even during alpha development.

## Supported versions

Only the newest published prerelease receives security fixes. Alpha releases are for testing and do not carry a production-support commitment.

| Version | Supported |
|---|---|
| Latest `0.1.0-alpha.*` | Best-effort security fixes |
| Older prereleases and source snapshots | No |

## Reporting a vulnerability

Use [GitHub's private vulnerability reporting form](https://github.com/NightTrek/threadbeat/security/advisories/new). Do not open a public issue for a suspected vulnerability and do not include live credentials, private prompts, access tokens, or user data in a report.

Include the affected commit or release, platform, configuration with secrets removed, expected security boundary, reproduction steps, and impact. You should receive an acknowledgement through the advisory within seven days; investigation and remediation timing depend on severity and reproducibility.

## Security boundary

- Threadbeat is intended to run as an unprivileged, dedicated user or the interactive user's service.
- Delivery is off by default. Enabling it requires an explicitly configured existing app-server endpoint and exact-build compatibility evidence.
- The service must not expose its control socket or app-server proxy to untrusted users or a public network.
- Service setup never enables, starts, or grants lingering by itself. Review the content-addressed plan before applying it.
- Release archives should be verified against `SHA256SUMS` and GitHub artifact attestations before installation.
- Dependency advisories, licenses, registries, and Git sources are gated by `cargo-deny` in the release workflow.

The current non-production limitations and unresolved trust boundaries are listed in [`docs/10_IMPLEMENTATION_STATUS.md`](docs/10_IMPLEMENTATION_STATUS.md).
