//! Parsing `text/event-stream` bodies.
//!
//! MCP's Streamable HTTP transport may answer a POST with either a plain JSON
//! body or an SSE stream carrying the same JSON-RPC reply in a `data:` frame.
//! This module handles the second case.
//!
//! # Why there is an incremental reader
//!
//! A server is allowed to keep the response stream open *after* it has already
//! sent the single reply frame. Reading the body to completion therefore stalls
//! every call until the server closes or the request times out, and those
//! stalls compound across a chain of tool calls. [`first_complete_sse_data`]
//! exists so the caller can stop reading the instant the reply lands, while the
//! request-level timeout still bounds a server that never replies at all.

use serde_json::Value;

use crate::error::{Error, Result};
use tinymcp_bus::McpSseEvent;

/// Returns the first `data:` frame from a fully-received SSE body.
///
/// # Errors
///
/// Returns [`Error::MalformedResponse`] when the body carries no data frame at
/// all, or when a frame's payload is not valid JSON.
pub(super) fn parse_sse_message(body: &str) -> Result<Value> {
    parse_sse_events(body)?
        .into_iter()
        .find_map(|event| event.data)
        .ok_or_else(|| Error::malformed(format!("no sse data frame in response: {body}")))
}

/// Returns the first JSON `data:` frame from the fully-terminated prefix of a
/// partially-received buffer, or `None` if no complete frame has arrived.
///
/// Only the portion up to the last blank-line event boundary is parsed, so a
/// half-received final `data:` line is never decoded as truncated JSON.
/// Keepalive comments and frames with no data are skipped — those return `None`
/// and mean "keep reading" rather than "give up". `\r\n` is normalized first so
/// a CRLF stream splits on the same boundary as an LF one.
///
/// # Errors
///
/// Returns [`Error::MalformedResponse`] when a *complete* frame's payload is
/// not valid JSON. An incomplete tail is never an error.
pub(super) fn first_complete_sse_data(buffer: &str) -> Result<Option<Value>> {
    let normalized = buffer.replace("\r\n", "\n");
    // Without a terminated event, nothing is fully received yet.
    let Some(boundary) = normalized.rfind("\n\n") else {
        return Ok(None);
    };
    let complete = &normalized[..=boundary];
    Ok(parse_sse_events(complete)?
        .into_iter()
        .find_map(|event| event.data))
}

/// Parses every event in an SSE body.
///
/// # Errors
///
/// Returns [`Error::MalformedResponse`] when a frame's `data:` payload is not
/// valid JSON.
pub(super) fn parse_sse_events(body: &str) -> Result<Vec<McpSseEvent>> {
    let mut events = Vec::new();
    let mut pending = PendingEvent::default();

    for raw_line in body.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            pending.flush_into(&mut events)?;
            continue;
        }
        // A line beginning with a colon is a comment, which servers use as a
        // keepalive.
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            pending.event = Some(value.trim_start().to_string());
        } else if let Some(value) = line.strip_prefix("id:") {
            pending.id = Some(value.trim_start().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            pending.data_lines.push(value.trim_start().to_string());
        }
    }
    // A body whose final event is not followed by a blank line still carries an
    // event; servers do send that when they close immediately after replying.
    pending.flush_into(&mut events)?;

    Ok(events)
}

/// The event being accumulated between blank-line boundaries.
#[derive(Debug, Default)]
struct PendingEvent {
    event: Option<String>,
    id: Option<String>,
    data_lines: Vec<String>,
}

impl PendingEvent {
    /// Emits the accumulated event, if there is one, and resets.
    ///
    /// A boundary with nothing accumulated emits nothing, so consecutive blank
    /// lines do not produce empty events.
    fn flush_into(&mut self, events: &mut Vec<McpSseEvent>) -> Result<()> {
        if self.event.is_none() && self.id.is_none() && self.data_lines.is_empty() {
            return Ok(());
        }

        // Multiple `data:` lines in one event are joined with newlines, per the
        // event-stream format.
        let data = if self.data_lines.is_empty() {
            None
        } else {
            let joined = self.data_lines.join("\n");
            Some(serde_json::from_str(&joined).map_err(|error| {
                Error::malformed(format!("sse data frame is not json: {error} — {joined}"))
            })?)
        };

        events.push(McpSseEvent {
            event: self.event.take(),
            id: self.id.take(),
            data,
        });
        self.data_lines.clear();
        Ok(())
    }
}
