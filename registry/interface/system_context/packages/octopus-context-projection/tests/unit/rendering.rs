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

#[test]
fn oversized_fields_keep_their_beginning_and_end_within_the_bound() {
    let text = format!("start-{}-end", "x".repeat(4096));
    let payload = json!({"text": text, "list": vec!["y"; 1024], "small": "ok"});
    let pointers = ["/text", "/list", "/small"].map(String::from);
    let (projected, trimmed) = fields(&payload, &pointers, 256, true).unwrap();
    let kept = projected["/text"].as_str().unwrap();
    assert!(kept.starts_with("start-") && kept.ends_with("-end") && kept.contains("chars truncated"));
    for pointer in ["/text", "/list"] {
        assert!(serde_json::to_vec(&projected[pointer]).unwrap().len() <= 256);
    }
    assert_eq!(projected["/small"], "ok");
    assert_eq!(trimmed, vec![json!({"pointer":"/text","bytes":serde_json::to_vec(&payload["text"]).unwrap().len()}),
        json!({"pointer":"/list","bytes":serde_json::to_vec(&payload["list"]).unwrap().len()})]);
    assert!(fields(&payload, &["/absent".to_string()], 256, true).is_err());
    assert_eq!(fields(&payload, &["/absent".to_string()], 256, false).unwrap().0, json!({}));
}
