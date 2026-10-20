//! The v1 to v2 steps: the previous release's `mcp_*` collections and stream
//! become the `mcp.*` ones.
//!
//! Each step is a pure function of the records it is handed, so a resumed or
//! repeated run produces the same output. A step keeps a record's creation
//! time and expiry, renames its `*_at` time fields to `*_ms`, and re-keys it
//! with [`id`]. A cache row or index row that cannot be carried over (its body
//! is not JSON, or the field its new key is built from is missing) is dropped
//! instead of failing the step: it holds nothing the module cannot fetch
//! again, and one bad row must not keep a whole database from migrating.

use serde_json::{Map, Value};
use tinystoragedrivers_core::schema::{SourceRecord, Step, TargetRecord};

use super::schema::{
    AUDIT_STREAM, REGISTRY_CACHE, REGISTRY_INDEX, REGISTRY_INDEX_STATE, SERVER_NAMES, SERVERS,
    TOOL_CACHE, doc_id,
};

/// The version the steps read.
const FROM: u32 = 1;
/// The version the steps write.
const TO: u32 = 2;

/// The v1 stream the audit log was appended to.
const LEGACY_AUDIT: &str = "mcp_audit";

/// A document step's per-record rule: the new id and body, or `None` to drop
/// the record.
type Rewrite = fn(
    &SourceRecord,
    Map<String, Value>,
) -> tinystoragedrivers_core::Result<Option<(String, Value)>>;

/// Every step, in the order they run.
pub(super) fn steps() -> Vec<Step> {
    vec![
        documents("servers_v2", SERVERS, "mcp_servers", server),
        documents("server_names_v2", SERVER_NAMES, "mcp_server_names", claim),
        documents(
            "registry_cache_v2",
            REGISTRY_CACHE,
            "mcp_registry_cache",
            cached_response,
        ),
        documents("tool_cache_v2", TOOL_CACHE, "mcp_tool_cache", cached_tools),
        documents(
            "registry_index_v2",
            REGISTRY_INDEX,
            "mcp_registry_index",
            index_row,
        ),
        documents(
            "registry_index_state_v2",
            REGISTRY_INDEX_STATE,
            "mcp_registry_index_state",
            index_state,
        ),
        Step::stream("audit_v2", AUDIT_STREAM, FROM, TO, |name, entries| {
            let rest = name
                .strip_prefix(LEGACY_AUDIT)
                .unwrap_or(&name)
                .trim_start_matches('/');
            let target = if rest.is_empty() {
                AUDIT_STREAM.to_owned()
            } else {
                format!("{AUDIT_STREAM}/{rest}")
            };
            Ok(Some((target, entries)))
        })
        .source_prefix(LEGACY_AUDIT),
    ]
}

/// A step reading the legacy collection `legacy` and writing `item`, applying
/// `rewrite` to each record's body.
fn documents(id: &'static str, item: &'static str, legacy: &'static str, rewrite: Rewrite) -> Step {
    Step::new(id, item, FROM, TO, move |batch| {
        let mut targets = Vec::with_capacity(batch.len());
        for record in batch {
            let Value::Object(body) = record.body.clone() else {
                continue;
            };
            if let Some((new_id, body)) = rewrite(&record, body)? {
                let mut target =
                    TargetRecord::new(item, new_id, body).expires_ms(record.meta.expires_ms);
                if record.meta.created_ms > 0 {
                    target = target.created_ms(record.meta.created_ms);
                }
                targets.push(target);
            }
        }
        Ok(targets)
    })
    .source(legacy)
}

/// Moves `from` to `to` when the record has it.
fn rename(body: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = body.remove(from) {
        body.insert(to.to_owned(), value);
    }
}

/// The string field `name`, if the record has it.
fn text<'a>(body: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    body.get(name).and_then(Value::as_str)
}

/// Replaces the JSON text in `from` with the value it holds, under `to`.
/// `None` when the field is missing or is not JSON.
fn unwrap_json(body: &mut Map<String, Value>, from: &str, to: &str) -> Option<()> {
    let parsed = serde_json::from_str::<Value>(text(body, from)?).ok()?;
    body.remove(from);
    body.insert(to.to_owned(), parsed);
    Some(())
}

fn server(
    record: &SourceRecord,
    mut body: Map<String, Value>,
) -> tinystoragedrivers_core::Result<Option<(String, Value)>> {
    rename(&mut body, "installed_at", "installed_ms");
    rename(&mut body, "last_connected_at", "last_connected_ms");
    Ok(Some((doc_id(&[&record.id])?, Value::Object(body))))
}

/// A name claim: keyed by the qualified name it holds. A claim from before
/// claims were timed reads as claimed at `0`, i.e. long expired.
fn claim(
    _record: &SourceRecord,
    mut body: Map<String, Value>,
) -> tinystoragedrivers_core::Result<Option<(String, Value)>> {
    let Some(name) = text(&body, "qualified_name").map(ToOwned::to_owned) else {
        return Ok(None);
    };
    let claimed = body
        .remove("claimed_at")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    body.insert("claimed_ms".to_owned(), Value::from(claimed));
    Ok(Some((doc_id(&[&name])?, Value::Object(body))))
}

fn cached_response(
    _record: &SourceRecord,
    mut body: Map<String, Value>,
) -> tinystoragedrivers_core::Result<Option<(String, Value)>> {
    let Some(key) = text(&body, "cache_key").map(ToOwned::to_owned) else {
        return Ok(None);
    };
    if unwrap_json(&mut body, "body_json", "body").is_none() {
        return Ok(None);
    }
    rename(&mut body, "cached_at", "cached_ms");
    Ok(Some((doc_id(&[&key])?, Value::Object(body))))
}

fn cached_tools(
    _record: &SourceRecord,
    mut body: Map<String, Value>,
) -> tinystoragedrivers_core::Result<Option<(String, Value)>> {
    let Some(key) = text(&body, "server_key").map(ToOwned::to_owned) else {
        return Ok(None);
    };
    rename(&mut body, "cached_at", "cached_ms");
    Ok(Some((doc_id(&[&key])?, Value::Object(body))))
}

fn index_row(
    _record: &SourceRecord,
    mut body: Map<String, Value>,
) -> tinystoragedrivers_core::Result<Option<(String, Value)>> {
    let (Some(source), Some(name)) = (
        text(&body, "source").map(ToOwned::to_owned),
        text(&body, "qualified_name").map(ToOwned::to_owned),
    ) else {
        return Ok(None);
    };
    if unwrap_json(&mut body, "record_json", "record").is_none() {
        return Ok(None);
    }
    rename(&mut body, "synced_at", "synced_ms");
    Ok(Some((doc_id(&[&source, &name])?, Value::Object(body))))
}

fn index_state(
    _record: &SourceRecord,
    mut body: Map<String, Value>,
) -> tinystoragedrivers_core::Result<Option<(String, Value)>> {
    let Some(source) = text(&body, "source").map(ToOwned::to_owned) else {
        return Ok(None);
    };
    rename(&mut body, "started_at", "started_ms");
    rename(&mut body, "synced_at", "synced_ms");
    Ok(Some((doc_id(&[&source])?, Value::Object(body))))
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
