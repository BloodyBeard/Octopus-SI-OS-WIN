#[tokio::main]
async fn main() {
    if let Err(error) = octopus_world_pyecho_life_runtime::run_spawned_interface_life(
        octopus_speech_interface::synthesis_interface_life(),
    )
    .await
    {
        eprintln!("octopus-text-to-speech-interface-life failed: {error}");
        std::process::exit(1);
    }
}
