# Bounded metadata preparation

The module owns argument normalization and MCP display/output preparation.
Requests use bus DTOs; service methods delegate directly to this module. Generic
lexical helpers are re-exported from TinyTools rather than duplicated.

Each request is serialized through a capped writer before processing. The
1 MiB budget includes request JSON overhead. Tool output is written incrementally
with the same raw UTF-8 output cap, including pretty JSON indentation and join
separators. A compact deeply nested JSON input can exceed that output budget;
the writer refuses its next chunk without first building the full output.

TransformText takes a caller-selected lexical operation and cap. DisplayRemoteTool
applies established MCP description/title caps. NormalizeToolArguments preserves
tolerant object/fenced JSON forms and precise refusal wording. RenderToolOutput
preserves text-only, all-block and explicit Markdown preference projections.
Approvals, dispatch and presentation preference remain host choices. Shared
library extension methods retain legacy unbounded behavior; the module entry
points add admission/output bounds.
