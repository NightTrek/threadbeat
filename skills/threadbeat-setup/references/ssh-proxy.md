# Constrained SSH proxy

SSH is a transport to an existing remote app-server, not permission to administer the remote host.

Require host-key verification, batch mode, explicit identity selection, no PTY, no agent or X11 forwarding, and no general shell or arbitrary forwarding. The supported v1 target is a dedicated key and account constrained to the exact `codex app-server proxy` command. A broad shell account or local-forward setup is development-only and must not be presented as the hardened default.

Never create or alter keys, `authorized_keys`, `known_hosts`, SSH daemon settings, accounts, or forced commands without a separate exact plan and explicit authorization. Never accept a new or changed host key silently.

The deployment plan must show the host alias and fingerprint, remote account, identity reference, exact forced command, permitted address families, retry/backoff behavior, and which local credential facility protects the key. After separately authorized activation, use `threadbeat --config <absolute-config-path> ctl doctor --thread-id <originating-thread-id> --full` while testing failure-closed behavior for unknown host keys, wrong keys, unexpected commands, network loss, and app-server incompatibility.
