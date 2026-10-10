use std::sync::Arc;

use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftFn};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let life: InterfaceLiftFn = Arc::new(|ctx| {
        Box::pin(
            octopus_world_pyecho_life_runtime::run_descriptor_activation_life(
                ctx,
                octopus_interface_activation_http_json_poll::TRANSPORT_REF,
                octopus_interface_activation_http_json_poll::activation_start,
            ),
        ) as InterfaceLifeFuture
    });
    if let Err(error) = octopus_world_pyecho_life_runtime::run_spawned_interface_life(life).await {
        eprintln!("octopus-http-json-poll-life failed: {error}");
        std::process::exit(1);
    }
}
