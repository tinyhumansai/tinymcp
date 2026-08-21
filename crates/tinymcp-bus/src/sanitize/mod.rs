//! Sanitization helpers for remote MCP tool metadata.
//!
//! Remote MCP servers send free-form `description` and `title` strings that
//! flow directly into an agent LLM's tool-use context. This module strips and
//! caps those strings before anything stores or forwards them.
//!
//! The full pipeline ([`sanitize_for_llm`]) runs three steps:
//!
//! 1. **Control-character strip** ([`strip_control_chars`]) — removes ASCII
//!    control bytes that have no place in human-readable copy. Newline and tab
//!    are preserved so multi-line descriptions render.
//! 2. **Instruction-fence strip** ([`strip_instruction_fences`]) — removes
//!    well-known LLM prompt-template boundary tokens (`<|im_start|>`,
//!    `<system>`, `[INST]`, …) so a remote server cannot smuggle a role or
//!    template switch into the tool-use context.
//! 3. **UTF-8-safe truncate** ([`truncate_utf8_safe`]) — bounds the byte length
//!    so a very long description cannot dominate the context window.
//!
//! # Why this is in the contract crate
//!
//! The HTTP transport applies this pipeline to every remote tool description
//! and title before a caller sees them, so the bound is part of what a caller
//! is promised rather than an implementation detail. A host that renders the
//! same vocabulary — `OpenHuman` runs *skill* descriptions through the identical
//! pipeline from its orchestrator prompt builder — needs the same rule, and two
//! copies of a security-relevant stripping rule in two repositories would
//! drift. The code is pure and allocation-only, so it costs this crate nothing
//! to be the one place both can name.
//!
//! # What this is not
//!
//! This is lexical defence, not semantic. It removes markers; it does not judge
//! intent. A host that wants prompt-injection *detection* runs its own detector
//! over the definitions on top of this — that is host policy and deliberately
//! lives outside this crate.

/// Maximum bytes accepted for a remote tool `description` after sanitization.
///
/// Sized to fit a reasonable natural-language summary; servers that need
/// richer copy can host it externally and link to it.
pub const MAX_DESCRIPTION_BYTES: usize = 1024;

/// Maximum bytes accepted for a remote tool `title` after sanitization.
pub const MAX_TITLE_BYTES: usize = 128;

/// Suffix appended when [`truncate_utf8_safe`] shortens the input.
const TRUNCATION_SUFFIX: &str = "\u{2026}"; // single-codepoint ellipsis

/// Tokens recognised as LLM instruction-fence / prompt-template markers.
///
/// Matched case-insensitively. The list is intentionally narrow — these are
/// markers with no legitimate place in a free-form natural-language tool
/// description. Every entry is ASCII and lowercase, which
/// [`strip_instruction_fences`] relies on; `test::fence_tokens_are_ascii_and_lowercase`
/// enforces it.
const INSTRUCTION_FENCE_TOKENS: &[&str] = &[
    "<|im_start|>",
    "<|im_end|>",
    "<|system|>",
    "<|user|>",
    "<|assistant|>",
    "<|endoftext|>",
    "<system>",
    "</system>",
    "<assistant>",
    "</assistant>",
    "<user>",
    "</user>",
    "[system]",
    "[/system]",
    "[inst]",
    "[/inst]",
    "<<sys>>",
    "<</sys>>",
    "### instructions:",
    "### system:",
    "### user:",
    "### assistant:",
];

/// Strips ASCII control characters, preserving newline and tab.
///
/// Removes `\x00`..=`\x08`, `\x0b`, `\x0c`, `\x0e`..=`\x1f`, and `\x7f`.
/// Newline (`\x0a`) and tab (`\x09`) survive so legitimate multi-line
/// descriptions still render.
///
/// # Examples
///
/// ```
/// # use tinymcp_bus::sanitize::strip_control_chars;
/// assert_eq!(strip_control_chars("a\x00b\nc"), "ab\nc");
/// ```
#[must_use]
pub fn strip_control_chars(input: &str) -> String {
    input
        .chars()
        .filter(|ch| {
            if *ch == '\n' || *ch == '\t' {
                return true;
            }
            // Drop ASCII C0 and DEL.
            let code = *ch as u32;
            !(code <= 0x1f || code == 0x7f)
        })
        .collect()
}

/// Strips well-known LLM instruction-fence and prompt-template boundary
/// tokens, case-insensitively, repeating until the input is stable.
///
/// Repeating matters: removing a token can splice two halves of another one
/// together, and a single pass would leave the result behind.
///
/// # Implementation note
///
/// The search runs over the **original** bytes rather than a lowercased copy.
/// Lowercasing is not length-preserving in Unicode — `İ` (U+0130, two bytes)
/// lowercases to two codepoints totalling three — so a byte offset found in a
/// lowercased string can land mid-codepoint in the original. Splicing at such
/// an offset would corrupt the string or panic. Because every token this scans
/// for is ASCII, an ASCII-case-insensitive scan of the original is both exactly
/// equivalent for the tokens that matter and immune to that class of bug.
///
/// # Examples
///
/// ```
/// # use tinymcp_bus::sanitize::strip_instruction_fences;
/// assert_eq!(strip_instruction_fences("<SYSTEM>hi</system>"), "hi");
/// assert_eq!(strip_instruction_fences("system uptime"), "system uptime");
/// ```
#[must_use]
pub fn strip_instruction_fences(input: &str) -> String {
    let mut out = input.to_string();
    let mut changed = true;
    while changed {
        changed = false;
        for token in INSTRUCTION_FENCE_TOKENS {
            if let Some(pos) = find_ascii_case_insensitive(&out, token) {
                // The match came from an ASCII scan of `out` itself, so
                // `pos..pos + token.len()` is a valid char-boundary range.
                out.replace_range(pos..pos + token.len(), "");
                changed = true;
                break;
            }
        }
    }
    out
}

/// Returns the byte offset of the first ASCII-case-insensitive occurrence of
/// `needle` in `haystack`.
///
/// `needle` must be ASCII. Because the comparison is byte-wise against ASCII,
/// a match can only begin at a UTF-8 character boundary: every byte of a
/// multi-byte sequence has its high bit set and so can never equal an ASCII
/// byte. The returned offset is therefore always safe to slice at.
fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// Truncates `input` to at most `max_bytes` bytes, including the ellipsis
/// suffix, respecting UTF-8 codepoint boundaries.
///
/// Input that already fits is returned unchanged. Bytes for the suffix are
/// reserved *before* slicing, so the result never exceeds `max_bytes`. When
/// `max_bytes` is too small to hold even the suffix, the result is a plain
/// codepoint-safe truncation with no suffix — anything else would exceed the
/// cap.
///
/// # Examples
///
/// ```
/// # use tinymcp_bus::sanitize::truncate_utf8_safe;
/// assert_eq!(truncate_utf8_safe("hello", 32), "hello");
/// assert_eq!(truncate_utf8_safe("hello world", 8), "hello\u{2026}");
/// ```
#[must_use]
pub fn truncate_utf8_safe(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    let suffix_len = TRUNCATION_SUFFIX.len();
    // Degenerate case: cap shorter than even the suffix.
    if max_bytes <= suffix_len {
        return input[..floor_char_boundary(input, max_bytes)].to_string();
    }
    let end = floor_char_boundary(input, max_bytes - suffix_len);
    let mut buf = String::with_capacity(end + suffix_len);
    buf.push_str(&input[..end]);
    buf.push_str(TRUNCATION_SUFFIX);
    buf
}

/// Returns the largest offset `<= index` that is a UTF-8 character boundary.
///
/// `index` is assumed to be within `input`; callers here only reach this with
/// an index below the length, because the fits-already case returns early.
fn floor_char_boundary(input: &str, index: usize) -> usize {
    let mut end = index.min(input.len());
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Applies the full sanitization pipeline: control-char strip, then fence
/// strip, then UTF-8-safe truncate.
///
/// The order is load-bearing. Stripping runs before truncation so a caller
/// cannot spend the byte budget on content that was going to be removed, and
/// the fence strip runs after the control-char strip so a marker cannot be
/// hidden by interleaved control bytes.
///
/// # Examples
///
/// ```
/// # use tinymcp_bus::sanitize::{sanitize_for_llm, MAX_DESCRIPTION_BYTES};
/// let clean = sanitize_for_llm("Returns the current weather.", MAX_DESCRIPTION_BYTES);
/// assert_eq!(clean, "Returns the current weather.");
/// ```
#[must_use]
pub fn sanitize_for_llm(input: &str, max_bytes: usize) -> String {
    let no_ctrl = strip_control_chars(input);
    let no_fences = strip_instruction_fences(&no_ctrl);
    truncate_utf8_safe(&no_fences, max_bytes)
}

#[cfg(test)]
mod test;
