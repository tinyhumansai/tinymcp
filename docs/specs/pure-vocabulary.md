# Pure MCP vocabulary and bounded preparation

The contract owns DTOs, wire constants, fixed declaration/schema catalogs and
error mappings. TinyMCP re-exports identical bus-owned server info, tool/resource
specifications, request headers/context and ToolCallError. Existing protocol,
persisted registry identifiers, declarations and error wire forms remain pinned
by golden fixtures. Raw remote metadata stays raw in serialized DTOs.

Generic lexical sanitization moves unchanged to TinyTools at reviewed canonical
commit fa61666; TinyMCP uses that library with default features disabled. This
keeps skills and harness metadata independent of MCP loading. The bus has no
TinyTools, TinyBus, runtime, HTTP or native dependencies, even optionally.

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
functions remain available from tinymcp::sanitize and tinytools::sanitize.
Contract imports of these algorithms must migrate to owner library APIs or
module calls. This is a necessary pre-1.0 Rust source API break requiring a minor
package release; the release workflow owns package versioning. Fixed
RegistryTool::spec and registry_tool_specs remain in the bus unchanged.

Callers migrating from the previous source API should use
`tinymcp::normalize_tool_arguments` instead of the bus-root helper,
`tinymcp::sanitize::{sanitize_for_llm, strip_control_chars,
strip_instruction_fences, truncate_utf8_safe}` (or TinyTools directly) instead
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

## Remaining work

This slice does not wire OpenHuman adapters, stdio/HTTP listener ownership,
server lifetime orchestration or additional supervisor notifications. Existing
module-owned protocol callbacks remain available, but transport/server adapter
migration still needs its own lifecycle and fault fixtures. No root gitlinks,
release digest or host switch is updated before compatible reviewed artifacts
are released.
