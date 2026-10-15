use super::*;

#[test]
fn model_text_keeps_selected_evidence_without_duplicating_unselected_refs() {
    let mut output = json!({"selected_input":{"fields":{"/text":"original task"}},
        "evidence":[{"source_artifact":"selected reference","fields":{"/ok":true}}],
        "omission":{"selected_fields_trimmed":false,"selected_evidence_trimmed":false,
            "unselected_entries":[{"source_artifact":"unselected reference","source_movement_seq":7}]}});
    let original = output.clone();
    render_text("Capture knowledge", &mut output).unwrap();
    let rendered: Value =
        serde_json::from_str(output["text"].as_str().unwrap().lines().last().unwrap()).unwrap();
    assert_eq!(rendered["selected_input"], original["selected_input"]);
    assert_eq!(rendered["evidence"], original["evidence"]);
    assert_eq!(rendered["omission"]["unselected_entry_count"], 1);
    assert!(rendered["omission"].get("unselected_entries").is_none());
    assert_eq!(output["omission"], original["omission"]);
    let first = output.clone();
    render_text("Capture knowledge", &mut output).unwrap();
    assert_eq!(output, first);
}
