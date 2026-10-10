#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = octopus_world_pyecho_life_runtime::run_spawned_interface_life(
        octopus_world_provider_responses::interface_life(),
    )
    .await
    {
        eprintln!("octopus-world-provider-responses-life failed: {error}");
        std::process::exit(1);
    }
}
