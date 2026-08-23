# macOS LaunchAgent

Use a per-user LaunchAgent. It normally starts only in that user's GUI/login domain; do not describe it as a pre-login system daemon.

Resolve absolute versioned binary, config, state, runtime, and log paths. Render `service/templates/com.nighttrek.threadbeat.plist.in`; never load the unrendered template. Keep credentials in Keychain or a separately protected reference rather than the plist or command arguments.

The approved plan must separately show copying the plist, bootstrapping or loading it, enabling it, and starting it. Do not use `sudo`, install a LaunchDaemon, or bootstrap another domain unless the user explicitly chooses a separately reviewed system deployment.

After apply, verify the loaded label and resolved paths, process ownership, restart behavior, private directories, clock trust, daemon handshake, app-server connection, queue compatibility, and bounded logs.
