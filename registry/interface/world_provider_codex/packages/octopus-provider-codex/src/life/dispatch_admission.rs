use super::*;

pub(crate) struct DispatchAdmission {
    pub(crate) callable_tool_names: BTreeSet<String>,
    pub(crate) max_attempts: u32,
}

pub(crate) struct DispatchRefusal {
    pub(crate) malformed_policy: Option<(&'static str, String)>,
    pub(crate) message: String,
}

impl DispatchRefusal {
    fn terminal(message: impl Into<String>) -> Self {
        Self {
            malformed_policy: None,
            message: message.into(),
        }
    }
}

pub(crate) fn admit_dispatch(
    descriptor: &InterfaceDescriptor,
    envelope: &InterfaceInvocationEnvelope,
    handoff: &ProviderOutputHandoff,
) -> Result<Result<DispatchAdmission, DispatchRefusal>, String> {
    let private_config = &descriptor
        .interface_driver_config
        .as_ref()
        .ok_or_else(|| {
            format!(
                "provider interface '{}' has no interface_driver_config",
                descriptor.id.as_str()
            )
        })?
        .private_config;
    let callable_tool_names = match handoff.callable_tool_names(envelope) {
        Ok(names) => names,
        Err(error) => {
            return Ok(Err(DispatchRefusal::terminal(format!(
                "provider model tool topology is invalid before dispatch: {error}"
            ))));
        }
    };
    let hidden_tools = model_hidden_tool_names(private_config)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut model_visible_tools = BTreeSet::new();
    for affordance in envelope
        .interface_affordance_specs()
        .map_err(|error| error.to_string())?
    {
        let tool = affordance.name.as_str().to_string();
        if !callable_tool_names.contains(&tool) || hidden_tools.contains(&tool) {
            continue;
        }
        if !model_visible_tools.insert(tool.clone()) {
            return Ok(Err(DispatchRefusal::terminal(format!(
                "formed model affordances repeat tool '{tool}'"
            ))));
        }
    }
    if let Err(error) = handoff.validate_model_tool_routes(&model_visible_tools) {
        return Ok(Err(DispatchRefusal::terminal(format!(
            "provider model tool topology is invalid before dispatch: {error}"
        ))));
    }
    let mut limits = [0u32; 2];
    for (limit, field) in limits.iter_mut().zip(["max_attempts", "max_in_flight"]) {
        *limit = match required_policy_u32(envelope, field) {
            Ok(value) => value,
            Err(error) => {
                return Ok(Err(DispatchRefusal {
                    message: format!("formed provider {field} is invalid: {error}"),
                    malformed_policy: Some((field, error)),
                }));
            }
        };
    }
    let [max_attempts, max_in_flight] = limits;
    if max_attempts == 0 || max_in_flight == 0 {
        return Ok(Err(DispatchRefusal::terminal(
            "formed interface policy denied provider calls",
        )));
    }
    if let Err(error) = enforce_formed_model_policy(descriptor, envelope) {
        return Ok(Err(DispatchRefusal::terminal(format!(
            "provider_model_unavailable: {error}"
        ))));
    }
    Ok(Ok(DispatchAdmission {
        callable_tool_names,
        max_attempts,
    }))
}
