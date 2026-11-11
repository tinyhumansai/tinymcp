# Curated servers at runtime

Status: accepted (tinymcp#49).

## Problem

The curated first-party servers are a compiled-in `const`
(`registry/curation/servers.rs`). Adding, correcting or removing one means a
tinymcp change, a release and a version bump in every host before it reaches a
directory. A vendor endpoint that moves stays wrong until then. Curated entries
are also the only way a server the official registry doesn't list (Slack,
Swiggy) reaches a host's catalog.

## Behaviour

A host may supply extra curated servers at runtime. They are merged with the
built-in list and take part everywhere the built-in ones do.

### Where they come from

`McpRegistryAuthConfig` gains `curated_servers: Vec<CuratedServerEntry>`
(`#[serde(default)]`, empty by default). It travels the same way as the other
registry settings: in `McpClientConfig.registry_auth` when the module object is
opened, and from a host's own config file. For openhuman that is
`[[mcp_client.registry_auth.curated_servers]]` in `config.toml`, which already
deserializes into this type, so adding a server is an edit plus a restart. No
release, and no host code change.

The module reads no file of its own. A path or URL to fetch was considered and
dropped: tinymcp has no runtime file loading, hosts own storage, and a hosted
list adds a network dependency and a question of who may publish to it.

`RegistrySettingsSet` does not carry the list; its arguments are positional and
fixed. A runtime-only change of curated servers goes through a re-open with the
new client config, as any other client-config change does.

### Entry shape

`CuratedServerEntry` mirrors `CuratedServer` with owned strings:
`qualified_name`, `display_name`, `description`, `remote_url`, `transport`
(`streamable_http` | `sse`), `auth` (`oauth` | `oauth_preregistered` | `token` |
`none`), optional `icon_url`. Unknown fields are refused.

### Validation

When the registry dispatcher takes the settings (the module object opening), each
entry is checked:

- `remote_url` (and `icon_url`, when set) is `https` with a host and no
  credentials;
- `qualified_name` is non-blank and unique among the runtime entries;
- `display_name` is non-blank.

An invalid entry is dropped and logged with its name and the reason. The rest
still load. An entry is never silently changed into something else. Open does
not fail over a bad curated entry, because the catalog is optional and the rest
of the client still has to work.

### Runtime entries are never promoted

The official badge is a claim that someone vetted a server. A configuration
line must not be able to make an arbitrary endpoint look like that, so a runtime
entry is never badged official, never leads a first search page, and never
ranks as curated. Those stay with the built-in servers.

A runtime entry whose `qualified_name` matches a built-in replaces its
endpoint, which lets a host fix a moved endpoint without waiting for a release.
It also takes the built-in's badge and promotions away. The replacement is
logged at warn with both URLs when the settings are taken.

Runtime entries come after the built-ins in listing order, so the existing
order is kept.

### Where the merged list is used

`CuratedCatalog` (`registry/curation/catalog.rs`) is the merged list. Each use
builds it from the registry settings in force:

- listing and search: the local-index merge (`official/index.rs`) and the
  cache fallback when the registry is unreachable (`official/fallback.rs`)
  include runtime entries;
- the detail fallback when the registry has no record
  (`McpOfficialRegistry::get`) describes a runtime entry, and a replacement wins
  over the built-in;
- promotion (badging through `RegistrySearchCurated`, `official_first`, the
  curated lead on a first page, and curated-first ranking) uses only built-in
  servers nobody replaced (`CuratedCatalog::is_promoted`).

`OFFICIAL_SERVERS`, `CURATED_SERVERS` and `tag_official` keep their meaning:
the built-in list alone. No host reads them today (openhuman uses only
`RegistrySearchCurated`).

## Contract

An additive minor bump of tinymcp-bus: a new optional field and three new
payload types (`CuratedServerEntry`, `CuratedEntryTransport`,
`CuratedEntryAuth`). It takes 1.31: #164 lands as 1.29 and the OAuth endpoint
guard (`local_oauth_origins`) as 1.30, both before this.

## Out of scope

Any UI for managing entries. The host's config file is the whole interface.
