# Template variables

Every variable is required unless the platform template does not contain it.

| Variable | Requirement |
|---|---|
| `THREADBEAT_BINARY` | Absolute, versioned, signature-verified executable path; never a mutable PATH lookup. |
| `THREADBEAT_CONFIG` | Absolute protected config path containing references, not inline secrets. |
| `WORKING_DIRECTORY` | Absolute stable directory validated by the plan. |
| `STATE_DIRECTORY` | Absolute local-filesystem state directory, owned by the service account and mode `0700`. |
| `RUNTIME_DIRECTORY` | Absolute private runtime or socket directory, owned by the service account and mode `0700`. |
| `LOG_DIRECTORY` | Absolute private, size-bounded log directory with rotation; logs remain redacted. |
| `THREADBEAT_DOCUMENTATION` | A valid documentation URI or `man:` reference included in the approved package. |
| `RESTRICT_ADDRESS_FAMILIES` | `AF_UNIX` for local UDS; `AF_UNIX AF_INET AF_INET6` only for approved SSH topology. |

Rendering must reject missing tokens, relative paths where absolute paths are required, unsafe symlinks, unexpected owner or mode, newline or control characters, and a state database on a shared or network filesystem. No secret value is substituted into either template.
