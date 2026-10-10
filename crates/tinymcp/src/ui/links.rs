//! Links a tool result offers: payment pages, deep links, sign-in URLs.
//!
//! Found in the structured content and the text, classified, and capped. A
//! scheme that could run script or reach the host is blocked, and an image
//! asset is not an action.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{LinkClass, UiLink, UiLinkKind};

/// The most links one result surfaces.
pub const MAX_LINKS: usize = 5;

const MAX_URL_LEN: usize = 2048;
const MAX_WALK_DEPTH: usize = 8;
const MAX_WALK_STRINGS: usize = 512;

const BLOCKED_SCHEMES: &[&str] = &[
    "javascript",
    "data",
    "vbscript",
    "file",
    "blob",
    "about",
    "tauri",
    "ipc",
    "asset",
    "ohwidget",
    "filesystem",
    "chrome",
    "chrome-extension",
    "view-source",
    "ws",
    "wss",
    "ftp",
    "openhuman",
];

const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "svg", "bmp", "ico", "heic", "heif", "tif", "tiff",
];

static URL_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b([a-z][a-z0-9+.\-]{1,31})://[^\s"'<>\x60{}|\\^\[\]]+"#).ok()
});

/// How `url` is classified, by scheme and, for `http(s)`, by path.
#[must_use]
pub fn classify_link(url: &str) -> LinkClass {
    let Some((scheme, rest)) = url.split_once(':') else {
        return LinkClass::Blocked;
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        || !scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        || BLOCKED_SCHEMES.contains(&scheme.as_str())
        || rest.trim().is_empty()
        || url.chars().any(char::is_control)
        || url.len() > MAX_URL_LEN
    {
        return LinkClass::Blocked;
    }
    match scheme.as_str() {
        "http" | "https" => match url::Url::parse(url) {
            Ok(parsed) if parsed.host_str().is_some_and(|host| !host.is_empty()) => {
                if image_path(&parsed) {
                    LinkClass::Image
                } else {
                    LinkClass::Web
                }
            }
            _ => LinkClass::Blocked,
        },
        _ => LinkClass::Handoff,
    }
}

/// How a link of class `class` may be opened, or `None` when it is not an
/// action.
#[must_use]
pub fn link_kind(class: LinkClass) -> Option<UiLinkKind> {
    match class {
        LinkClass::Web => Some(UiLinkKind::External),
        LinkClass::Handoff => Some(UiLinkKind::Handoff),
        LinkClass::Image | LinkClass::Blocked => None,
    }
}

/// Whether an `http(s)` URL points at an image asset rather than a page.
#[must_use]
pub fn is_image_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|parsed| image_path(&parsed))
}

fn image_path(parsed: &url::Url) -> bool {
    let path = parsed.path().to_ascii_lowercase();
    if path.contains("/image/upload/") {
        return true;
    }
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| !ext.contains('/') && IMAGE_EXTENSIONS.contains(&ext))
}

/// Every actionable link in `structured` and `text`, deduplicated, at most
/// [`MAX_LINKS`], structured content first.
#[must_use]
pub fn extract_links(structured: Option<&Value>, text: &str) -> Vec<UiLink> {
    let mut out: Vec<UiLink> = Vec::new();
    let Some(pattern) = URL_RE.as_ref() else {
        return out;
    };
    let mut strings = Vec::new();
    if let Some(value) = structured {
        collect_strings(value, 0, &mut strings);
    }
    strings.push(text);
    for haystack in strings {
        for found in pattern.find_iter(haystack) {
            if out.len() >= MAX_LINKS {
                return out;
            }
            let url = trim_trailing(found.as_str());
            let Some(kind) = link_kind(classify_link(url)) else {
                tracing::trace!("[mcp_ui] skipped a link that is not an action");
                continue;
            };
            if out.iter().any(|link| link.url == url) {
                continue;
            }
            out.push(UiLink {
                url: url.to_string(),
                kind,
            });
        }
    }
    out
}

fn collect_strings<'a>(value: &'a Value, depth: usize, out: &mut Vec<&'a str>) {
    if depth > MAX_WALK_DEPTH || out.len() >= MAX_WALK_STRINGS {
        return;
    }
    match value {
        Value::String(text) => out.push(text),
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_strings(item, depth + 1, out)),
        Value::Object(map) => map
            .values()
            .for_each(|item| collect_strings(item, depth + 1, out)),
        _ => {}
    }
}

fn trim_trailing(url: &str) -> &str {
    let mut end = url.len();
    loop {
        let trimmed = &url[..end];
        let Some(last) = trimmed.chars().last() else {
            return trimmed;
        };
        let unbalanced_paren =
            last == ')' && trimmed.matches('(').count() < trimmed.matches(')').count();
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '*') || unbalanced_paren {
            end -= last.len_utf8();
        } else {
            return trimmed;
        }
    }
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
