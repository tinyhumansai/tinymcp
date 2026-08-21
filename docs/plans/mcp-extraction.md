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
- [x] Rename the interface, object path, and member constants in
      `crates/tinymcp-bus/src/names/`. The matching `provides` / `methods`
      declarations move with the adapter in Phase 5.
- [x] Delete the placeholder `greeting` module from both crates.
- [ ] Rewrite `README.md`, `MODULE.md`, `ROADMAP.md`, and both `src/lib.rs`
      crate docs for this project; delete the example spec and plan.
- [x] Add the workspace dependencies the port needs: `reqwest`, `tokio`
      (process + io + sync), `rusqlite`, `parking_lot`, `anyhow`, `base64`,
      `futures-util`, `tracing`, `url`, and `schemars` as an optional feature of
      the contract crate.

**Verify:** `cargo build --all-targets --all-features`.

---

## Phase 1 — `sanitize` into the contract crate

Source: `src/openhuman/util/sanitize.rs` (252 lines, no dependencies).

- [x] Create `crates/tinymcp-bus/src/sanitize/{mod.rs,test.rs}`; move the
      module verbatim, keeping `MAX_DESCRIPTION_BYTES`, `MAX_TITLE_BYTES`,
      `strip_control_chars`, `strip_instruction_fences`, `truncate_utf8_safe`,
      and `sanitize_for_llm`.
- [x] Move its test suite into `test.rs` — this is a security-relevant
      truncation-and-stripping rule and the tests are the specification of it.
- [x] Re-export the four functions and two constants from
      `crates/tinymcp-bus/src/lib.rs`.

**Verify:** `cargo test -p tinymcp-bus sanitize`.

---

## Phase 2 — The contract crate

Every type that crosses the boundary, one directory per family, each with
`mod.rs` / `types.rs` / `test.rs`. **Each type gets a unit test pinning its
serde representation** before the type is considered done; that test is the
only thing standing between a field rename and a production decode failure.

- [x] `crates/tinymcp-bus/src/config/` — `McpClientConfig`, `McpServerConfig`,
      `McpAuthConfig`, `HttpHeader`, `McpClientIdentityConfig`,
      `McpRegistryAuthConfig`, from
      `src/openhuman/config/schema/tools/mcp.rs`. Replace `schemars::JsonSchema`
      with a `#[cfg_attr(feature = "schemars", derive(JsonSchema))]` behind an
      optional `schemars` feature so OpenHuman's desktop schema still generates.
      Replace `super::super::defaults` with local `default_true`.
      Add the explicit proxy fields that replace
      `config::apply_runtime_proxy_to_builder`: the host resolves the proxy
      decision and sends `McpProxyConfig`, so the scoping policy stays in the
      one place that owns it.
- [x] `crates/tinymcp-bus/src/transport/` — `McpRemoteTool`,
      `McpInitializeResult`, `McpServerToolResult`, `McpSseEvent`,
      `McpAuthChallenge`, `McpAuthorizationContext`,
      `ProtectedResourceMetadata`, `AuthorizationServerMetadata`, and
      `McpToolResult` (the shape `skills::types::ToolResult` supplied).
      `McpRemoteTool`'s sanitized display accessors move with it and call into
      Phase 1.
- [x] `crates/tinymcp-bus/src/registry/` — `InstalledServer`, `McpTool`,
      `ConnStatus`, `ServerStatus`, `Transport`, `CommandKind`, and the
      Smithery and official-registry DTOs, from
      `src/openhuman/mcp/registry/types.rs` (706 lines).
- [x] `crates/tinymcp-bus/src/audit/` — the record types from
      `src/openhuman/mcp/audit/types.rs`.
- [x] `crates/tinymcp-bus/src/method/` — a reply type per operation whose
      answer is more than a value already modelled. There is deliberately **no
      success-or-error envelope**: a failure crosses the bus as a failure, so a
      caller does not unwrap two layers to learn whether anything went wrong.
- [x] `crates/tinymcp-bus/src/names/` — `INTERFACE =
      "ai.tinyhumans.tinymcp.Mcp"`, `OBJECT_PATH = "/ai/tinyhumans/tinymcp/Mcp"`,
      one constant per member, and `METHODS` in dispatch order.
- [x] Reset `CONTRACT_VERSION` to `(1, 0)`.
- [x] Re-export the whole surface from `crates/tinymcp-bus/src/lib.rs` and
      rewrite the crate docs.

**Verify:** `cargo test -p tinymcp-bus`, plus the CI job asserting the crate
pulls in no transport.

---

## Phase 3 — Transports

- [x] `crates/tinymcp/src/error/mod.rs` — extend the crate-wide `Error` with the
      variants the ported code needs, replacing the `anyhow` returns at the
      public surface. Internal `anyhow` use may stay; the public boundary
      returns `Result<T>`.
- [x] `crates/tinymcp/src/transport/http/` — `McpHttpClient` from
      `http_client/client.rs` (828 lines) and `client_helpers.rs` (160), with
      `client_tests.rs` (670) as `test.rs`. Protocol-version negotiation, SSE
      draining, session lifecycle, `WWW-Authenticate` parsing, the
      reinitialize-and-retry-once rule, `x-mcp-header` mirroring,
      `render_tool_result`, and `redact_endpoint` all move unchanged.
- [x] `crates/tinymcp/src/transport/stdio/spawn_env/` — from
      `config_servers/spawn_env.rs` (524 lines). The login-shell PATH probe and
      the up-front command resolution.
- [x] `crates/tinymcp/src/transport/stdio/` — `McpStdioClient` from
      `config_servers/stdio.rs` (314 lines).
- [x] Hoist `SUPPORTED_PROTOCOL_VERSIONS` and `LATEST_PROTOCOL_VERSION` into one
      place — done in Phase 2, in `tinymcp-bus::transport`. They were duplicated
      across the two transports; both now negotiate from the one list, and a
      test pins it.

**Verify:** `cargo test -p tinymcp transport`.

Two defects found while porting, each fixed with a regression test:

- The stdio transport never validated the protocol version the server
  negotiated, though the HTTP one always did. A subprocess is no more
  trustworthy than a remote endpoint; both check now.
- A stdio child was spawned without `kill_on_drop`, so a client dropped without
  an explicit close orphaned the server process. These are `npx` and `uvx`
  processes a user never started directly and has no obvious way to find.

---

## Phase 4 — The registries

- [x] `crates/tinymcp/src/config_servers/` — `McpServerRegistry`,
      `McpServerDefinition`, `McpTransportClient`, `McpRegistrySource` from
      `config_servers/registry.rs` (592 lines). Built from the contract
      `McpClientConfig` rather than OpenHuman's `Config`. Allow/deny
      enforcement stays fail-closed and pre-transport; its tests come with it.
- [x] `crates/tinymcp/src/registry/store/` — the SQLite store (965 lines).
      Schema unchanged. The data directory arrives from module configuration.
- [x] `crates/tinymcp/src/registry/sources/` — `smithery.rs` (268) and
      `mcp_official.rs` (1438), plus the cache. Dispatch is an **enum** rather
      than a boxed trait, which keeps it visible and makes the compiler name
      every site a third source would have to be handled at. The official
      registry's page-to-cursor map is owned by the dispatcher rather than
      being a process global, for the same reason the connection map is.
- [x] `crates/tinymcp/src/registry/connections/` — the live connection map
      (979 lines), and `supervisor/` (223). The map and the failure record are
      **owned**, not process-global: two hosts in one process would otherwise
      have shared connections, and every test would have run against the same
      map in whatever order the runner chose.
- [x] `crates/tinymcp/src/registry/oauth/` — discovery, registration, code
      exchange, and silent refresh (618). The redirect URI is a **parameter**:
      only the host knows which loopback port it actually bound, and a guess
      that is wrong sends the browser somewhere sign-in simply hangs.
      Completing an authorization stores the token and stops — reconnecting is
      the caller's job, which is what lets the connection map depend on this
      module for refresh without a cycle.
- [x] `crates/tinymcp/src/registry/curation/` (174) and `boot/` (110).
- [~] `crates/tinymcp/src/registry/setup/` — the secret vault from `setup.rs`
      (327) is done; the `setup_ops.rs` operations (690) land with `ops`. The
      vault is **owned**, not a process global, and its handle parser now trims
      before stripping the scheme — a padded handle, which is what a model
      producing one in prose writes, was being rejected.

      The OpenHuman agent invocation does not come across:
      `mcp_setup_config_assist` reaches `agent::turn_origin` to run an agent
      turn. That is host policy; the module's `ConfigAssist` member returns the
      prepared context and the host runs the turn.
- [x] `crates/tinymcp/src/registry/ops/` — the operation bodies from `ops.rs`
      (1057), as an `McpRegistry` **facade** rather than free functions: every
      operation needs several of the pieces, and those pieces need each other in
      a fixed order, so free functions would mean every caller assembling that
      themselves and every caller getting a chance to assemble it differently.
      Returns the contract's reply types instead of `RpcOutcome<Value>`.

      Two things stay with the host, and the facade returns what they need
      rather than doing them: publishing an event, and running a model turn for
      `config_assist`.

      `schemas.rs` (1268) does **not** move: it is OpenHuman's RPC controller
      wiring and is replaced by the bus adapter in Phase 5.
- [ ] Drop `registry/bus.rs`. It was pure `tracing` logging over `DomainEvent`
      with no side effects; the module logs directly and emits signals instead.
- [x] `crates/tinymcp/src/audit/` — the store. It gets **its own SQLite file**
      rather than sharing the host's memory-tree database, which is what made
      it unmovable. Existing audit rows stay in the host's old database; that
      is history, not operational state, and migrating it is a separate
      decision for the host to make.

**Verify:** `cargo test -p tinymcp`, and the four OpenHuman integration suites
ported into `crates/tinymcp/tests/`.

---

## Phase 5 — The bus adapter

`crates/tinymcp/src/tinybus_module/` and the two `verify_*` examples were
**removed** when the placeholder `greeting` behavior they served was deleted,
and are rebuilt here against the real interface. Until then the workspace
builds a `cdylib` that exports nothing, and the manifest-versus-`METHODS`
assertion has nothing to assert. That is the one gap between the contract and
the implementation, and it closes in this phase — nothing else is stubbed.

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
- [x] Phase 1 — `sanitize`
- [x] Phase 2 — contract crate
- [x] Phase 3 — transports
- [x] Phase 4 — registries
- [ ] Phase 5 — bus adapter
- [ ] Phase 6 — OpenHuman
