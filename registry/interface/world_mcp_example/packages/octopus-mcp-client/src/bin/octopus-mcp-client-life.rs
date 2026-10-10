#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = octopus_world_pyecho_life_runtime::run_spawned_interface_life(
        octopus_mcp_client::catalog_interface_life(),
    )
    .await
    {
        eprintln!("octopus-mcp-client-life failed: {error}");
        std::process::exit(1);
    }
}
