//! The official registry's wire shapes, and how they become catalog rows.
//!
//! These are deliberately permissive — every nested field is optional — so a
//! schema bump upstream does not stop the catalog parsing. There is one
//! exception, marked below, where permissiveness caused the bug it was supposed
//! to prevent.

use serde::Deserialize;
use serde_json::{Map, Value};

use super::super::types::SOURCE_MCP_OFFICIAL;
use tinymcp_bus::{ExtraFields, RegistryConnection, RegistryServerDetail, RegistryServerSummary};

/// The value `auth_kind` takes for a server declaring a static credential.
const AUTH_KIND_API_KEY: &str = "api_key";

/// The status the registry marks a withdrawn version with.
const STATUS_DEPRECATED: &str = "deprecated";

/// Where the registry keeps its own bookkeeping inside a row's metadata.
const REGISTRY_META_KEY: &str = "io.modelcontextprotocol.registry/official";

/// A page of the list endpoint.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct OfficialListResponse {
    #[serde(default)]
    servers: Vec<OfficialServerEnvelope>,
    #[serde(default)]
    metadata: Option<OfficialMetadata>,
}

impl OfficialListResponse {
    /// The rows worth showing, deduplicated by name.
    ///
    /// Drops anything that cannot actually be installed and anything the
    /// registry has deprecated. Both are noise: a row a user cannot install is
    /// a dead end they can only discover by trying.
    pub(super) fn into_summaries(self) -> Vec<RegistryServerSummary> {
        let mut seen = std::collections::HashSet::new();

        self.servers
            .into_iter()
            .filter(|envelope| envelope.is_installable() && !envelope.is_deprecated())
            .filter_map(|envelope| {
                seen.insert(envelope.server.name.clone())
                    .then(|| envelope.server.into_summary())
            })
            .collect()
    }

    /// The cursor for the next page, when there is one.
    ///
    /// An empty string counts as absent — the registry sends one at the end of
    /// a result set, and treating it as a cursor would page forever.
    pub(super) fn next_cursor(&self) -> Option<&str> {
        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.next_cursor.as_deref())
            .filter(|cursor| !cursor.is_empty())
    }
}

/// The page metadata.
#[derive(Debug, Clone, Deserialize)]
struct OfficialMetadata {
    #[serde(default, rename = "nextCursor")]
    next_cursor: Option<String>,
}

/// One row of the list endpoint.
///
/// # Why `server` has no default
///
/// Everything else here is optional so an upstream schema change does not stop
/// the catalog working. This field is the exception, and deliberately: an
/// earlier version of this adapter parsed the *inner* shape at the top level,
/// so serde filled every field with a default and the catalog silently rendered
/// pages of blank cards. A missing `server` key has to be a parse error, or the
/// same failure comes back looking like an empty registry.
#[derive(Debug, Clone, Deserialize)]
struct OfficialServerEnvelope {
    server: OfficialServer,
    #[serde(default, rename = "_meta")]
    meta: Option<Value>,
}

impl OfficialServerEnvelope {
    /// Whether this row offers any way to connect at all.
    fn is_installable(&self) -> bool {
        !self.server.remotes.is_empty() || !self.server.packages.is_empty()
    }

    /// Whether the registry has withdrawn this version.
    ///
    /// Absent metadata counts as not deprecated, which is what a row cached by
    /// an older build looks like.
    fn is_deprecated(&self) -> bool {
        self.meta
            .as_ref()
            .and_then(|meta| meta.get(REGISTRY_META_KEY))
            .and_then(|registry| registry.get("status"))
            .and_then(Value::as_str)
            == Some(STATUS_DEPRECATED)
    }
}

/// One server, as the official registry describes it.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct OfficialServer {
    /// The reverse-DNS identifier, such as `io.github.foo/server-bar`.
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "iconUrl")]
    icon_url: Option<String>,
    /// Hosted endpoints.
    #[serde(default)]
    pub(super) remotes: Vec<OfficialRemote>,
    /// Installable subprocess packages.
    #[serde(default)]
    pub(super) packages: Vec<OfficialPackage>,
    #[serde(default, rename = "websiteUrl")]
    website_url: Option<String>,
}

impl OfficialServer {
    /// The declared vendor site, when it declares a non-blank one.
    fn website(&self) -> Option<String> {
        self.website_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(ToString::to_string)
    }

    /// Whether the server declares a named static credential.
    ///
    /// A secret header, an `Authorization` header, or a secret environment
    /// variable. This is metadata, not a probe: it is what lets the catalog say
    /// "this needs an API key" without dialling anything.
    fn declares_secret_credential(&self) -> bool {
        let secret_header = self.remotes.iter().any(|remote| {
            remote.headers.iter().any(|header| {
                header.is_secret == Some(true)
                    || header.name.eq_ignore_ascii_case("authorization")
            })
        });

        let secret_variable = self.packages.iter().any(|package| {
            package
                .environment_variables
                .iter()
                .any(|variable| variable.is_secret == Some(true))
        });

        secret_header || secret_variable
    }

    /// A readable name.
    ///
    /// The declared title when there is one. Otherwise the last segment of the
    /// reverse-DNS name with separators turned into spaces, which is a better
    /// thing to show a user than `io.github.someone/some-server`.
    pub(super) fn display_name(&self) -> String {
        if let Some(title) = self.title.as_deref().filter(|title| !title.trim().is_empty()) {
            return title.to_string();
        }

        let segment = self
            .name
            .rsplit_once('/')
            .map(|(_, last)| last)
            .or_else(|| self.name.rsplit_once('.').map(|(_, last)| last))
            .unwrap_or(&self.name);

        segment.replace(['-', '_'], " ")
    }

    /// This server as a catalog row.
    pub(super) fn into_summary(self) -> RegistryServerSummary {
        let display_name = self.display_name();
        let website_url = self.website();
        let auth_kind = self
            .declares_secret_credential()
            .then(|| AUTH_KIND_API_KEY.to_string());

        RegistryServerSummary {
            qualified_name: self.name,
            display_name,
            description: self.description,
            icon_url: self.icon_url,
            // The official registry publishes no install count.
            use_count: 0,
            is_deployed: !self.remotes.is_empty(),
            source: SOURCE_MCP_OFFICIAL.to_string(),
            // Badged later, by curation, from its own list.
            official: false,
            website_url,
            auth_kind,
            extra: ExtraFields::new(),
        }
    }

    /// This server as a detail record, with one connection per way in.
    pub(super) fn into_detail(self) -> RegistryServerDetail {
        let display_name = self.display_name();

        let mut connections = Vec::with_capacity(self.remotes.len() + self.packages.len());

        for remote in &self.remotes {
            connections.push(RegistryConnection {
                r#type: "http".to_string(),
                deployment_url: remote.url.clone(),
                config_schema: remote.to_config_schema(),
                example_config: None,
                published: true,
                extra: ExtraFields::new(),
            });
        }

        for package in &self.packages {
            connections.push(RegistryConnection {
                r#type: "stdio".to_string(),
                deployment_url: None,
                config_schema: package.to_config_schema(),
                example_config: package.to_example_config(),
                published: true,
                extra: ExtraFields::new(),
            });
        }

        RegistryServerDetail {
            qualified_name: self.name,
            display_name,
            description: self.description,
            icon_url: self.icon_url,
            connections,
            source: SOURCE_MCP_OFFICIAL.to_string(),
            extra: ExtraFields::new(),
        }
    }
}

/// A hosted endpoint.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct OfficialRemote {
    #[serde(default)]
    url: Option<String>,
    /// The inputs it wants, sent as request headers.
    #[serde(default)]
    headers: Vec<OfficialHeader>,
}

impl OfficialRemote {
    /// The declared headers as an input schema, so an install form can prompt
    /// for each one. `None` when the remote declares none.
    fn to_config_schema(&self) -> Option<Value> {
        build_schema(self.headers.iter().map(|header| SchemaField {
            name: &header.name,
            description: header.description.as_deref(),
            is_secret: header.is_secret == Some(true),
            is_required: header.is_required == Some(true),
        }))
    }
}

/// One declared header.
#[derive(Debug, Clone, Deserialize)]
struct OfficialHeader {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "isRequired")]
    is_required: Option<bool>,
    #[serde(default, rename = "isSecret")]
    is_secret: Option<bool>,
}

/// An installable package.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct OfficialPackage {
    #[serde(default, rename = "registryType")]
    registry_type: Option<String>,
    #[serde(default)]
    identifier: Option<String>,
    #[serde(default, rename = "runtimeHint")]
    runtime_hint: Option<String>,
    #[serde(default, rename = "runtimeArguments")]
    runtime_arguments: Vec<OfficialRuntimeArg>,
    #[serde(default, rename = "environmentVariables")]
    environment_variables: Vec<OfficialEnvVar>,
    #[serde(default, rename = "configSchema")]
    config_schema: Option<Value>,
}

impl OfficialPackage {
    /// A worked example of how to launch this package.
    ///
    /// `uvx` for Python, `npx -y` for Node, and `npx -y` for anything
    /// unrecognised — most of the ecosystem is Node, so it is the least
    /// surprising guess when the registry does not say.
    pub(super) fn to_example_config(&self) -> Option<Value> {
        let (command, mut args) = match self.registry_type.as_deref() {
            Some("pypi") => (
                self.runtime_hint.as_deref().unwrap_or("uvx").to_string(),
                Vec::new(),
            ),
            Some("npm") => {
                let command = self.runtime_hint.as_deref().unwrap_or("npx").to_string();
                // `-y` only when the package declares no arguments of its own;
                // a package that does may well be passing its own flags first.
                let args = if self.runtime_arguments.is_empty() {
                    vec!["-y".to_string()]
                } else {
                    Vec::new()
                };
                (command, args)
            }
            _ => (
                self.runtime_hint.as_deref().unwrap_or("npx").to_string(),
                vec!["-y".to_string()],
            ),
        };

        args.extend(
            self.runtime_arguments
                .iter()
                .filter_map(|argument| argument.value.clone()),
        );
        if let Some(identifier) = &self.identifier {
            args.push(identifier.clone());
        }

        Some(serde_json::json!({ "command": command, "args": args }))
    }

    /// The package's input schema.
    ///
    /// Built from the declared environment variables when there are any, and
    /// otherwise the schema the registry supplied verbatim.
    pub(super) fn to_config_schema(&self) -> Option<Value> {
        if self.environment_variables.is_empty() {
            return self.config_schema.clone();
        }

        build_schema(self.environment_variables.iter().map(|variable| SchemaField {
            name: &variable.name,
            description: variable.description.as_deref(),
            is_secret: variable.is_secret == Some(true),
            is_required: variable.is_required == Some(true),
        }))
    }
}

/// One argument the package is launched with.
#[derive(Debug, Clone, Deserialize)]
struct OfficialRuntimeArg {
    #[serde(default)]
    value: Option<String>,
}

/// One declared environment variable.
#[derive(Debug, Clone, Deserialize)]
struct OfficialEnvVar {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "isRequired")]
    is_required: Option<bool>,
    #[serde(default, rename = "isSecret")]
    is_secret: Option<bool>,
}

/// One input, however it was declared.
struct SchemaField<'a> {
    name: &'a str,
    description: Option<&'a str>,
    is_secret: bool,
    is_required: bool,
}

/// Builds an input schema from declared fields.
///
/// Unnamed fields are skipped — an input with no name cannot be prompted for
/// and cannot be sent. `None` when nothing usable is left.
fn build_schema<'a>(fields: impl Iterator<Item = SchemaField<'a>> + Clone) -> Option<Value> {
    let mut properties = Map::new();
    let mut required = Vec::new();

    for field in fields {
        if field.name.is_empty() {
            continue;
        }

        let mut property = Map::new();
        if let Some(description) = field.description {
            property.insert("description".into(), Value::String(description.to_string()));
        }
        if field.is_secret {
            // The marker an install form reads to render a masked input.
            property.insert("x-secret".into(), Value::Bool(true));
        }

        properties.insert(field.name.to_string(), Value::Object(property));
        if field.is_required {
            required.push(Value::String(field.name.to_string()));
        }
    }

    if properties.is_empty() {
        return None;
    }

    let mut schema = Map::new();
    schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), Value::Array(required));
    }

    Some(Value::Object(schema))
}
