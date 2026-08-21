//! Percent-encoding for registry path segments.

/// The bytes that may appear in a path segment unencoded.
///
/// The unreserved set from RFC 3986, plus `@`. Qualified names routinely start
/// with one — `@modelcontextprotocol/server-filesystem` — and encoding it makes
/// the resulting URLs unreadable in a log for no benefit, since `@` is legal in
/// a path segment.
///
/// Everything else is encoded, `/` included: a qualified name is *one* segment,
/// and letting its slash through would address a different resource entirely.
const fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'@')
}

/// Percent-encodes a qualified name for use as one path segment.
///
/// # Examples
///
/// ```
/// # use tinymcp::registry::sources::encode_path_segment;
/// assert_eq!(encode_path_segment("simple-name"), "simple-name");
/// assert_eq!(encode_path_segment("hello world"), "hello%20world");
/// // The `@` survives; the `/` does not, because the whole name is one segment.
/// assert_eq!(
///     encode_path_segment("@scope/name"),
///     "@scope%2Fname",
/// );
/// ```
#[must_use]
pub fn encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());

    for byte in segment.bytes() {
        if is_unreserved(byte) {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }

    encoded
}
