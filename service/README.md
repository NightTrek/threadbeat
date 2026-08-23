# Service packaging references

These files are inputs to `threadbeat service plan`; they are not ready-to-install service definitions. The planner must replace every `@NAME@` token with a validated absolute value, render the exact result and digest, and apply only that user-authorized digest.

The templates never configure Codex app-server, user lingering, SSH trust or authorization, accounts, or public listeners. Those are separate, explicit operations. See `references/template-variables.md` and the platform references before rendering.
