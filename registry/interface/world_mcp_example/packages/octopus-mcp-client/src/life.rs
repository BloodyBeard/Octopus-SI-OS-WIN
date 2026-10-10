use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use std::sync::Arc;

use crate::mcp_client::McpClient;
use crate::{
    emit_catalog_as_inbound_artifacts, emit_operation_catalog,
    load_interface_config_from_descriptor, load_operation_catalog, mcp_interface_process,
    McpInterfaceConfig, McpInterfaceProcess, McpOperationCatalog, McpOperationRequest,
};

pub fn catalog_interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(remote_mcp_catalog_life(ctx)) as InterfaceLifeFuture)
}

pub fn operation_interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(remote_mcp_operation_life(ctx)) as InterfaceLifeFuture)
}

async fn remote_mcp_catalog_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let config = load_interface_config_from_descriptor(descriptor.clone(), &ctx.interface)?;
    let operations = load_operation_catalog(&ctx.interface_dir, &descriptor)?;
    emit_operation_catalog(&ctx, &operations)
        .await
        .map_err(|error| error.to_string())?;
    ctx.signal_ready()?;
    project_remote_catalog(&ctx, &config).await
}

async fn remote_mcp_operation_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let config = load_interface_config_from_descriptor(descriptor.clone(), &ctx.interface)?;
    let operations = load_operation_catalog(&ctx.interface_dir, &descriptor)?;
    let process = mcp_interface_process(
        ctx.interface.clone(),
        config.clone(),
        ctx.interface_var_dir().join("remote-mcp"),
    );
    serve_remote_operations(&ctx, &process, &operations).await
}

async fn project_remote_catalog(
    ctx: &InterfaceLiftContext,
    config: &McpInterfaceConfig,
) -> Result<(), String> {
    loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        match McpClient::connect(config, &ctx.interface_var_dir().join("remote-mcp-catalog")).await
        {
            Ok(client) => match emit_catalog_as_inbound_artifacts(ctx, config, &client).await {
                Ok(()) => {
                    ctx.cancel.cancelled().await;
                    return Ok(());
                }
                Err(error) if error.is_reconnectable() => tracing::warn!(
                    interface = ctx.interface.as_str(),
                    error = %error,
                    "MCP catalog projection disconnected"
                ),
                Err(error) => return Err(error.to_string()),
            },
            Err(error) if error.is_reconnectable() => tracing::warn!(
                interface = ctx.interface.as_str(),
                error = %error,
                "MCP resource connection failed"
            ),
            Err(error) => return Err(error.to_string()),
        }
        tokio::select! {
            _ = ctx.cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep(config.reconnect_backoff) => {}
        }
    }
}

async fn serve_remote_operations(
    ctx: &InterfaceLiftContext,
    process: &McpInterfaceProcess,
    catalog: &McpOperationCatalog,
) -> Result<(), String> {
    let mut reader = LedgerTailReader::open_named(ctx, "mcp-operations")?;
    let output_root = ctx.interface_var_dir().join("operation-results");
    ctx.signal_ready()?;
    loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        let batch = reader.read_new()?;
        if batch.is_empty() {
            reader.wait_for_change(&ctx.cancel).await?;
            continue;
        }
        for own in batch {
            let envelope = match reader.envelope(&own) {
                Ok(envelope) => envelope,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "poisoned_outbound",
                        serde_json::json!({"error": error}),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let manifest = match envelope.read_artifact_manifest() {
                Ok(manifest) => manifest,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "malformed_artifact_manifest",
                        serde_json::json!({"error": error.to_string()}),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            if manifest.label != catalog.request_label {
                reader.commit_through(&own)?;
                continue;
            }
            let request = envelope
                .read_artifact_payload()
                .map_err(|error| error.to_string())
                .and_then(|payload| {
                    serde_json::from_slice::<McpOperationRequest>(&payload)
                        .map_err(|error| format!("MCP operation request is malformed: {error}"))
                });
            let request = match request {
                Ok(request) => request,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "malformed_mcp_operation",
                        serde_json::json!({"error": error}),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let Some(spec) = catalog.operations.get(&request.operation) else {
                ctx.hand_admitted_failure(
                    own.movement.seq.0,
                    "unknown_mcp_operation",
                    serde_json::json!({"operation": request.operation}),
                )
                .await?;
                reader.commit_through(&own)?;
                continue;
            };
            match process.receive(spec, envelope, &output_root).await {
                Ok(artifact) => {
                    ctx.hand_admitted_arrival(artifact, Vec::new()).await?;
                }
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "mcp_operation_refused",
                        serde_json::json!({
                            "operation": request.operation,
                            "error": error,
                        }),
                    )
                    .await?;
                }
            }
            reader.commit_through(&own)?;
        }
    }
}
