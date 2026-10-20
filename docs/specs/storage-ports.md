# Storage on the tinystoragedrivers ports

Status: accepted. Part of the storage consolidation tracked by
tinyhumansai/openhuman#7522 (child #7639).

## Why

This crate used to link `rusqlite` and keep two database files,
`mcp_clients/mcp_clients.db` and `mcp_audit/mcp_audit.db`, with server
environment values and OAuth bundles in a plain table. A module loaded over
TinyBus cannot assume a writable data directory, and a cloud host runs many
tenants in one process. The host owns storage; this crate asks for ports.

## What changed

- No database client. The crate depends on `tinystoragedrivers-core` (the
  `DocumentStore` and `StreamStore` ports and the `Blocking` bridge) and
  `tinystoragedrivers-secrets` (the `SecretStore` port), by git tag, and on no
  driver.
- `Store` and `AuditStore` open over `StoragePorts`
  (`documents`, `streams`, `secrets`). Their API stays synchronous; each call
  runs its port future on one shared bridge thread.
- A host supplies ports in process (`StoragePorts::from_scoped`), or installs a
  `StorageProvider` that maps a data directory to ports; the module's
  `McpService` asks the installed provider. The default is an in-memory
  provider. The storage bus client implements the same port traits, so the
  module code is identical over the bus.

## Layout

| Port | Name | Holds |
| --- | --- | --- |
| documents | `mcp.servers` | installed servers, indexed on `qualified_name` and `installed_ms` |
| documents | `mcp.server_names` | one claim per qualified name; makes insert-if-absent atomic |
| documents | `mcp.registry_cache` | browse responses with a timestamp |
| documents | `mcp.tool_cache` | last advertised tools per server, keyed by a keyed fingerprint |
| documents | `mcp.registry_index` | the catalog, searched with `DocumentStore::search` |
| documents | `mcp.registry_index_state` | the sync cursor per catalog |
| streams | `mcp.audit` | tool-write audit records; id is offset + 1 |
| secrets | `mcp_client_env/<server id>` | environment values and the OAuth bundle, one secret |
| secrets | `mcp_cache_fingerprint_key` | key for tool-cache fingerprints (hex text) |

`tinymcp-bus` declares these resources with the canonical
`tinystoragedrivers_bus::StorageNeedsWithExactSecrets` type. Its `needs`
field carries each collection schema and index, the `mcp_audit` stream prefix,
and the `mcp_client_env/` secret prefix. The separate `exact_secret_names`
field declares `mcp_cache_fingerprint_key` as an exact-name need; it must not
be widened to a prefix. This declaration describes what the module uses; it
does not grant authority by itself. A host selects the declaration through its
module admission and resource-mapping policy, applies its own storage policy,
and binds the resulting grant to the authenticated caller. TinyMCP does not
maintain a parallel `StorageGrant` list of the same resources.

The bus crate keeps only this static needs vocabulary. TinyMCP's storage
module projects it into TinyStorageDrivers' executable schema and owns the
versioned migration callbacks; a contract-only host does not compile either.

Consumers replacing the implementation-local storage API should read
`tinymcp_bus::storage_needs().needs.collections` and the sibling stream and
secret selectors; exact names are in
`tinymcp_bus::storage_needs().exact_secret_names`. The implementation crate
re-exports this declaration as `tinymcp::storage::storage_needs()`. It replaces
the removed `tinymcp::storage::{StorageGrant, grant, collection_specs}`
declarations.

## Behaviour notes

- Index search: the port finds candidates by words and word prefixes; each is
  then checked so every query term is a substring of the label or description,
  as before. A term of one letter is checked but cannot select candidates.
- Insert-if-absent writes a claim with `Precondition::Absent` before the server.
  A claim whose server is gone (a crash between the two writes) is taken over.
- Deleting a server also removes its secret, its cached tools and its claim.
- Audit listing reads the stream in windows and filters, sorts and pages in
  memory, so ordering matches the previous SQL (`timestamp DESC, id DESC`).

## Hosting as a loadable module

Loaded as a `cdylib`, the module cannot reach the host's storage handles. When
its configuration names a `data_dir` and the host has not installed a
`StorageProvider` itself, `setup` connects to the host's
`ai.tinyhumans.tinystorage.Storage` objects (`BusProvider`, over
`tinystoragedrivers-busproxy::connect_storage`) and installs that as the
provider. The host binds the scope from the broker-authenticated caller and
enforces the grant above; the module never names a tenant. Every data
directory answers the one host-bound scope. A host with no storage objects
fails the load rather than falling back to memory, which would lose every
install at the next restart. A host that links the module into its own
process installs its own provider before `setup` and skips the bus.

Secrets travel on the confidential route, which the broker completes only to
an attested recipient. The host's storage objects must therefore be served by
an attested artifact before a released module can use the bus provider in
production; an in-process broker in tests cannot supply that, so the bus
tests keep secrets in a local store.

## Not in this crate

- Importing an existing `mcp_clients.db` / `mcp_audit.db`. The host reads the
  legacy files read-only and writes the records through these ports
  (tinyhumansai/openhuman#7636).
- Selecting a backend. Features such as `storage-sqlite` belong to the host.

## Schema v2

The module's declarations are `tinymcp::storage::storage_schema()` (owner `mcp`,
scope class Module, tinystoragedrivers v1.0.0); the storage grant is generated
from it. v1 names become `mcp.*`, `*_at` times become `*_ms` fields, ids are
`Key::of` keys, and JSON kept inside strings (`body_json`, `record_json`)
becomes an object body. Steps `servers_v2`, `server_names_v2`,
`registry_cache_v2`, `tool_cache_v2`, `registry_index_v2`,
`registry_index_state_v2` and the stream step `audit_v2` migrate a v1 database.
Secret names are unchanged (a secret cannot be renamed by a step, and they sit
outside the `mcp/` owner prefix).
