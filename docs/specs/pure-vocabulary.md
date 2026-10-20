# Pure MCP vocabulary and bounded preparation

The contract owns DTOs, wire constants, fixed tool declarations, the static
storage-needs declaration, and error mappings. TinyMCP re-exports identical bus-owned server info, tool/resource
specifications, request headers/context and ToolCallError. Existing protocol,
persisted registry identifiers, declarations and error wire forms remain pinned
by golden fixtures. Raw remote metadata stays raw in serialized DTOs.

Generic lexical sanitization lives in TinyTools 0.6 at reviewed canonical
commit 1ec3468; TinyMCP imports it from `tinytools::sanitize` with default
features disabled. This keeps skills and harness metadata independent of MCP
loading. The bus has no TinyTools, TinyBus, runtime, HTTP or native
dependencies, even optionally.

## Public Rust API compatibility

Serialized type identity and simple constructors/getters remain. Foreign DTOs
cannot carry inherent implementation algorithms. Library consumers import these
TinyMCP extension traits to keep method syntax:

| DTO | Extension trait | Behavior |
| --- | --- | --- |
| McpRemoteTool | McpRemoteToolExt | sanitized description/title |
| McpToolResult | McpToolResultExt | text/output/LLM rendering |
| McpWriteListQuery | McpWriteListQueryExt | bounds and normalized filters |
| CommandKind / Transport | CommandKindExt / TransportExt | persisted parse fallbacks |
| McpRegistryAuthConfig | McpRegistryAuthConfigExt | redaction |
| RequestHeaders / RequestContext | RequestHeadersExt / RequestContextExt | header normalization/lookup |
| ServerToolSpec / ResourceSpec | ServerToolSpecExt / ResourceSpecExt | protocol JSON projection |

normalize_tool_arguments remains available from TinyMCP. Generic sanitizer
functions remain available from `tinymcp::sanitize` and
`tinytools::sanitize`.
Contract imports of these algorithms must migrate to owner library APIs or
module calls. This is a necessary pre-1.0 Rust source API break requiring a minor
package release; the reviewed source commit sets the package version, which the
release workflow validates. Fixed
RegistryTool::spec and registry_tool_specs remain in the bus unchanged.

Callers migrating from the previous source API should use
`tinymcp::normalize_tool_arguments` instead of the bus-root helper,
`tinymcp::sanitize::{sanitize_for_llm, strip_control_chars,
strip_instruction_fences, truncate_utf8_safe}` (or
`tinytools::sanitize::{sanitize_for_llm, strip_control_chars,
strip_instruction_fences, truncate_utf8_safe}`) instead
of bus sanitizers, and import `McpRemoteToolExt` / `McpToolResultExt` for the
display and rendering methods. The DTOs themselves retain their serde wire
forms; only executable helpers move to the implementation or their owner
library.

## Additive contract 1.9 operations

All four members take one argument; existing member arities stay unchanged.
TransformText takes an explicit transform/text/cap request. NormalizeToolArguments
accepts null, object or tolerant model argument input. DisplayRemoteTool returns
sanitized optional description/title using MCP's established 1024/128 byte caps.
RenderToolOutput takes the shared result, explicit projection and host Markdown
preference. It preserves text, pretty JSON and Markdown fallback behavior.

Serialized requests and generated output are bounded at 1 MiB. Rendering writes
incrementally and stops at its budget before further pretty JSON or join
allocation. Failures use the established InvalidArgument module error. Host
approval, dispatch and presentation policy remain explicit host choices.

## Additive contract 1.10 directory configuration

`Open(data_dir)` keeps its existing one-string tuple and load-time client
settings. `OpenConfigured(request)` adds a one-argument `OpenConfiguredRequest`
that pairs an absolute data directory with that host's complete
`McpClientConfig`. The module therefore keeps storage, static servers,
credentials, client identity, registry settings and proxy isolated per opened
workspace even when one module instance serves several workspaces.

Opening the same directory with the same settings is idempotent. If its client
settings change, the module replaces the service at the same object path after
current bus calls finish, then stops the old service's supervisor, protocol
operations and dynamic transports. This prevents stale endpoints or tokens
without invalidating existing object paths. The root object keeps its directory
opener across its own refresh. Legacy `Open` remains idempotent and keeps its
original load-time settings. The client settings can contain secrets, so hosts
call `OpenConfigured` through the confidential TinyBus path; formatting the
request for diagnostics omits its path and client fields.

## Additive contract 1.11 directory lifecycle

`OpenConfigured` returns `OpenConfiguredResponse { object_path,
transition_id }`, so the host can distinguish an accepted replacement from
completed cleanup. `TransitionStatus(transition_id)` is a short nonblocking
query with `pending`, `complete`, `failed`, and `unknown` states. It does not
hold a wait under TinyBus dispatch; hosts poll it asynchronously. `Close` keeps
its one-string argument and returns an optional transition id: `None` means the
directory is already closed, while `Some(id)` requires status polling until
cleanup completes. Closing a child removes its object path and joins its
maintenance, protocol, and dynamic-connection work. Closing the root replaces
it with an empty in-memory service, resets legacy client settings, and keeps
the root lifecycle surface alive.

One lifecycle transition may run at a time. Conflicting open, reconfigure, or
close requests fail quickly while it is pending; callers poll and retry. The
active-directory bound includes pending closes, and monotonic object ids prevent
a stale path from naming a later workspace. An unknown or retired transition
id does not claim cleanup succeeded.

## Additive contract 1.12 cached tool reads

`CachedOverview()` returns every enabled installed server's live tools when
connected, or its persisted tool-cache entry when disconnected, without
dialling it. The existing `ConnectedOverview()` remains live-only. The static
`StaticCachedListTools(server)` operation reads the persisted cache for one
host-declared server without connecting; `StaticListTools(server)` retains its
existing response and refreshes the cache after a successful listing. Both
reads use the current definition fingerprint and reapply its allow/deny rules,
so edits cannot expose metadata from an older endpoint or relaxed policy.

## Additive contract 1.21 identifier normalization

`TransformText` adds `NormalizeIdentifier`, which delegates to the generic MCP
bridge's existing model-markdown rule. It removes surrounding markdown
wrappers, but preserves plain punctuation unless sentence punctuation follows
one of those wrappers.

## Remaining work

This slice does not wire OpenHuman adapters, stdio/HTTP listener ownership,
server lifetime orchestration or additional supervisor notifications. Existing
module-owned protocol callbacks remain available, but transport/server adapter
migration still needs its own lifecycle and fault fixtures. No root gitlinks,
release digest or host switch is updated before compatible reviewed artifacts
are released.

## Client lifecycle helper ownership

`ConfiguredLifecycle` and `ClientTable` implement client-side state transitions,
admission, retirement, and cleanup reservations. They belong in the separate
`tinymcp-client` crate, which depends on `tinymcp-bus` for the lifecycle DTOs.
Consumers migrate Rust imports from either `tinymcp_bus::client_lifecycle` or
`tinymcp::tinymcp_bus::client_lifecycle` to
`tinymcp_client::client_lifecycle`. The bus crate does not forward these helpers.
The move does not change TinyBus wire names, payloads, or `CONTRACT_VERSION`.
