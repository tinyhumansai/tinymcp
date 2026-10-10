//! Library-only builds retain bounded processing failures without the module adapter.
use tinymcp::Error;
use tinymcp::processing::transform_text;
use tinymcp_bus::processing::{MAX_PROCESSING_BYTES, TextTransform, TransformTextRequest};

#[test]
fn library_processing_reports_invalid_argument_without_module_feature() {
    let result = transform_text(&TransformTextRequest {
        operation: TextTransform::SanitizeForLlm,
        text: "fixture".into(),
        max_bytes: MAX_PROCESSING_BYTES + 1,
    });
    assert!(
        matches!(result, Err(Error::InvalidArgument { detail }) if detail == "metadata processing exceeds byte limit")
    );
}
