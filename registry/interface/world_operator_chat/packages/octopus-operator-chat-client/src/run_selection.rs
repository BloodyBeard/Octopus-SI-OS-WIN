use super::*;

pub(crate) fn resolve_run_selection(
    request: &OperatorChatPlanRequest,
    config: &ClientConfig,
) -> Result<RunSelection, String> {
    let requested_surface_role = client_surface_role(&request.client_surface_role)?;
    if let Some(existing) = request.existing_run_record.as_ref() {
        let run_id = existing
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "existing chat run.id is required".to_string())?
            .to_string();
        let ee_id = existing
            .get("ee")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "existing chat run.ee is required".to_string())?
            .to_string();
        let private_worker = existing
            .get("worker")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "existing chat run.worker is required".to_string())?;
        let interface_state = existing
            .get("interface_state")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "existing chat run.interface_state is required".to_string())?;
        let clients = interface_state
            .iter()
            .filter_map(|(interface, state)| {
                let state = state.as_object()?;
                matches!(
                    state
                        .get("client_surface_role")
                        .and_then(serde_json::Value::as_str),
                    Some("home" | "task")
                )
                .then_some((interface.as_str(), state))
            })
            .collect::<Vec<_>>();
        let [(chat_interface_id, client_state)] = clients.as_slice() else {
            return Err(format!(
                "existing chat run must contain exactly one client interface state, found {}",
                clients.len()
            ));
        };
        let worker_choice_interface = client_state
            .get("worker_choice_interface")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "existing chat state '{}' does not record its worker choice",
                    chat_interface_id
                )
            })?
            .to_string();
        let provider_choice_interface = client_state
            .get("provider_choice_interface")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "existing chat state '{}' does not record its provider choice",
                    chat_interface_id
                )
            })?
            .to_string();
        let provider_runtime_interface = client_state
            .get("provider_runtime_interface")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                format!(
                    "existing chat state '{}' does not record its exact provider runtime interface",
                    chat_interface_id
                )
            })?
            .to_string();
        let surface_role = client_state
            .get("client_surface_role")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                format!(
                    "existing chat state '{}' does not record its client surface role",
                    chat_interface_id
                )
            })?;
        if surface_role != requested_surface_role {
            return Err(format!(
                "existing chat surface role is '{}', not the requested '{}'",
                surface_role, requested_surface_role
            ));
        }
        if request.provider_interface_id != provider_choice_interface {
            return Err(format!(
                "existing chat provider choice is '{}', not the requested '{}'",
                provider_choice_interface, request.provider_interface_id
            ));
        }
        if request
            .ee_id
            .as_deref()
            .is_some_and(|requested| requested != ee_id)
        {
            return Err(format!(
                "existing chat run execution environment is '{ee_id}', not the requested one"
            ));
        }
        if request
            .worker_interface_id
            .as_deref()
            .is_some_and(|requested| {
                requested != worker_choice_interface && requested != private_worker
            })
        {
            return Err(format!(
                "existing chat run worker choice is '{worker_choice_interface}', not the requested one"
            ));
        }
        return Ok(RunSelection {
            ee_id,
            worker_choice_interface,
            provider_choice_interface,
            provider_runtime_interface: Some(provider_runtime_interface),
            run_id: Some(run_id),
            chat_interface_id: Some((*chat_interface_id).to_string()),
            worker_runtime_interface: Some(private_worker.to_string()),
            surface_role: requested_surface_role,
        });
    }

    Ok(RunSelection {
        ee_id: safe_segment(request.ee_id.as_deref().unwrap_or(&config.run.ee), "ee_id")?,
        worker_choice_interface: safe_segment(
            request
                .worker_interface_id
                .as_deref()
                .unwrap_or(&config.run.worker),
            "worker_interface_id",
        )?,
        provider_choice_interface: safe_segment(
            &request.provider_interface_id,
            "provider_interface_id",
        )?,
        provider_runtime_interface: None,
        run_id: None,
        chat_interface_id: None,
        worker_runtime_interface: None,
        surface_role: requested_surface_role,
    })
}

pub(crate) fn client_surface_role(value: &str) -> Result<String, String> {
    match value.trim() {
        "home" => Ok("home".to_string()),
        "task" => Ok("task".to_string()),
        other => Err(format!(
            "client_surface_role must be 'home' or 'task', got '{other}'"
        )),
    }
}
