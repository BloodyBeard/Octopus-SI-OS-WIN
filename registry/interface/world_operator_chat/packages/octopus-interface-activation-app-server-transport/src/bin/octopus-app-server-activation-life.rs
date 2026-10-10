use std::sync::Arc;

use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftFn};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let life: InterfaceLiftFn = Arc::new(|ctx| {
        Box::pin(async move {
            if ctx.resolved_interface_descriptor()?.interface_driver_config.as_ref()
                .is_some_and(|config| config.private_config.contains_key("application_consent_authority")) {
                return Err("selected Home consent authority requires explicit migration before ordinary activation".into());
            }
            octopus_world_pyecho_life_runtime::run_descriptor_activation_life(
                ctx,
                octopus_interface_activation_app_server_transport::TRANSPORT_REF,
                octopus_interface_activation_app_server_transport::activation_start,
            ).await
        }) as InterfaceLifeFuture
    });
    if let Err(error) = octopus_world_pyecho_life_runtime::run_spawned_interface_life(life).await {
        eprintln!("octopus-app-server-activation-life failed: {error}");
        std::process::exit(1);
    }
}
