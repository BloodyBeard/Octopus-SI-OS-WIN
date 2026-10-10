#[tokio::main]
async fn main() {
    if let Err(error) = octopus_world_pyecho_life_runtime::run_spawned_interface_life(
        octopus_image_generation_interface::interface_life(),
    )
    .await
    {
        eprintln!("octopus-image-generation-interface-life failed: {error}");
        std::process::exit(1);
    }
}
