# tinymcp-bus

Shared serialized MCP vocabulary, fixed tool declarations, errors, method names
and contract version. The implementation re-exports the same definitions.
Normal/build dependency closures contain only serialization/schema utilities,
including with all features enabled. No optional runtime, transport, native or
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

Library consumers bring implementation extension traits into scope for moved
DTO methods. Contract-only hosts call the compiled module. See
[the relocation specification](../../docs/specs/pure-vocabulary.md) for public
API compatibility and remaining transport/host integration work.
