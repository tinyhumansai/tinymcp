//! The remote tool's wire form, annotations included.

use serde_json::json;

use super::*;

type TestResult = Result<(), serde_json::Error>;

#[test]
fn a_listed_tools_annotations_are_kept() -> TestResult {
    let tool: McpRemoteTool = serde_json::from_value(json!({
        "name": "list_meetings",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    }))?;

    assert_eq!(
        tool.annotations,
        Some(json!({"readOnlyHint": true, "openWorldHint": false}))
    );
    assert!(tool.is_read_only());
    Ok(())
}

#[test]
fn a_tool_without_the_hint_or_with_it_false_is_not_read_only() {
    for annotations in [
        None,
        Some(json!({})),
        Some(json!({"readOnlyHint": false})),
        Some(json!({"readOnlyHint": "yes"})),
    ] {
        let mut tool = McpRemoteTool::new("sign_out");
        tool.annotations = annotations.clone();
        assert!(!tool.is_read_only(), "{annotations:?}");
    }
}

#[test]
fn absent_annotations_stay_off_the_wire() -> TestResult {
    let wire = serde_json::to_value(McpRemoteTool::new("forecast"))?;
    assert_eq!(
        wire,
        json!({"name": "forecast", "title": null, "description": null, "inputSchema": null})
    );

    let mut annotated = McpRemoteTool::new("forecast");
    annotated.annotations = Some(json!({"readOnlyHint": true}));
    let wire = serde_json::to_value(&annotated)?;
    assert_eq!(wire["annotations"], json!({"readOnlyHint": true}));
    assert_eq!(serde_json::from_value::<McpRemoteTool>(wire)?, annotated);
    Ok(())
}
