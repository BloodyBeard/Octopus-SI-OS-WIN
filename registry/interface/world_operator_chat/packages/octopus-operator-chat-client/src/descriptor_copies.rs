use super::*;

pub(crate) fn copy_interface_descriptor(descriptor: &mut InterfaceDescriptor, id: &str) {
    if descriptor.id.as_str() != id {
        descriptor.copy_of = Some(descriptor.copy_of.take().unwrap_or_else(|| descriptor.id.clone()));
        descriptor.id = InterfaceId::new(id);
    }
}

pub(crate) fn copy_tool_descriptor(descriptor: &mut ToolDescriptor, id: &str) {
    if descriptor.id.as_str() != id {
        descriptor.copy_of = Some(descriptor.copy_of.take().unwrap_or_else(|| descriptor.id.clone()));
        descriptor.id = ToolDescriptorId::new(id);
    }
}

pub(crate) fn copy_json_descriptor(descriptor: &mut serde_json::Value, id: &str) -> Result<(), String> {
    let source = descriptor["id"].as_str().ok_or("copied descriptor has no exact id")?.to_string();
    if source != id {
        let original = match descriptor.get("copy_of") {
            Some(serde_json::Value::String(original)) => original.clone(),
            None | Some(serde_json::Value::Null) => source,
            Some(_) => return Err(format!("copied descriptor '{source}' has a malformed copy_of")),
        };
        descriptor["copy_of"] = serde_json::json!(original);
        descriptor["id"] = serde_json::json!(id);
    }
    Ok(())
}

pub(crate) fn sibling_descriptor_bytes(
    source: &InterfaceDescriptor,
    sibling_id: &str,
    source_names: Option<&[String]>,
    driver_config: Option<&octopus_interfaces::InterfaceDriverConfig>,
) -> Result<String, String> {
    let mut sibling = source.clone();
    copy_interface_descriptor(&mut sibling, sibling_id);
    if let Some(source_names) = source_names {
        sibling.source_names = source_names.to_vec();
    }
    if let Some(config) = driver_config {
        sibling.interface_driver_config = Some(config.clone());
    }
    let content = pretty_json(
        &serde_json::to_value(sibling)
            .map_err(|error| format!("private interface did not encode: {error}"))?,
    )?;
    let parsed = parse_strict::<InterfaceDescriptor>(content.as_bytes()).map_err(|error| {
        format!("private interface '{sibling_id}' failed descriptor validation: {error}")
    })?;
    if parsed.id.as_str() != sibling_id {
        return Err(format!(
            "private interface '{sibling_id}' is not a self-contained exact interface"
        ));
    }
    Ok(content)
}

pub(crate) fn preserve_exact_door_host_slots(
    interface_root: &Path,
    source: &InterfaceDescriptor,
    exact_id: &str,
) -> Result<Option<octopus_interfaces::InterfaceDriverConfig>, String> {
    let path = interface_root.join(exact_id).join("interface.json");
    let existing = match std::fs::read(&path) {
        Ok(bytes) => Some(
            parse_strict::<InterfaceDescriptor>(&bytes).map_err(|error| {
                format!(
                    "existing exact door descriptor '{}' is malformed: {error}",
                    path.display()
                )
            })?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "existing exact door descriptor '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    let Some(existing) = existing else {
        return Ok(source.interface_driver_config.clone());
    };
    if existing.id.as_str() != exact_id
        || existing.interface_driver_ref != source.interface_driver_ref
        || existing.interface_type != source.interface_type
        || existing.role != source.role
        || existing.direction != source.direction
    {
        return Err(format!(
            "existing exact door '{}' changed its executable boundary contract",
            exact_id
        ));
    }
    let Some(existing_config) = existing.interface_driver_config else {
        return Ok(source.interface_driver_config.clone());
    };
    if existing_config.host_slots.is_empty() {
        return Ok(source.interface_driver_config.clone());
    }
    let mut planned = source.interface_driver_config.clone().unwrap_or_default();
    planned.host_slots.extend(existing_config.host_slots);
    Ok(Some(planned))
}
