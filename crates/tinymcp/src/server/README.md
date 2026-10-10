# `server` — serving the Model Context Protocol

The rest of this crate is an MCP *client*. This module is the other side: it
lets a host expose its own tools and resources to MCP clients (Claude Desktop,
Cursor, another agent) without reimplementing the protocol.

## The seam

A host implements [`McpServerHandler`](types.rs):

| Method | Decides |
| --- | --- |
| `server_info` | `serverInfo` and `instructions` in the `initialize` result |
| `source_type_prefix` | the base of each session's provenance (default `mcp`) |
| `list_tools` | the `tools/list` catalog, as `ServerToolSpec`s |
| `call_tool` | what running a tool does |
| `list_resources` / `read_resource` | the resource catalog (none by default) |

Everything else is this module's: JSON-RPC framing, batching, notifications,
protocol version negotiation, method routing, and every error shape. Async
methods return `BoxFuture`, so the trait is object-safe and transports hold an
`Arc<dyn McpServerHandler>`. These are in-process Rust types; nothing here
crosses the TinyBus contract.

A handler receives a `RequestContext` with the session's source type and the
request's transport headers. A host that carries state across an HTTP hop
(OpenHuman's subagent delegation depth, for instance) reads its header there;
the transport knows nothing about it. `RequestHeaders`' `Debug` prints names
only, since `authorization` carries a bearer token.

## Files

| Path | Role |
| --- | --- |
| `types.rs` | the trait, `ServerInfo`, `ServerToolSpec`, `ResourceSpec`, `RequestContext`, `ToolCallError` |
| `protocol/` | `handle_line` / `handle_value`: one message or batch in, responses out |
| `session/` | `ClientSession`: `clientInfo.name` → `<prefix>:<slug>`, first observation wins |
| `args/` | argument validators handlers share, so rejections read alike |
| `stdio/` | `run_stdio`: newline-delimited JSON-RPC over a reader/writer |
| `http/` | `run_http` / `run_http_reporting`: Streamable HTTP + SSE (`server-http`) |

## Wire guarantees

- Output is compact `serde_json`; object keys are emitted in `serde_json`'s map
  order, so a host that does not enable `preserve_order` gets sorted keys.
- `ToolCallError` picks the error code: `InvalidParams` → `-32602`,
  `Internal` → `-32603`, `ResourceNotFound` → `-32002`.
- `tools/call` arguments go through `tinymcp_bus::normalize_tool_arguments`:
  absent or `null` is `{}`, a JSON-encoded object is decoded.
- `resources/templates/list` is always an empty list.
- Over HTTP each POST is dispatched on a fresh session: only the negotiated
  protocol version is kept per session id. A batch body has no top-level `id`
  and is therefore answered `204 No Content`. Both are long-standing wire
  behavior, pinned by tests.
- Auth rejections are `401` with `text/plain`; session rejections are
  plain-text `400`/`404` with fixed messages.


The compiled-module adapter uses [opaque server sessions](bridge/README.md) and
host callbacks for tools, resources and prompts. Prompt-capable handlers advertise
`prompts` and answer `prompts/list` and `prompts/get`; existing handlers keep their
previous capabilities through the default trait methods.
