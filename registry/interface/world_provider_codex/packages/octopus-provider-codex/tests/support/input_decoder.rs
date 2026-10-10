use super::*;
use octopus_ledger::ContentHash;

#[test]
fn arbitrary_json_is_artifact_data_but_operator_envelopes_remain_strict() {
    let source = InterfaceId::new("system_context");
    for payload in [serde_json::json!({"authority":"operator","constraints":[]}), serde_json::json!([1, 2])] {
        let input = decode_provider_input(&serde_json::to_vec(&payload).unwrap(), "document", Some(&source), &BTreeMap::new(), None).unwrap();
        let input = input.into_turn_input().unwrap();
        let [UserInput::Text { text, .. }] = input.as_slice() else { panic!("artifact text expected") };
        let rendered: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(rendered["artifact"]["payload"], payload);
        assert_eq!(rendered["artifact"]["source_interface"], "system_context");
    }
    for (label, payload) in [(OPERATOR_MESSAGE_LABEL, b"{}".as_slice()), ("request", b"{\"prompt\":\"\"}".as_slice()), (OPERATOR_TURN_CONTROL_LABEL, b"{}".as_slice())] {
        assert!(decode_provider_input(payload, label, Some(&source), &BTreeMap::new(), None).is_err());
    }
}

#[test]
fn input_template_does_not_claim_operator_messages_but_input_delivery_needs_exact_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let selection = serde_json::json!({"template_interface_id":"system_service_input"});
    let make = |label: &str, from: &str, payload: serde_json::Value| {
        let file =
            InterfaceArtifactFile::write(dir.path(), label, &serde_json::to_vec(&payload).unwrap())
                .unwrap();
        InterfaceInvocationEnvelope {
            artifact: ArtifactRef { id:"fixture".into(), content:ContentHash::of_bytes(b"fixture") },
            artifact_file:file,
            artifact_binding_payloads:Default::default(),
            metadata:Some(serde_json::from_value(serde_json::json!({"runtime":{"run":"run","ee":"ee","worker":"worker","checkpoint":"input","interface":"provider","epoch":0},"checkpoint_binding_policy":{"inbound":{"interface":from,"config":{"artifact_policy":{"mode":"all"}}},"outbound":[]}})).unwrap()),
        }
    };
    for label in ["operator.message", "operator.turn.control"] {
        assert!(
            admitted_peer_message(
                &make(label, "client", serde_json::json!({})),
                Some(&selection)
            )
            .unwrap()
            .is_none()
        );
    }
    let body = serde_json::json!({"schema_version":1,"source_movement_seq":1,"source_artifact":{"id":"source","content":ContentHash::of_bytes(b"source")},"message":{"action":"interrupt","control_id":"exact"}});
    let input = make(
        octopus_operator_chat_client::input_intake::INPUT_INTAKE_LABEL,
        "intake",
        body.clone(),
    );
    assert!(admitted_peer_message(&input, None).is_err());
    assert!(admitted_peer_message(&input, Some(&selection)).is_err());
    let exact =
        serde_json::json!({"template_interface_id":"system_service_input","interface_id":"intake"});
    assert_eq!(
        admitted_peer_message(&input, Some(&exact))
            .unwrap()
            .unwrap()
            .action,
        PeerAction::Interrupt
    );
    let wrong = make(
        octopus_operator_chat_client::input_intake::INPUT_INTAKE_LABEL,
        "worker",
        body,
    );
    assert!(admitted_peer_message(&wrong, Some(&exact)).is_err());
}
