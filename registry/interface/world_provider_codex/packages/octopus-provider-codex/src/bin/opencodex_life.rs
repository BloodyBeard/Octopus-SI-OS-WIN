#[tokio::main(flavor = "current_thread")]
async fn main() {
    let result = match octopus_provider_codex::doctor_requested() {
        Ok(true) => octopus_provider_codex::doctor_opencodex_interface(),
        Ok(false) => {
            octopus_world_pyecho_life_runtime::run_spawned_interface_life(
                octopus_provider_codex::opencodex_interface_life(),
            )
            .await
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        eprintln!("octopus-provider-opencodex-life failed: {error}");
        std::process::exit(1);
    }
}
