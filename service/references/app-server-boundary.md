# Separately managed app-server boundary

Threadbeat connects to an existing Codex app-server; its service plan does not install, start, update, replace, or expose that dependency.

The plan records only the selected topology and protected endpoint reference. Same-host mode uses the existing Unix socket. Remote mode uses the separately approved constrained SSH proxy described by the setup skill. Missing app-server readiness blocks verification but does not trigger a bootstrap fallback.

Service success and product success are distinct: a running Threadbeat process is not verified until `threadbeat --config <absolute-config-path> ctl doctor --thread-id <originating-thread-id> --full` establishes clock and storage health, daemon protocol compatibility, app-server initialization, a semantically supported build, strict queue capability, and the intended task authority fields.
