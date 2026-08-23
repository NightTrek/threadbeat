# Service packaging references

These files are review references, not runtime inputs and not ready-to-install service definitions. The current CLI renders service content internally. Create a saved plan with `threadbeat --config <absolute-config-path> service plan --destination <absolute-service-file> --output <absolute-plan.json>`, then compare its content and safety properties with the relevant reference template.

Apply only an exact reviewed plan with `threadbeat --config <absolute-config-path> service apply --plan <absolute-plan.json> --plan-digest <reviewed-digest>`. Apply writes the service file and prints the activation command; it does not run that command or otherwise load, enable, start, or bootstrap the service.

The templates never configure Codex app-server, user lingering, SSH trust or authorization, accounts, or public listeners. Those are separate, explicit operations. See `references/template-variables.md` and the platform references when reviewing generated content.
