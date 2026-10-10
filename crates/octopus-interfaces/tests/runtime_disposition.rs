use std::path::Path;

use octopus_interfaces::{
    form_interface_runtime_disposition, InterfaceDescriptor, InterfaceRuntimeDisposition,
};

fn descriptor(body: serde_json::Value) -> InterfaceDescriptor {
    serde_json::from_value(body).expect("interface descriptor parses")
}

#[test]
fn runtime_disposition_requires_a_positive_reader_or_worker_policy_fact() {
    let interface_root = Path::new("/registry-without-life-sidecars");
    let bare_system = descriptor(serde_json::json!({
        "id": "system_bare",
        "interface_type": "system",
        "role": "system",
        "direction": "bidirectional",
        "source_names": ["bare"]
    }));
    assert_eq!(
        form_interface_runtime_disposition(&bare_system, interface_root, false).unwrap(),
        InterfaceRuntimeDisposition::Inert
    );

    let door = descriptor(serde_json::json!({
        "id": "system_door",
        "interface_type": "system",
        "role": "system",
        "direction": "bidirectional",
        "source_names": ["door"],
        "interface_driver_ref": "door_family"
    }));
    assert_eq!(
        form_interface_runtime_disposition(&door, interface_root, false).unwrap(),
        InterfaceRuntimeDisposition::ExternalReader
    );

    let worker = descriptor(serde_json::json!({
        "id": "system_worker_probe",
        "interface_type": "system",
        "role": "worker",
        "direction": "bidirectional",
        "source_names": ["worker"]
    }));
    assert_eq!(
        form_interface_runtime_disposition(&worker, interface_root, true).unwrap(),
        InterfaceRuntimeDisposition::RuntimeRelay
    );

    let mut executable_worker = worker.clone();
    executable_worker.interface_driver_ref = "worker_driver".to_string();
    assert!(form_interface_runtime_disposition(&executable_worker, interface_root, true).is_err());

    let world_worker = descriptor(serde_json::json!({
        "id": "world_worker_probe",
        "interface_type": "world",
        "role": "worker",
        "direction": "bidirectional",
        "source_names": ["worker"],
        "interface_driver_ref": "world_worker_driver"
    }));
    assert!(form_interface_runtime_disposition(&world_worker, interface_root, false).is_err());
}
