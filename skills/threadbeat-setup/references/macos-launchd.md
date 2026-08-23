# macOS LaunchAgent

Use a per-user LaunchAgent. It normally starts only in that user's GUI/login domain; do not describe it as a pre-login system daemon.

Resolve absolute versioned binary, config, state, runtime, and log paths. Generate the plan with `threadbeat --config <absolute-config-path> service plan --destination <absolute-LaunchAgents-plist> --output <absolute-plan.json>`. The CLI renders the plist internally; `service/templates/dev.nighttrek.threadbeat.plist.in` is a reference and must never be loaded directly. Keep credentials in Keychain or a separately protected reference rather than the plist or command arguments.

Apply the reviewed file only with `threadbeat --config <absolute-config-path> service apply --plan <absolute-plan.json> --plan-digest <reviewed-digest>`. Apply copies the plist but does not bootstrap, enable, or start it. Treat the plan's printed activation command as a separate approval boundary. Do not use `sudo`, install a LaunchDaemon, or bootstrap another domain unless the user explicitly chooses a separately reviewed system deployment.

After separately authorized activation, verify the loaded label and resolved paths, process ownership, restart behavior, private directories, clock trust, daemon handshake, app-server connection, queue compatibility, and bounded logs. Then run `threadbeat --config <absolute-config-path> ctl doctor --thread-id <originating-thread-id> --full`.
