# Extracting the OpenHuman MCP client and registry into `tinymcp`

- **Status:** Accepted
- **Owner:** Maintainers
- **Plan:** [`../plans/mcp-extraction.md`](../plans/mcp-extraction.md)

## Problem

OpenHuman carries a complete Model Context Protocol client in-tree: two
transports, a static config-declared server set, a dynamic SQLite-backed
registry of user-installed servers, OAuth discovery, a subprocess supervisor,
and a write-audit log. It is roughly 15,000 lines under
`src/openhuman/mcp/`, and none of it is kernel work.

Three costs follow from that:

1. **Build weight.** The subsystem drags `reqwest`, the SQLite store, and the
   process-supervision machinery into a binary whose primary job is elsewhere.
   The existing `mcp` Cargo feature gates the *code* but sheds **zero**
   dependencies, because every one of them is load-bearing for some other
   domain. Gating cannot recover what only a boundary can.
2. **No reuse.** The transports are a generally useful MCP client. Anything
   else in this organization that wants to talk to an MCP server has to
   reimplement them or depend on all of OpenHuman.
3. **Blast radius.** The registry spawns arbitrary user-chosen subprocesses
   (`npx`, `uvx`, direct binaries) and speaks to arbitrary remote endpoints.
   That work belongs behind a declared interface with a versioned contract,
   not woven into the same module graph as the agent runtime.

## Goals

- Move the MCP **client** (both transports), the **static** config-declared
  server set, the **dynamic** registry, and the **audit** log out of OpenHuman
  and into this repository, preserving observable behavior exactly.
- Publish every type that crosses the boundary from a dependency-light
  contract crate, `tinymcp-bus`, so a host names payloads from a library rather
  than by string literal or a structural twin.
- Ship the implementation as a `cdylib` TinyBus module, `tinymcp`, following the
  same ABI, manifest, and digest gates as `tinydocs` and `tinywallet`.
- Give OpenHuman a consumption path that works **before** the first release:
  a vendored submodule plus path dependency, flipped to the loadable-module
  path once `tinymcp` has cut a release.
- Leave OpenHuman with no second implementation. The extracted code is deleted,
  not gated.
- Provide an optional `tools` feature that adapts advertised MCP tools to the
  shared `tinytools::Tool` interface. Calls remain behind the host's injected
  invoker; host-specific scanning and approval policy stay at the host edge.

## Non-goals

- **OpenHuman's server policy does not move.** That is OpenHuman acting *as*
  an MCP server, exposing its own curated tool surface to external hosts, and
  it stays bound to OpenHuman's tool registry, permission model, and agent turn
  machinery. A later stage did move the *generic* server half — protocol,
  session provenance, argument validators, stdio and Streamable HTTP
  transports — behind a handler trait; see [`mcp-server.md`](mcp-server.md).
- No protocol changes. The supported MCP protocol versions, the JSON-RPC
  framing, the SSE handling, and the OAuth discovery sequence are ported as-is.
- No new registry sources. Smithery and the official
  `modelcontextprotocol/registry` are ported; nothing is added.
- No change to the on-disk SQLite schema or the `mcp_clients` RPC namespace
  names. Existing user state must survive the move untouched.
- No process isolation. A TinyBus module is in-process native code; this
  extraction is a dependency and contract boundary, not a security sandbox.

## Proposed behavior

### Crate split

`tinymcp-bus` holds the vocabulary and nothing else — no transport, no runtime,
no behavior, no I/O. CI asserts it. It carries:

| Area | Contents |
| --- | --- |
| `names/` | `INTERFACE`, `OBJECT_PATH`, one constant per member, and `METHODS` |
| `config/` | `McpClientConfig`, `McpServerConfig`, `McpAuthConfig`, `HttpHeader`, `McpClientIdentityConfig`, `McpRegistryAuthConfig` |
| `transport/` | `McpRemoteTool`, `McpInitializeResult`, `McpServerToolResult`, `McpToolResult`, `McpSseEvent`, `McpAuthChallenge`, `McpAuthorizationContext`, `ProtectedResourceMetadata`, `AuthorizationServerMetadata` |
| `registry/` | `InstalledServer`, `McpTool`, `ConnStatus`, `ServerStatus`, `Transport`, `CommandKind`, and the Smithery / official-registry DTOs |
| `audit/` | the write-audit record types |
| `sanitize/` | `MAX_DESCRIPTION_BYTES`, `MAX_TITLE_BYTES` (presentation limits only) |
| `version/` | `CONTRACT_VERSION` and `is_compatible` |
| method payloads | one request and one response type per member |

`tinymcp` depends on `tinymcp-bus`, re-exports all of it, and holds the
behavior: the two transports, the spawn-environment probe, the static registry,
the dynamic registry with its store and supervisor, OAuth, the audit log, and
the TinyBus adapter.

### Generic lexical helpers

Generic stripping and UTF-8 truncation live in the TinyTools library, so skill
and harness metadata can use them without MCP loading. MCP implementation
re-exports the exact helper functions and retains its presentation byte caps.
The bus has vocabulary only. Contract-only MCP hosts request preparation over
the four bounded operations described in [pure vocabulary](pure-vocabulary.md).

### The interface

The module claims `ai.tinyhumans.tinymcp.Mcp` at `/ai/tinyhumans/tinymcp/Mcp`.
Members map one-to-one onto OpenHuman's existing `mcp_clients` and `mcp_setup`
operations, so the host's RPC surface is unchanged from the frontend's point of
view:

```text
RegistrySearch        RegistryGet           InstalledList
Install               Uninstall             SetEnabled
Connect               Disconnect            DetectAuth
OAuthBegin            UpdateEnv             Status
ListTools             ToolCall              ConfigAssist
RegistrySettingsGet   RegistrySettingsSet
SetupSearch           SetupGet              SetupRequestSecret
SetupSubmitSecret     SetupTestConnection   SetupInstallAndConnect
StaticList            StaticListTools       StaticCallTool
```

`StaticList` / `StaticListTools` / `StaticCallTool` are the static,
config-declared server set that OpenHuman's `mcp_list_servers`,
`mcp_list_tools`, and `mcp_call_tool` bridge tools drive today.

### Host seams

Seven things the extracted code reaches for today do not exist inside a module.
Each is resolved explicitly rather than by pulling OpenHuman in behind it:

| What it used | Resolution |
| --- | --- |
| `config::Config` | The host passes an `McpClientConfig` (contract type) in the module configuration blob and on the calls that need it. The module never reads a TOML file. |
| `config::apply_runtime_proxy_to_builder` | Proxy settings become explicit fields on the configuration payload the host hands over. |
| `core::bus::BUS`, `core::events::DomainEvent` | Lifecycle notifications become TinyBus **signals** the module emits. `registry/bus.rs` was pure `tracing` logging with no side effects and does not move. |
| `security::prompt_injection::scan_tool_definition` | Stays in OpenHuman. Tool-definition scanning is host policy: the module returns definitions verbatim and the host scans at its own edge, where the result feeds the host's own threat model. |
| `util::sanitize` | Generic helpers belong to TinyTools; host adapters migrate separately. |
| `skills::types::ToolResult` | Becomes `McpToolResult` in the contract crate. OpenHuman converts at its edge. |
| `agent::turn_origin`, `tools::traits::*` | The agent runtime and policy stay in OpenHuman. The optional `tinymcp::tools` adapter implements the shared `tinytools::Tool` interface and delegates execution through a host-provided invoker; OpenHuman retains its own bridge and policy. |
| SQLite file location | The host supplies a data directory in the module configuration. The filename stays `mcp_clients.db`. |

### OpenHuman consumption, in two steps

**Step one — vendored path dependency.** `vendor/tinymcp` becomes a git
submodule pinned by gitlink, exactly as `vendor/tinymemory` is today.
OpenHuman depends on `tinymcp` and `tinymcp-bus` by path and calls them
directly. The migrated code is deleted from `src/openhuman/mcp/` in the same
change. MCP keeps working with no release loop and no ABI in the way.

**Step two — loadable module.** Once `tinymcp` has cut a release, OpenHuman
adds a `TINYMCP` entry to `src/openhuman/modules/registry.rs` with per-platform
digests taken verbatim from the release's `checksum.toml`, adds a
`src/openhuman/modules/mcp.rs` host half, and drops the path dependency on
`tinymcp` — keeping only `tinymcp-bus`, which is the whole point of the split.
`LoadPolicy` is `Lazy`: a user who never installs an MCP server should not pay
a download, a `dlopen`, and a resident library that TinyBus will never unload.

## Invariants and constraints

- **Behavior is preserved exactly.** Every ported test comes with it and keeps
  passing. This is a move, not a rewrite; improvements are separate changes on
  top.
- **`tinymcp-bus` stays transport-free.** No `tinybus`, no async runtime, no
  HTTP client, no native library, no `rusqlite`. CI asserts the dependency tree.
- **One definition per payload.** `tinymcp::McpRemoteTool` and
  `tinymcp_bus::McpRemoteTool` are the same type. No parallel host vocabulary.
- **Serde representations are pinned by test.** Every payload type has a unit
  test fixing its wire form. A host and a module that disagree about a field
  name fail at runtime with a decode error, so the test is the only thing
  standing between a rename and a production decode failure.
- **On-disk compatibility.** The SQLite schema, the `mcp_clients.db` filename,
  and the RPC namespace names are unchanged. A user upgrading across this
  change keeps their installed servers.
- **Tool allow/deny enforcement stays fail-closed and pre-transport.** Empty
  names are rejected, the denylist wins over the allowlist, and the check runs
  before any network or subprocess I/O.
- **Endpoints are redacted everywhere they are logged.** `redact_endpoint`
  returns `<redacted>` for any URL carrying userinfo or a non-`http(s)` scheme.
- **Stdio children inherit the reconstructed PATH**, not the GUI-stripped
  process PATH, and the command is resolved before spawn so a missing `npx` or
  `uvx` surfaces install guidance rather than a raw `ENOENT`.
- **No secret is echoed back.** Registry-auth getters report whether a secret is
  set, never its value.
- Workspace rules apply unchanged: no `unsafe`, no `unwrap`/`expect`/`panic` in
  library paths, rustdoc on every public item, at least 90% line coverage in
  every source file.

## Acceptance criteria

- `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -D
  warnings`, `cargo build --all-targets --all-features`, and `cargo test
  --all-features` all pass in this repository.
- Every test that existed in `src/openhuman/mcp/{http_client,config_servers,
  registry,audit}` exists here and passes, including the four OpenHuman
  integration suites (`mcp_registry_multi_server`, `mcp_stdio_integration`,
  `mcp_registry_e2e`, `mcp_setup_e2e`), ported against the public API.
- The CI job asserting `tinymcp-bus` is transport-free passes.
- `cargo run -p tinymcp --example verify_module` loads the built `cdylib`
  through TinyBus and completes a round trip on the interface.
- In OpenHuman, after step one: the build succeeds with
  `src/openhuman/mcp/{http_client,config_servers,registry,audit}` deleted, and
  the four MCP integration suites pass against the vendored crate.
- An OpenHuman instance carrying an existing `mcp_clients.db` lists the same
  installed servers before and after the change.
- No file under `src/openhuman/` names `reqwest` for MCP purposes after step
  two, and the `mcp` Cargo feature is gone.

## Contract 1.2: closing the gaps a host hit (issue #4)

Contract 1.1 added the agent-tools family. Contract 1.2 adds the members and
payloads below; all additions are backward-compatible for hosts that do not
call the new members.

- **`OAuthComplete(state, code)`** finishes an authorization. The module
  resolves `state` to a server through its own pending-authorization map, so a
  host never supplies a server id.
- **The module owns the boot connect pass and the reconnect supervisor.** They
  start from `setup` (and for each opened directory) as background work, and
  loading never waits on them. They are not members: the supervisor's backoff
  state and liveness probe are module-internal.
- **`ConnectedOverview()`** returns the connected servers' identities and tools
  in one call, replacing an N+1 over `Status`, `InstalledList` and `ListTools`.
- **`ServerDetail`, `AuthDetection`, `AuthKind`** moved into `tinymcp-bus` so a
  host names the types the module returns. Serde forms are unchanged.
- **`RegistrySearchCurated(query, page, page_size, {tag_official, official_first})`**
  applies the module's curation list. It is a new member because arguments are
  positional and arity is checked, so a fourth argument on `RegistrySearch`
  would break every 1.0 host.
- **`ToolCallOutcome.rendered`** carries the rendered result (defaulted when
  absent). `redact_endpoint` and `render_tool_result`, both pure, moved into
  `tinymcp-bus`.
- **`errors`** in `tinymcp-bus` names each failure class. A failed call carries
  the name in its error header; the message is unchanged.
- **`Open(data_dir)`** on the root object serves that directory as its own
  object and returns the path. Idempotent per exact absolute path; the
  load-time directory maps to the root path; capped at 32 directories.

The library ships under its host-derived filename, `lib<id>_module.<ext>`, in
release archives; the Rust library keeps the name `tinymcp`.

## Open questions

- Does the audit log's storage move with it, or does the module call back to a
  host-provided store? Provisionally: it moves, with its own SQLite file, on the
  same reasoning as the registry store. Revisit if OpenHuman needs to query
  audit records in the same transaction as its own data.
- Should `StaticList` / `StaticListTools` / `StaticCallTool` stay on the same
  interface as the dynamic registry, or claim a second object? One interface
  provisionally; they share every transport primitive and splitting them would
  double the host's connection bookkeeping for no isolation gain.
