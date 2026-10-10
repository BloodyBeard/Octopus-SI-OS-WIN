use super::*;
use color_eyre::eyre::WrapErr;
use pretty_assertions::assert_eq;
use std::path::Path;
use tempfile::tempdir;

#[test]
fn write_trusted_project_local_delegates_to_the_provider_config_runtime() {
    let codex_home = tempdir().expect("temp codex home");
    let runtime = crate::test_support::TestProviderConfigRuntime::new();

    write_trusted_project_local(
        runtime.as_ref(),
        codex_home.path(),
        Path::new("/workspace/team.project"),
    )
    .expect("trust write");

    let writes = runtime.trust_writes.lock().expect("test runtime lock");
    assert_eq!(
        writes.as_slice(),
        &[(
            codex_home.path().to_path_buf(),
            std::path::PathBuf::from("/workspace/team.project"),
        )]
    );
}

#[test]
fn format_config_error_preserves_server_validation_message() {
    let err = Err::<(), _>(color_eyre::eyre::eyre!(
        "config/batchWrite failed: Invalid configuration: features.fast_mode=true violates \
         managed requirements; allowed set [fast_mode=false]"
    ))
    .wrap_err("config/batchWrite failed in TUI")
    .unwrap_err();

    assert_eq!(
        format_config_error(&err),
        "config/batchWrite failed in TUI: config/batchWrite failed: Invalid configuration: \
         features.fast_mode=true violates managed requirements; allowed set [fast_mode=false]"
    );
}
