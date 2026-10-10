#[test]
fn steering_compatibility_keeps_acceptance_and_failure_evidence_exact() {
    let attempt = |response| match steer_attempt_application_with_retarget(
        "client-message".into(),
        "thread-1".into(),
        "turn-1".into(),
        response,
        true,
    ) {
        SteerAttemptApplication::Complete(result) => result,
        SteerAttemptApplication::Retarget { .. } => panic!("unexpected retarget"),
    };
    let accepted = attempt(Ok(TurnSteerResponse {
        turn_id: "turn-1".into(),
    }));
    assert!(accepted.result.acknowledged);
    assert_eq!(accepted.result.detail["stage"], "accepted_by_app_server");
    assert_eq!(accepted.result.detail["consumption_proven"], false);
    for (code, message, data) in [
        (-32603, "no active turn to steer", None),
        (
            -32600,
            "cannot steer a compact turn",
            Some(serde_json::json!({
                "codexErrorInfo":{"activeTurnNotSteerable":{"turnKind":"compact"}},
            })),
        ),
        (
            -32600,
            "expected active turn id `turn-1` but found ` turn-2`",
            None,
        ),
    ] {
        let result = attempt(Err(TypedRequestError::Server {
            method: "turn/steer".into(),
            source: octopus_appserver_proto::JSONRPCErrorError {
                code,
                message: message.into(),
                data: data.clone(),
            },
        }));
        assert!(!result.result.acknowledged);
        assert_eq!(result.disposition, CodexTurnControlDisposition::Applied);
        assert_eq!(result.result.detail["upstream_error"]["code"], code);
        assert_eq!(
            result.result.detail["upstream_error"]["data"],
            serde_json::json!(data)
        );
    }
    let partial: ThreadItem = serde_json::from_value(serde_json::json!({
        "type":"agentMessage", "id":"partial", "text":"Work so far", "phase":"partial_answer",
    }))
    .unwrap();
    let turn: octopus_appserver_proto::Turn = serde_json::from_value(serde_json::json!({
        "id":"turn-1", "rootTurnId":"turn-1", "items":[partial],
        "itemsView":"full", "status":"completed", "error":null,
    }))
    .unwrap();
    assert!(completed_turn_final_message(&turn).is_err());
}
