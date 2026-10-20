# tinymcp-bus

Shared serialized MCP vocabulary, fixed tool declarations, errors, method names
and contract version. The implementation re-exports the same definitions.
Normal/build dependency closures contain only serialization/schema utilities
and `base64`, including with all features enabled. No optional runtime, transport, native or
TinyTools dependency belongs here.

DTO constructors, fixed catalog/schema declarations and serde helpers are
vocabulary. Protocol routing, transport, argument normalization, dynamic
metadata preparation, query filtering, persisted-value parsing and redaction
execute in the implementation. Generic lexical helpers live in TinyTools.
The sanitize module retains only MCP presentation limits.

Contract 1.9 adds four one-argument operations: TransformText,
NormalizeToolArguments, DisplayRemoteTool and RenderToolOutput. Their requests
and output are capped at 1 MiB. Rendering applies its cap incrementally, before
buffering oversized pretty JSON or joined content. Existing member arities and
wire forms are preserved. Prefer member constants from names::methods.

Contract 1.10 adds the one-argument `OpenConfigured` operation and its
`OpenConfiguredRequest` payload. It pairs a data directory with that host's
MCP client settings; `Open` and its original string argument remain unchanged.
Because those settings may contain credentials, callers send this request using
TinyBus's confidential-call path. Contract 1.11 adds `Close(data_dir)` and the
nonblocking `TransitionStatus(id)` observation. `OpenConfigured` returns an
`OpenConfiguredResponse` carrying the object path and lifecycle transition id;
replacement and old-service cleanup are complete only when the status is
`complete`. The root remains available after close, with no persistent
directory or static client configuration.

Contract 1.12 adds no-argument `CachedOverview`, which returns live tools for
connected installs and persisted, fingerprint-matched cached tools for enabled
disconnected installs without dialling them. `StaticCachedListTools(server)`
reads one static server's persisted cache without a network request;
`StaticListTools(server)` keeps its response shape and refreshes that cache
after a successful listing. Both cached reads use the current definition's
fingerprint and reapply its allow and deny rules.

Contract 1.14 adds module-owned configuration-document read and apply members.
The apply request can contain credentials and therefore requires TinyBus's
confidential-call path. Its response includes the re-rendered document, which
contains credential names but never credential values, and the ordered change
report.

Contract 1.21 adds identifier normalization to `TransformText`. The module
uses the generic MCP bridge's existing rules, preserving plain punctuation and
removing sentence punctuation only when it follows a markdown wrapper.

Contract 1.23 adds pure helpers hosts used to copy: `require_identifier`,
`ServerDetail::server_json` (the detail screen's JSON shape with
`required_env_keys`) and `explain_not_connected` (how a `NotConnected` answer
reads against the installs). No member was added or removed.

Contract 1.24 carries each tool's `_meta` on `McpTool`, so `ListTools`,
`ConnectedOverview` and `CachedOverview` report an installed server's MCP Apps
`ui` resource reference as `StaticListTools` already did. It also adds
`ReadResource(server_id, uri)` for installed servers and
`StaticReadResource(server, uri)` for host-declared ones. Both return the
`resources/read` contents verbatim, refuse anything above `MAX_RESOURCE_BYTES`
with `ResourceTooLarge`, and redact the server's known credentials from the
contents' text and `_meta`.

Contract 1.25 moves the pure tool-UI rules (template resolution, widget
document checks, link classification and extraction, the widget tool-call and
rendering policy) from `tinymcp::ui` into `ui`, so a host renders tool UI
without linking the module crate. `tinymcp::ui` re-exports them. Link
classification splits `http(s)` hosts by hand, fail-closed, instead of with a
URL parser. No member was added or removed. Behaviour change (fail-closed):
`http(s)` links with userinfo, IDN/punycode or %-encoded hosts, IPv6,
non-canonical IPv4, bad ports or dot segments are now `Blocked` (previously
`Web`).

Library consumers bring implementation extension traits into scope for moved
DTO methods. Contract-only hosts call the compiled module. See
[the relocation specification](../../docs/specs/pure-vocabulary.md) for public
API compatibility and remaining transport/host integration work.

The stateful client-side receipt machine and admission table live in the
`tinymcp-client` crate. Consumers migrate Rust imports from either
`tinymcp_bus::client_lifecycle` or
`tinymcp::tinymcp_bus::client_lifecycle` to
`tinymcp_client::client_lifecycle`; the TinyBus contract keeps only serialized
lifecycle request and response types.
