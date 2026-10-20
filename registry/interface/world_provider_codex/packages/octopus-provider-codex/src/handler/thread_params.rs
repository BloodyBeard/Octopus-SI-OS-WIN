use super::*;

pub(crate) fn isolated_thread_start_params(
    model: Option<String>,
    bound_instructions: String,
    native_affordances: Vec<DynamicToolSpec>,
) -> ThreadStartParams {
    ThreadStartParams {
        model,
        developer_instructions: Some(bound_instructions),
        environments: Some(Vec::<TurnEnvironmentParams>::new()),
        sandbox: Some(SandboxMode::ReadOnly),
        dynamic_tools: (!native_affordances.is_empty()).then_some(native_affordances),
        ..ThreadStartParams::default()
    }
}

pub(crate) fn non_empty_string_field(
    value: Option<&serde_json::Value>,
    field_path: &str,
) -> Result<Option<String>, InterfaceDriverError> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(InterfaceDriverError::Protocol(format!(
            "{field_path} must be a non-empty string"
        ))),
    }
}

pub(crate) fn parse_reasoning_effort(
    effort: Option<&str>,
) -> Result<Option<ReasoningEffort>, InterfaceDriverError> {
    effort
        .map(|value| {
            value.parse::<ReasoningEffort>().map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "provider reasoning_effort '{value}' is invalid: {error}"
                ))
            })
        })
        .transpose()
}

pub(crate) fn parse_reasoning_summary(
    summary: Option<&str>,
) -> Result<ReasoningSummary, InterfaceDriverError> {
    match summary {
        Some("auto") => Ok(ReasoningSummary::Auto),
        Some("concise") => Ok(ReasoningSummary::Concise),
        Some("detailed") => Ok(ReasoningSummary::Detailed),
        Some("none") => Ok(ReasoningSummary::None),
        Some(value) => Err(InterfaceDriverError::Protocol(format!(
            "provider reasoning_summary '{value}' is invalid"
        ))),
        None => Err(InterfaceDriverError::FailedClosed(
            "provider interface declares no reasoning_summary; the app-server may not choose a hidden summary mode"
                .to_string(),
        )),
    }
}

pub(crate) fn prepend_context_block_to_turn_input(
    context_block: Option<String>,
    input: Vec<UserInput>,
) -> Vec<UserInput> {
    match context_block.filter(|block| !block.trim().is_empty()) {
        Some(block) => {
            let mut prepended = Vec::with_capacity(input.len() + 1);
            prepended.push(UserInput::Text {
                text: block,
                text_elements: Vec::new(),
            });
            prepended.extend(input);
            prepended
        }
        None => input,
    }
}

pub(crate) fn fresh_turn_start_params(
    thread_id: String,
    input: Vec<UserInput>,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
    service_tier: Option<Option<String>>,
    reasoning_summary: ReasoningSummary,
    additional_context: BTreeMap<String, AdditionalContextEntry>,
    turn_trigger: &str,
) -> TurnStartParams {
    TurnStartParams {
        thread_id,
        input,
        model: Some(model),
        effort: reasoning_effort,
        service_tier,
        summary: Some(reasoning_summary),
        additional_context: (!additional_context.is_empty())
            .then(|| additional_context.into_iter().collect()),
        turn_trigger: Some(turn_trigger.to_string()),
        ..TurnStartParams::default()
    }
}

pub(crate) fn validate_model_service_tier(
    model: &octopus_appserver_proto::Model,
    tier: &str,
) -> Result<(), InterfaceDriverError> {
    if tier == "default" || model.service_tiers.iter().any(|entry| entry.id == tier) {
        return Ok(());
    }
    Err(InterfaceDriverError::FailedClosed(format!(
        "native model '{}' does not advertise service tier '{}'; available tiers: [{}]",
        model.model, tier, model.service_tiers.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>().join(", ")
    )))
}

pub(crate) fn validate_native_service_tier_response(
    requested: Option<&str>,
    reported: Option<&str>,
) -> Result<(), InterfaceDriverError> {
    if requested.is_some() && requested != reported {
        return Err(InterfaceDriverError::FailedClosed(format!(
            "native provider did not retain selected service tier {requested:?}; active tier: {reported:?}"
        )));
    }
    Ok(())
}

pub(crate) fn native_turn_context(
    conversation: Option<String>,
    grant: Option<String>,
) -> BTreeMap<String, AdditionalContextEntry> {
    [
        ("octopus_bound_context", conversation, AdditionalContextKind::Untrusted),
        ("octopus_formed_policy", grant, AdditionalContextKind::Application),
    ]
    .into_iter()
    .filter_map(|(name, value, kind)| {
        value.filter(|value| !value.trim().is_empty())
            .map(|value| (name.to_string(), AdditionalContextEntry { value, kind }))
    })
    .collect()
}

pub(crate) fn encode_tool_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '-' => out.push_str("-h"),
            '.' => out.push_str("-d"),
            other => out.push(other),
        }
    }
    out
}

pub(crate) fn decode_tool_name(encoded: &str) -> Result<String, InterfaceDriverError> {
    let mut out = String::with_capacity(encoded.len());
    let mut chars = encoded.chars();
    while let Some(ch) = chars.next() {
        if ch != '-' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('h') => out.push('-'),
            Some('d') => out.push('.'),
            other => {
                return Err(InterfaceDriverError::Protocol(format!(
                    "native tool name '{encoded}' has a malformed identifier escape (a '-' must be \
                     followed by 'h' or 'd'; got {other:?}) — failing closed, never routing a mangled \
                     tool name through the gate"
                )));
            }
        }
    }
    Ok(out)
}

pub(crate) fn extract_native_affordances_from_invocation(
    input: &InterfaceInvocationEnvelope,
    hidden_tools: &[String],
) -> Result<Vec<DynamicToolSpec>, InterfaceDriverError> {
    let arr = model_visible_affordances(input.interface_affordance_specs()?, hidden_tools)
        .map_err(InterfaceDriverError::Protocol)?;
    let mut specs = Vec::with_capacity(arr.len());
    for (i, t) in arr.iter().enumerate() {
        let name = t
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                InterfaceDriverError::Protocol(format!(
                    "invocation metadata interface_affordance_specs[{i}] has no non-empty 'name' (failing closed — a nameless \
                     native affordance would silently disappear from the interface-rendered list)"
                ))
            })?
            .to_string();
        let description = t
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let input_schema = match t.get("input_schema") {
            None => serde_json::json!({ "type": "object", "properties": {} }),
            Some(s) if s.is_object() => s.clone(),
            Some(_) => {
                return Err(InterfaceDriverError::Protocol(format!(
                    "invocation metadata interface_affordance_specs[{i}] ('{name}') has a non-object 'input_schema' (the native \
                     tool protocol requires a JSON-schema object) — failing closed"
                )));
            }
        };
        specs.push(DynamicToolSpec::Function(DynamicToolFunctionSpec {
            name: encode_tool_name(&name),
            description,
            input_schema,
            defer_loading: false,
        }));
    }
    Ok(specs)
}
