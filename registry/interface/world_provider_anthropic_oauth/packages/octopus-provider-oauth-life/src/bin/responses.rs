#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = octopus_world_pyecho_life_runtime::run_spawned_interface_life(
        octopus_provider_oauth_life::responses_interface_life(),
    )
    .await
    {
        eprintln!("octopus-provider-oauth-responses-life failed: {error}");
        std::process::exit(1);
    }
}
