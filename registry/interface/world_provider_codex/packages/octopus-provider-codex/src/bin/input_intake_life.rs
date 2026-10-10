#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = octopus_world_pyecho_life_runtime::run_spawned_interface_life(
        octopus_provider_codex::input_intake_life(),
    )
    .await
    {
        eprintln!("input intake failed: {error}");
        std::process::exit(1);
    }
}
