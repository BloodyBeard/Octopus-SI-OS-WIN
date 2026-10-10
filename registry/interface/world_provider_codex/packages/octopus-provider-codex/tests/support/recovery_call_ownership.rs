#[test]
fn inner_code_mode_result_requires_its_own_native_function_call() {
    let rollout = tempfile::NamedTempFile::new().unwrap();
    let expected = recovered_function_call_output("exec-inner", "admitted result");
    let outer = serde_json::json!({"type":"response_item","payload":{
        "type":"custom_tool_call","call_id":"call-outer","name":"exec","input":"source"
    }});
    let retained = format!("{outer}\n");
    std::fs::write(rollout.path(), &retained).unwrap();
    let evidence = recovered_tool_result_rollout_evidence(rollout.path(), &expected, None).unwrap();
    assert!(!evidence.output_persisted);
    assert_eq!(evidence.native_function_calls, 0);
    assert!(
        matches!(evidence.validate_injection(&expected), Err(InterfaceDriverError::FailedClosed(message))
        if message.contains("exec-inner") && message.contains("explicit continuity repair"))
    );
    assert_eq!(std::fs::read_to_string(rollout.path()).unwrap(), retained);
    assert_eq!(expected["output"], "admitted result");
}

#[test]
fn recovered_result_injection_uses_current_compaction_history() {
    let rollout = tempfile::NamedTempFile::new().unwrap();
    let expected = recovered_function_call_output("call-exact", "admitted result");
    let call = serde_json::json!({"type":"function_call","call_id":"call-exact",
        "name":"observe","namespace":"formed_interfaces","arguments":"{}"});
    let recorded = serde_json::json!({"type":"response_item","payload":call});
    let compacted = |history| {
        serde_json::json!({"type":"compacted","payload":{
            "message":"summary","replacement_history":history
        }})
    };
    for (history, allowed) in [
        (format!("{recorded}\n"), true),
        (format!("{recorded}\n{recorded}\n"), false),
        (
            format!("{recorded}\n{}\n", compacted(serde_json::json!([]))),
            false,
        ),
        (
            format!("{recorded}\n{}\n", compacted(serde_json::json!([call]))),
            true,
        ),
        (
            format!("{recorded}\n{}\n", compacted(serde_json::Value::Null)),
            false,
        ),
        (
            format!("{}\n{recorded}\n", compacted(serde_json::json!([]))),
            true,
        ),
    ] {
        std::fs::write(rollout.path(), &history).unwrap();
        let evidence =
            recovered_tool_result_rollout_evidence(rollout.path(), &expected, None).unwrap();
        assert!(!evidence.output_persisted);
        assert_eq!(evidence.validate_injection(&expected).is_ok(), allowed);
        assert_eq!(std::fs::read_to_string(rollout.path()).unwrap(), history);
    }
    let persisted = serde_json::json!({"type":"response_item","payload":expected});
    std::fs::write(
        rollout.path(),
        format!(
            "{recorded}\n{persisted}\n{}\n",
            compacted(serde_json::json!([]))
        ),
    )
    .unwrap();
    let evidence = recovered_tool_result_rollout_evidence(rollout.path(), &expected, None).unwrap();
    assert!(evidence.output_persisted);
    assert_eq!(evidence.native_function_calls, 0);
    assert!(evidence.validate_injection(&expected).is_ok());
    std::fs::write(
        rollout.path(),
        format!("{}\n", compacted(serde_json::json!([call, expected]))),
    )
    .unwrap();
    let evidence = recovered_tool_result_rollout_evidence(rollout.path(), &expected, None).unwrap();
    assert!(evidence.output_persisted);
    let mut conflicting = expected.clone();
    conflicting["output"] = serde_json::json!("invented result");
    for history in [
        serde_json::json!([call, conflicting]),
        serde_json::json!([call, expected, expected]),
    ] {
        std::fs::write(rollout.path(), format!("{}\n", compacted(history))).unwrap();
        assert!(recovered_tool_result_rollout_evidence(rollout.path(), &expected, None).is_err());
    }
    let malformed = compacted(serde_json::json!({"call_id":"call-exact"}));
    std::fs::write(rollout.path(), format!("{recorded}\n{malformed}\n")).unwrap();
    assert!(recovered_tool_result_rollout_evidence(rollout.path(), &expected, None).is_err());
}
