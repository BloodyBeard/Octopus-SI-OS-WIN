#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = octopus_world_pyecho_life_runtime::run_spawned_interface_life(
        octopus_context_projection::interface_life(),
    ).await {
        eprintln!("context projection failed: {error}");
        std::process::exit(1);
    }
}
