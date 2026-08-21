# Plan: Extracting the OpenHuman MCP client and registry into `tinymcp`

- **Specification:** [`../specs/mcp-extraction.md`](../specs/mcp-extraction.md)
- **Status:** In progress

## Goal

Move `src/openhuman/mcp/{http_client,config_servers,registry,audit}` into this
repository behind a versioned TinyBus contract, then delete it from OpenHuman.
Behavior is preserved exactly; every ported test comes with its code.

## Assumptions

- `src/openhuman/mcp/server/` stays in OpenHuman. It is the server side.
- OpenHuman consumes this repository as a vendored path dependency first and as
  a loadable module second. Both are in scope; the module step is last.
- The SQLite schema, the `mcp_clients.db` filename, and the RPC namespace names
  do not change. User state survives the move.

## Ordering, and why

The dependency graph dictates it. `sanitize` has no dependencies; the contract
types depend on `sanitize`; the transports depend on the contract types; the
static server set depends on the transports; the dynamic registry depends on
all of it; the bus adapter depends on the whole crate; and OpenHuman depends on
the finished thing. Each phase below builds and tests green on its own, so a
reviewer never has to hold two phases in their head at once.

Phases 1–3 are a pure move of self-contained code and can land independently.
Phase 4 is the bulk. Phase 6 is the only one that touches OpenHuman.

---

## Phase 0 — De-template the repository

- [x] Rename `crates/template` → `crates/tinymcp` and `crates/template-bus` →
      `crates/tinymcp-bus`, and update `name`, `description`, `keywords`, and
      `categories` in both manifests plus the root `[workspace.dependencies]`.
- [x] Point `[workspace.package] repository` at `tinyhumansai/tinymcp`.
- [ ] Rename the interface, object path, and member constants in
      `crates/tinymcp-bus/src/names/`, and the matching `provides` / `methods`
      declarations in `crates/tinymcp/src/tinybus_module/`.
- [ ] Delete the placeholder `greeting` module from both crates once Phase 2
      has a real payload family to replace it with.
- [ ] Rewrite `README.md`, `MODULE.md`, `ROADMAP.md`, and both `src/lib.rs`
      crate docs for this project; delete the example spec and plan.
- [ ] Add the workspace dependencies the port needs: `reqwest`, `tokio`
      (process + io + sync), `rusqlite`, `parking_lot`, `anyhow`, `base64`,
      `futures-util`, `tracing`, `url`, and `schemars` as an optional feature of
      the contract crate.

**Verify:** `cargo build --all-targets --all-features`.

---

## Phase 1 — `sanitize` into the contract crate

Source: `src/openhuman/util/sanitize.rs` (252 lines, no dependencies).

- [ ] Create `crates/tinymcp-bus/src/sanitize/{mod.rs,test.rs}`; move the
      module verbatim, keeping `MAX_DESCRIPTION_BYTES`, `MAX_TITLE_BYTES`,
      `strip_control_chars`, `strip_instruction_fences`, `truncate_utf8_safe`,
      and `sanitize_for_llm`.
- [ ] Move its test suite into `test.rs` unchanged — this is a security-relevant
      truncation-and-stripping rule and the tests are the specification of it.
- [ ] Re-export the four functions and two constants from
      `crates/tinymcp-bus/src/lib.rs`.

**Verify:** `cargo test -p tinymcp-bus sanitize`.

---

## Phase 2 — The contract crate

Every type that crosses the boundary, one directory per family, each with
`mod.rs` / `types.rs` / `test.rs`. **Each type gets a unit test pinning its
serde representation** before the type is considered done; that test is the
only thing standing between a field rename and a production decode failure.

- [ ] `crates/tinymcp-bus/src/config/` — `McpClientConfig`, `McpServerConfig`,
      `McpAuthConfig`, `HttpHeader`, `McpClientIdentityConfig`,
      `McpRegistryAuthConfig`, from
      `src/openhuman/config/schema/tools/mcp.rs`. Replace `schemars::JsonSchema`
      with a `#[cfg_attr(feature = "schemars", derive(JsonSchema))]` behind an
      optional `schemars` feature so OpenHuman's desktop schema still generates.
      Replace `super::super::defaults` with local `default_true`.
      Add the explicit proxy fields that replace
      `config::apply_runtime_proxy_to_builder`.
- [ ] `crates/tinymcp-bus/src/transport/` — `McpRemoteTool`,
      `McpInitializeResult`, `McpServerToolResult`, `McpSseEvent`,
      `McpAuthChallenge`, `McpAuthorizationContext`,
      `ProtectedResourceMetadata`, `AuthorizationServerMetadata`, and
      `McpToolResult` (the shape `skills::types::ToolResult` supplied).
      `McpRemoteTool`'s sanitized display accessors move with it and call into
      Phase 1.
- [ ] `crates/tinymcp-bus/src/registry/` — `InstalledServer`, `McpTool`,
      `ConnStatus`, `ServerStatus`, `Transport`, `CommandKind`, and the
      Smithery and official-registry DTOs, from
      `src/openhuman/mcp/registry/types.rs` (706 lines).
- [ ] `crates/tinymcp-bus/src/audit/` — the record types from
      `src/openhuman/mcp/audit/types.rs`.
- [ ] `crates/tinymcp-bus/src/method/` — one request and one response type per
      member listed in the specification.
- [ ] `crates/tinymcp-bus/src/names/` — `INTERFACE =
      "ai.tinyhumans.tinymcp.Mcp"`, `OBJECT_PATH = "/ai/tinyhumans/tinymcp/Mcp"`,
      one constant per member, and `METHODS` in dispatch order.
- [ ] Reset `CONTRACT_VERSION` to `(1, 0)`.
- [ ] Re-export the whole surface from `crates/tinymcp-bus/src/lib.rs` and
      rewrite the crate docs.

**Verify:** `cargo test -p tinymcp-bus`, plus the CI job asserting the crate
pulls in no transport.

---

## Phase 3 — Transports

- [ ] `crates/tinymcp/src/error/mod.rs` — extend the crate-wide `Error` with the
      variants the ported code needs, replacing the `anyhow` returns at the
      public surface. Internal `anyhow` use may stay; the public boundary
      returns `Result<T>`.
- [ ] `crates/tinymcp/src/transport/http/` — `McpHttpClient` from
      `http_client/client.rs` (828 lines) and `client_helpers.rs` (160), with
      `client_tests.rs` (670) as `test.rs`. Protocol-version negotiation, SSE
      draining, session lifecycle, `WWW-Authenticate` parsing, the
      reinitialize-and-retry-once rule, `x-mcp-header` mirroring,
      `render_tool_result`, and `redact_endpoint` all move unchanged.
- [ ] `crates/tinymcp/src/transport/spawn_env/` — from
      `config_servers/spawn_env.rs` (524 lines). The login-shell PATH probe and
      the up-front command resolution.
- [ ] `crates/tinymcp/src/transport/stdio/` — `McpStdioClient` from
      `config_servers/stdio.rs` (314 lines).
- [ ] Hoist `SUPPORTED_PROTOCOL_VERSIONS` and `LATEST_PROTOCOL_VERSION` into one
      place. They are currently duplicated across the two transports; the move
      is the moment to stop that, and a test asserts both transports negotiate
      from the same list.

**Verify:** `cargo test -p tinymcp transport`.

---

## Phase 4 — The registries

- [ ] `crates/tinymcp/src/config_servers/` — `McpServerRegistry`,
      `McpServerDefinition`, `McpTransportClient`, `McpRegistrySource` from
      `config_servers/registry.rs` (592 lines). Built from the contract
      `McpClientConfig` rather than OpenHuman's `Config`. Allow/deny
      enforcement stays fail-closed and pre-transport; its tests come with it.
- [ ] `crates/tinymcp/src/registry/store/` — the SQLite store (965 lines).
      Schema unchanged. The data directory arrives from module configuration.
- [ ] `crates/tinymcp/src/registry/sources/` — `smithery.rs` (268) and
      `mcp_official.rs` (1438), plus the 10-minute cache in `registry.rs` (498).
- [ ] `crates/tinymcp/src/registry/connections/` — the live connection map
      (979 lines), and `supervisor/` (223).
- [ ] `crates/tinymcp/src/registry/oauth/` — OAuth discovery and callback (618).
- [ ] `crates/tinymcp/src/registry/curation/` (174) and `boot/` (110), with
      `boot_tests.rs` as `boot/test.rs`.
- [ ] `crates/tinymcp/src/registry/setup/` — `setup.rs` (327) and the
      `setup_ops.rs` operations (690), minus the OpenHuman agent invocation:
      `mcp_setup_config_assist` reaches `agent::turn_origin` to run an
      OpenHuman agent turn. That call is host policy and stays in OpenHuman;
      the module's `ConfigAssist` member returns the prepared prompt context
      and the host runs the turn.
- [ ] `crates/tinymcp/src/registry/ops/` — the operation bodies from `ops.rs`
      (1057), returning contract response types instead of `RpcOutcome<Value>`.
      `schemas.rs` (1268) does **not** move: it is OpenHuman's RPC controller
      wiring and is replaced by the bus adapter in Phase 5.
- [ ] Drop `registry/bus.rs`. It was pure `tracing` logging over `DomainEvent`
      with no side effects; the module logs directly and emits signals instead.
- [ ] `crates/tinymcp/src/audit/` — store, schemas, and types from `audit/`.

**Verify:** `cargo test -p tinymcp`, and the four OpenHuman integration suites
ported into `crates/tinymcp/tests/`.

---

## Phase 5 — The bus adapter

- [ ] `crates/tinymcp/src/tinybus_module/mod.rs` — one `#[tinybus::interface]`
      method per member, each deserializing a contract request type and
      returning a contract response type, delegating to Phase 4.
- [ ] Declare `provides`, `methods`, and `signals` in `module_export!`, and
      assert the served members against `tinymcp_bus::names::METHODS` in order,
      so a member added to one and not the other fails a test here rather than
      surfacing as an unknown method in a host.
- [ ] Module configuration: data directory, `McpClientConfig`, and registry
      auth, parsed from the configuration blob with a typed error on a
      malformed one.
- [ ] Update `examples/verify_module.rs` and `examples/verify_github_release.rs`
      to exercise a real member.

**Verify:** `cargo run -p tinymcp --example verify_module`.

---

## Phase 6 — OpenHuman

Step one, the path dependency:

- [ ] Add `vendor/tinymcp` as a git submodule pinned by gitlink.
- [ ] Depend on `tinymcp` and `tinymcp-bus` by path, with a comment on each
      entry saying why, per that repository's dependency rules.
- [ ] Repoint `src/openhuman/mcp/registry/schemas.rs` and the `mcp_setup`
      controllers at the vendored crate. The RPC surface the frontend sees does
      not change.
- [ ] Repoint the agent-tool bridge in `src/openhuman/tools/impl/network/mcp.rs`
      and `mcp_setup.rs`, and the bespoke `gitbooks` tool, at
      `tinymcp::McpHttpClient`.
- [ ] Keep `scan_tool_definition` at the host edge, applied to the definitions
      the module returns.
- [ ] Repoint `util::sanitize` consumers at `tinymcp_bus::sanitize` and delete
      the OpenHuman copy.
- [ ] Delete `src/openhuman/mcp/{http_client,config_servers,registry,audit}`
      and the now-empty `mcp` Cargo feature. Keep `src/openhuman/mcp/server/`.
- [ ] Verify an existing `mcp_clients.db` still lists the same servers.

Step two, the loadable module, after the first `tinymcp` release:

- [ ] Add a `TINYMCP` `ModuleRecord` to `src/openhuman/modules/registry.rs`
      with per-platform digests taken **verbatim from the release's
      `checksum.toml`**, never recomputed from a local build.
- [ ] Add `src/openhuman/modules/mcp.rs`, the host half, following
      `documents.rs`.
- [ ] `LoadPolicy::Lazy`.
- [ ] Drop the path dependency on `tinymcp`, keeping `tinymcp-bus`. That
      removal is the whole point of the split; if the dependency tree does not
      shrink, this phase did not land.

**Verify:** in OpenHuman, its own four contract commands plus the four MCP
integration suites.

---

## Verification

Focused, while iterating:

```sh
cargo test -p tinymcp-bus
cargo test -p tinymcp transport
```

Full, before declaring any phase done:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --all-targets --all-features
cargo test --all-features
cargo run -p tinymcp --example verify_module
```

## Completion checklist

- [x] Phase 0 — de-template (partial: renames and metadata done)
- [ ] Phase 1 — `sanitize`
- [ ] Phase 2 — contract crate
- [ ] Phase 3 — transports
- [ ] Phase 4 — registries
- [ ] Phase 5 — bus adapter
- [ ] Phase 6 — OpenHuman
