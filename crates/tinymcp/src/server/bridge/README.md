# Compiled-module server protocol

`ServerSessions` owns opaque session identifiers and runs the existing server
protocol. The bus service publishes seven positional operations:

| Member | Arguments | Result |
| --- | --- | --- |
| ServerOpen | config | opaque session string |
| ServerSubmit | session, headers, input | unit |
| ServerPoll | session | operation id plus pending, callback, complete, cancelled or failed |
| ServerComplete | session, callback id, reply | unit |
| ServerCancel | {session_id, operation_id} | unit |
| ServerClose | session | unit |
| ServerShutdown | none | closed session count |

Generate `session_id` as a UUID before calling `ServerOpen`, then use
`tinymcp_bus::ServerSessionConfig` for identity (`name`, `version`, optional
`instructions`), provenance prefix and resource declarations. Declaration values
are ordinary MCP JSON. `ServerHostCall` explicitly identifies tool listing, tool
execution, resource reads and prompt listing/resolution; every callback carries
module-derived provenance and operation headers. The host must apply its own
credential checks, security, approvals and domain dispatch before completing it.
Callbacks are not authority grants, and callback identifiers are not credentials.
Only trusted callers may access these methods, like the module's existing APIs.

There are at most 32 sessions per registry object, one active operation per
session, and one outstanding callback per operation. Input, headers, declarations,
callbacks and aggregate host replies each have a 1 MiB byte ceiling. A final
response also has that ceiling. A host polls pending states with its own event
loop scheduling; repeated polls return the same outstanding callback until completion, so a lost
poll reply cannot strand it. Hosts deduplicate execution by callback identifier. Completion identifiers
are single-use; invalid or oversized completions leave the callback outstanding.

Repeated `ServerOpen` with the same identifier and declarations returns the same
session. Identifiers are never reused: close records a tombstone even before an
open, so a late acquisition cannot resurrect cancelled state. At most 4096
reservations (active plus tombstones) exist per object. A lost open reply can be
recovered or closed using the identifier the caller already knows.

Submit a `ServerInput` containing a caller-generated `operation_id` UUID and the
MCP line. Retrying the same identifier, line and headers is idempotent, including
after completion. Conflicting retries and previously retired identifiers are
refused. Poll returns `ServerOperationSnapshot`, including the identifier, so a
late poll reply cannot be mistaken for a successor operation. Terminal responses
remain replayable until a new accepted operation explicitly acknowledges them.
Each session retains at most 4096 used operation identifiers; open a new session
after that limit. `ServerCancel` targets both session and operation; a stale
cancellation cannot abort a successor. Cancellation aborts and joins the
protocol future and drops outstanding callback channels. Close and shutdown also
join workers before returning. Batches contain at most 256 elements. Closing a session and
shutdown also release all state. Shutdown is scoped to the registry object; it
does not stop its installed-client supervisor or sessions on other directories.

The protocol handles malformed JSON, notifications, batches, negotiation and
JSON-RPC errors. Per-session declarations are frozen; opening a new session is
how the host changes identity/resources. Tools and prompts are requested from the
host for each relevant MCP request. The stdio and HTTP library entrypoints keep
working; module-owned listeners are a separate migration slice. No host should
reimplement MCP framing around these operations.
