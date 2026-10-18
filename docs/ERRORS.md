# Error codes

`zestroku_types::ErrorCode` is the canonical, additive registry. Existing code strings are never renamed or reused. New entries append to the registry and its wire snapshot.

## Error fields

- `error`: the operation-specific cause, such as `device_not_found` or `undo_app_not_installed`.
- `code`: the stable semantic code, such as `UNKNOWN_DEVICE` or `APP_NOT_INSTALLED`.
- `detail`: what failed in this operation.
- `hint`: the owner's next action.
- `suggestions`: available device/app candidates; an empty list means this error has no supplied candidates. Suggestions never select or launch a target.

HTTP non-success JSON and MCP tool-error data carry these fields. An MCP JSON-RPC numeric error is a transport classification, not the product's semantic code. The CLI prints its code, detail, hint and supplied candidates to **stderr**; machine-readable command results remain on **stdout**. A failed read does not fall back to direct device I/O.

Missing, malformed or unregistered daemon semantic metadata is `PROTOCOL_ERROR`, not a guessed device or policy failure. The HTTP status and valid operation cause remain available for the CLI's existing exit contract. Human-readable message text is never parsed to choose a semantic code.

## Registry and CLI exits

`ErrorCode::cli_exit()` describes a default, not a global one-code/one-number mapping. Callers preserve their operation-specific numeric exits. Several semantic codes share a class, and one code can have documented contextual exits. Never infer the semantic code from the exit number alone.

| Code | Meaning | Default exit | Contextual exits |
|---|---|---:|---|
| `UNKNOWN_DEVICE` | No inventory device matches the requested target | 2 | Daemon reads: 1 |
| `AMBIGUOUS_NAME` | A device target matches more than one candidate | 4 | Reads and alias resolution: 2 |
| `AMBIGUOUS_APP` | An app name matches more than one installed app | 2 | Undo-plan refusal: 5 |
| `DEVICE_UNREACHABLE` | The device transport could not complete the operation | 5 | Diagnostic reads/discovery: 1 |
| `DEVICE_SUSPENDED` | An observed device stopped responding | 5 | Preserve the receiving command's existing failure exit |
| `TIER_REFUSED` | The observed device tier refuses the operation | 3 | A late diagnostic mutation wire refusal remains 5 |
| `CONFIRMATION_REQUIRED` | Owner approval is absent or unusable | 4 | `approve` TTY/phrase refusal: 2 |
| `POLICY_DENIED` | The owner policy denied the operation | 5 | — |
| `APP_NOT_INSTALLED` | The requested or previously active app is unavailable | 2 | Undo's missing-before-app refusal: 5 |
| `STORE_UNAVAILABLE` | The local store or an offline backup/restore operation is unavailable | 1 | Discovery found devices but could not persist them: 5 |
| `LAN_BLOCKED_BY_OS_POLICY` | The OS denied the explicit diagnostic LAN connection | 5 | Diagnostic reads: 1 |
| `DAEMON_UNAVAILABLE` | The configured resident daemon did not answer | 1 | Mutations: 5 |
| `DOCTOR_WARN` | Health checks completed with warnings | 6 | — |
| `DOCTOR_FAIL` | Health checks completed with failures | 7 | — |
| `DIRECT_CONNECTION_FAILED` | An explicit diagnostic connection failed | 1 | — |
| `INVALID_DAEMON_URL` | The configured daemon URL is invalid or outside its permitted scope | 1 | Mutations: 2 |
| `INVALID_DEVICE_TARGET` | A target contains an invalid authority/path character | 2 | Read-client failure: 1 |
| `PROTOCOL_ERROR` | A response or command violates its protocol contract | 1 | Mutation wire failures: 5; existing HTTP-status-specific refusals retain their operation exit |
| `INVALID_ARGUMENT` | Command arguments or owner-provided input are invalid | 2 | Credential-input failure after installer authorization remains 5 |
| `CONFIGURATION_ERROR` | A target/policy or listener configuration is invalid or unavailable | 1 | Server bind validation: 2; direct mutation policy loading: 5 |
| `RUNTIME_UNAVAILABLE` | A local runtime, service lifecycle or approval-entropy resource could not complete | 1 | — |
| `OUTPUT_ERROR` | A command cannot write its result | 1 | — |
| `INSTALL_FAILED` | The authorized developer-installer operation failed | 5 | — |

Exit **0** means the command's success contract; it does not represent an error code. Public failure classes **1–7** are catalogued. The additive snapshot retains the original `TIER_REFUSED` spelling; renaming it must fail the snapshot. Unknown/unmapped failure classes must not be silently accepted by the mapping checks.

The mapping regression pins each semantic code's documented default, not merely membership in a valid exit class: swapping two valid defaults is still a contract violation. Contextual exits remain the responsibility of each operation's consumer regression.

## One failure, three frontends

The parity contract uses a real missing-inventory target: HTTP responds non-success with `UNKNOWN_DEVICE`; the MCP read tool preserves that same code and metadata; the real CLI read reports `[UNKNOWN_DEVICE]` on stderr, leaves stdout empty, and retains its read-failure exit **1**. This is one shared failure, not three independently fabricated response fixtures.

A registry/table test alone does not establish frontend parity, installed operation, physical-device control, or complete release qualification. Those require the original executable consumers and their source-bound evidence.
