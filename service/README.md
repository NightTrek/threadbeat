# Service packaging references

These files are review references, not runtime inputs and not ready-to-install service definitions. The current CLI renders service content internally. Create a saved plan with `threadbeat --config <absolute-config-path> service plan --destination <absolute-service-file> --output <absolute-plan.json>`, then compare its content and safety properties with the relevant reference template.

Apply only an exact reviewed plan with `threadbeat --config <absolute-config-path> service apply --plan <absolute-plan.json> --plan-digest <reviewed-digest>`. Apply reconstructs the canonical plan and creates exactly the configured private state and runtime leaf directories with mode `0700` plus the service definition with mode `0600`. The three parent directories must already exist; apply never creates unreviewed ancestors. It prints the activation command but never runs it or otherwise loads, enables, starts, or bootstraps the service.

The templates never configure Codex app-server, user lingering, SSH trust or authorization, accounts, or public listeners. Those are separate, explicit operations. See `references/template-variables.md` and the platform references when reviewing generated content.
