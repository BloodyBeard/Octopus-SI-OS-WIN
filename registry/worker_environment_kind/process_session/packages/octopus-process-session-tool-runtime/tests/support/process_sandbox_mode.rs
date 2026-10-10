use super::*;

#[test]
fn disabled_descriptor_launches_directly() {
    let root = tempfile::tempdir().unwrap();
    let spec = ProcessAuthoritySpec {
        sandbox_mode: octopus_descriptors::ProcessSandboxMode::Disabled,
        ..Default::default()
    };
    let spec: ProcessAuthoritySpec =
        serde_json::from_value(serde_json::to_value(spec).unwrap()).unwrap();
    let resolved = spec
        .resolve(
            root.path(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            [],
            [],
            [],
            [],
        )
        .unwrap();
    let target = root.path().join("unsandboxed");
    let process = resolved
        .authority
        .prepare(
            "/bin/mkdir",
            &[target.clone().into_os_string()],
            root.path(),
            None,
        )
        .unwrap();
    assert_eq!(process.program, process.executable);
    assert!(
        std::process::Command::new(process.program)
            .args(process.args)
            .env_clear()
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(target.exists());
}
