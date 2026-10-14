use super::*;
use octopus_interface_transport::{InterfaceLiftContext, SystemInterfaceEmissionArtifact};
use octopus_provider_run_authoring::{CheckedRunEvidence, CheckedRunEvidenceReader, FileCheckedRunReader};
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use serde_json::{Value, json};
use std::time::Duration;

pub(crate) const REQUEST_LABEL: &str = "provider.composer_prediction.request";
pub(crate) const RESULT_LABEL: &str = "provider.composer_prediction.result";

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PredictionRequest {
    pub request_id: String,
    pub reply_to: InterfaceId,
    pub source: ArtifactRef,
    pub source_movement_seq: u64,
}

#[derive(serde::Deserialize)]
struct PredictionConfig {
    is_enabled: bool,
    prompt: String,
    prompt_version: String,
    prediction_reasoning_effort: String,
    #[serde(default)]
    unsupported_models: Vec<String>,
}

pub(crate) async fn run(
    ctx: &InterfaceLiftContext, driver: &CodexAppServerInterfaceDriver, manager: &Arc<AuthManager>,
    input: &InterfaceInvocationEnvelope, sequence: u64,
) -> Result<(), String> {
    let cancel = ctx.cancel.child_token();
    let pending = process(ctx, driver, manager, input, sequence, &cancel);
    tokio::pin!(pending);
    tokio::select! {
        result = &mut pending => result,
        changed = wait_for_new_input(ctx, sequence) => {
            cancel.cancel();
            let result = pending.await;
            changed?;
            result
        }
    }
}

async fn wait_for_new_input(ctx: &InterfaceLiftContext, sequence: u64) -> Result<(), String> {
    let mut reader = LedgerTailReader::open_named(ctx, "composer-predictions")?;
    loop {
        if ctx.cancel.is_cancelled() { return Ok(()); }
        for own in reader.read_new()? {
            reader.commit_through(&own)?;
            if own.movement.seq.0 > sequence { return Ok(()); }
        }
        reader.wait_for_change(&ctx.cancel).await?;
    }
}

async fn process(
    ctx: &InterfaceLiftContext, driver: &CodexAppServerInterfaceDriver, manager: &Arc<AuthManager>,
    input: &InterfaceInvocationEnvelope, sequence: u64, cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    input.runtime().map_err(|e| e.to_string())?;
    let request: PredictionRequest = serde_json::from_slice(&input.read_artifact_payload_bounded(16_384).map_err(|e| e.to_string())?)
        .map_err(|e| format!("invalid composer prediction request: {e}"))?;
    let record = octopus_runtime::run_record::RunRecord::from_bytes(&ctx.pinned_run_record_bytes(input)?)
        .map_err(|e| e.to_string())?;
    let target = record.checkpoints.iter().filter(|cp| record.active_checkpoints.contains(&cp.id))
        .find(|cp| cp.inbound_interface() == Some(&request.reply_to) && cp.outbound_interface_bindings.is_empty())
        .and_then(|cp| cp.interface_binding.as_ref()).filter(|binding| binding.admits(RESULT_LABEL))
        .map(|binding| binding.interface.clone())
        .ok_or("composer prediction requires an authored result-only return binding")?;
    if request.request_id.is_empty() || request.request_id.len() > 128
        || !input.granted_tool_names().map_err(|e| e.to_string())?.is_empty()
        || !input.worker_environment_kinds().map_err(|e| e.to_string())?.is_empty() {
        return Err("composer prediction run must have no tool or worker-environment grant".into());
    }
    let source = validate_source(ctx, input, &request, sequence)?;
    let root = ctx.interface_var_dir().join("composer-predictions").join(sequence.to_string());
    let receipt = root.join("result.json");
    let output = if receipt.exists() {
        let output: Value = serde_json::from_slice(&std::fs::read(&receipt).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if output["request"] != serde_json::to_value(&request).map_err(|e| e.to_string())?
            || output["input"] != serde_json::to_value(&input.artifact).map_err(|e| e.to_string())? {
            return Err("retained composer prediction belongs to another input".into());
        }
        output
    } else {
        let result = async {
            let auth = manager.auth().await.ok_or("Codex sign-in is unavailable")?;
            if !auth.is_chatgpt_auth() { return Ok(json!({"status":"unavailable", "suggestion":null})); }
            let token = auth.get_token().map_err(|_| "Codex sign-in token is unavailable")?;
            let account = auth.get_account_id().ok_or("Codex account identity is unavailable")?;
            let response = octopus_login::default_client::create_client().get("https://chatgpt.com/backend-api/wham/predictions/config")
                .bearer_auth(token).header("ChatGPT-Account-Id", account).timeout(Duration::from_secs(10)).send().await.map_err(|_| "Codex prediction configuration request failed")?;
            if !response.status().is_success() { return Err(format!("Codex prediction configuration returned {}", response.status())); }
            let config: PredictionConfig = response.json().await.map_err(|_| "Codex prediction configuration is malformed")?;
            if !config.is_enabled { return Ok(json!({"status":"unavailable", "suggestion":null})); }
            if cancel.is_cancelled() { return Ok(json!({"status":"unavailable", "suggestion":null})); }
            driver.predict(&source, &request.request_id, &config, cancel).await
        };
        let result: Result<Value, String> = result.await;
        if ctx.cancel.is_cancelled() { return Err("composer prediction cancelled".into()); }
        let result = result.unwrap_or_else(|message| json!({"status":"failed", "suggestion":null, "error":message}));
        let output = json!({"request":request, "input":input.artifact, "source_movement_seq":sequence, "result":result});
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let temporary = root.join("result.tmp");
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(&output).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, &receipt).map_err(|e| e.to_string())?;
        std::fs::File::open(&root).and_then(|dir| dir.sync_all()).map_err(|e| e.to_string())?;
        output
    };
    let artifact = InterfaceArtifactFile::write(root, RESULT_LABEL, &serde_json::to_vec(&output).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    match ctx.hand_system_emission(SystemInterfaceEmissionArtifact::Materialized(artifact), sequence, target).await? {
        octopus_interfaces::DeliveryVerdict::Admitted { .. } => Ok(()),
        verdict => Err(format!("composer prediction result was not admitted: {verdict:?}")),
    }
}

fn validate_source(ctx: &InterfaceLiftContext, input: &InterfaceInvocationEnvelope, request: &PredictionRequest, sequence: u64) -> Result<Value, String> {
    if request.source_movement_seq >= sequence { return Err("prediction source must precede its request".into()); }
    let bound = input.artifact_binding_payloads.iter().find(|bound| bound.artifact == request.source)
        .ok_or("prediction source is not an exact readable bound artifact")?;
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence { movements_path: &ctx.movements_path, ledger_index_path: &ctx.ledger_index_path,
        blobs_dir: &ctx.blobs_dir, max_control_frame_bytes: limits.max_control_frame_bytes,
        max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes };
    let mut reader = FileCheckedRunReader::open(&evidence, None)?;
    let row = reader.admitted_movement(request.source_movement_seq)?.ok_or("prediction source movement is absent")?;
    let (manifest, payload) = reader.artifact(&request.source)?;
    let source: Value = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    let native = &source["codex_continuity"];
    let runtime = row.runtime()
        .ok_or("prediction source has no admitted runtime identity")?;
    let continuity = parse_codex_continuity(native, &ctx.interface, runtime.run.as_str()).map_err(|e| e.to_string())?;
    if row.artifact.as_ref() != Some(&request.source) || manifest.label != INTERFACE_ARTIFACT_OUTPUT_LABEL
        || row.from != octopus_ledger::Endpoint::Interface(ctx.interface.clone())
        || payload.as_slice() != bound.read_bytes().map_err(|e| e.to_string())?.as_ref()
        || native["provider_interface"] != ctx.interface.as_str()
        || !native["pending"].is_null() || !native["interrupted_pending"].is_null()
        || source["thread_id"].as_str().filter(|s| !s.is_empty()).is_none()
        || source["turn_id"].as_str().filter(|s| !s.is_empty()).is_none()
        || source["thread_id"] != native["thread_id"] {
        return Err("prediction source is not an exact completed result from this provider".into());
    }
    if continuity.pending.is_some() || continuity.interrupted_pending.is_some() || continuity.tool_operator_request.is_some() {
        return Err("prediction source still has pending work".into());
    }
    Ok(source)
}

impl CodexAppServerInterfaceDriver {
    async fn predict(&self, source: &Value, request_id: &str, config: &PredictionConfig, cancel: &tokio_util::sync::CancellationToken) -> Result<Value, String> {
        let mut client = self.client.lock().await;
        let parent = rpc(self, &client, "thread/resume", json!({"threadId":source["thread_id"],"excludeTurns":true})).await?;
        if parent["modelProvider"] != "openai" || parent["collaborationMode"]["mode"] == "plan"
            || config.unsupported_models.iter().any(|model| parent["model"] == *model) {
            return Ok(json!({"status":"unavailable", "suggestion":null}));
        }
        require_head(self, &client, source["thread_id"].as_str().unwrap(), &source["turn_id"]).await?;
        let fork = rpc(self, &client, "thread/fork", json!({"threadId":source["thread_id"],"ephemeral":true,
            "experimentalPredictionMode":true,"excludeTurns":true,"threadSource":"composer_predictions"})).await?;
        let thread = fork["thread"]["id"].as_str().filter(|s| !s.is_empty()).ok_or("prediction fork has no thread")?.to_string();
        let mut turn_id = None;
        let mut deferred_events = Vec::new();
        let work = async {
            require_head(self, &client, source["thread_id"].as_str().unwrap(), &source["turn_id"]).await?;
            let prompt = format!("{}\n\nReturn only JSON matching this schema: {{\"type\":\"object\",\"properties\":{{\"suggestion\":{{\"type\":[\"string\",\"null\"]}}}},\"required\":[\"suggestion\"],\"additionalProperties\":false}}.", config.prompt);
            let started = rpc(self, &client, "turn/start", json!({"threadId":thread,"turnTrigger":"composer_predictions",
                "clientUserMessageId":request_id,"input":[{"type":"text","text":prompt,"text_elements":[]}],
                "effort": if config.prediction_reasoning_effort.is_empty() { Value::Null } else { json!(config.prediction_reasoning_effort) }})).await?;
            let turn = started["turn"]["id"].as_str().ok_or("prediction turn has no id")?.to_string();
            turn_id = Some(turn.clone());
            let mut answer = None;
            loop {
                let event = tokio::select! {
                    _ = cancel.cancelled() => return Err("prediction cancelled".into()),
                    event = client.next_event() => event,
                };
                match event {
                    Some(AppServerEvent::ServerNotification(notification)) => {
                        let value = serde_json::to_value(&notification).map_err(|e| e.to_string())?;
                        if value["params"]["threadId"] != thread {
                            deferred_events.push(AppServerEvent::ServerNotification(notification));
                            continue;
                        }
                        if value["method"] == "item/completed" && value["params"]["threadId"] == thread
                            && value["params"]["turnId"] == turn && value["params"]["item"]["type"] == "agentMessage" {
                            answer = value["params"]["item"]["text"].as_str().map(str::to_owned);
                        }
                        if value["method"] != "turn/completed" || value["params"]["threadId"] != thread
                            || value["params"]["turn"]["id"] != turn { continue; }
                        if value["params"]["turn"]["status"] != "completed" { return Err("prediction turn did not complete".into()); }
                        break;
                    }
                    Some(AppServerEvent::ServerRequest(request)) => {
                        let value = serde_json::to_value(&request).map_err(|e| e.to_string())?;
                        if value["params"]["threadId"] == thread { return Err("prediction requested an action instead of a suggestion".into()); }
                        deferred_events.push(AppServerEvent::ServerRequest(request));
                    }
                    Some(AppServerEvent::Disconnected { .. }) | None => return Err("prediction connection closed".into()),
                    Some(AppServerEvent::Lagged { .. }) => return Err("prediction output was lost".into()),
                    Some(event) => deferred_events.push(event),
                }
            }
            let text = answer.ok_or("prediction returned no completed message")?;
            let value: Value = serde_json::from_str(&text).map_err(|_| "prediction returned invalid JSON")?;
            if value.as_object().is_none_or(|object| object.len() != 1 || !object.contains_key("suggestion"))
                || !(value["suggestion"].is_null() || value["suggestion"].is_string()) { return Err("prediction returned an invalid suggestion".into()); }
            let suggestion = value["suggestion"].as_str().map(str::trim).filter(|s| !s.is_empty());
            Ok(json!({"status":"completed","suggestion":suggestion,"prompt_version":config.prompt_version,
                "prediction_thread_id":thread,"prediction_turn_id":turn}))
        };
        let result = tokio::time::timeout(Duration::from_secs(30), work).await
            .unwrap_or_else(|_| Err("prediction timed out".into()));
        if result.is_err() && let Some(turn) = turn_id {
            let _ = rpc(self, &client, "turn/interrupt", json!({"threadId":thread,"turnId":turn})).await;
        }
        let cleanup = rpc(self, &client, "thread/unsubscribe", json!({"threadId":thread})).await;
        client.restore_ready_events(deferred_events);
        cleanup?;
        if cancel.is_cancelled() { return Ok(json!({"status":"unavailable", "suggestion":null})); }
        result
    }
}

async fn rpc(driver: &CodexAppServerInterfaceDriver, client: &AppServerClient, method: &str, params: Value) -> Result<Value, String> {
    let id = driver.next_request_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let request: ClientRequest = serde_json::from_value(json!({"id":id,"method":method,"params":params})).map_err(|e| e.to_string())?;
    tokio::time::timeout(Duration::from_secs(10), client.request_typed(request)).await
        .map_err(|_| format!("{method} timed out"))?.map_err(|e| e.to_string())
}

async fn require_head(driver: &CodexAppServerInterfaceDriver, client: &AppServerClient, thread: &str, turn: &Value) -> Result<(), String> {
    let turns = rpc(driver, client, "thread/turns/list", json!({"threadId":thread,"limit":1,"sortDirection":"desc","itemsView":"notLoaded"})).await?;
    if turns["data"][0]["id"] != *turn || turns["data"][0]["status"] != "completed" {
        return Err("conversation advanced beyond the requested prediction source".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prediction_reuses_connection_preserves_foreign_events_and_cleans_up() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        for interrupted in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let cancel = tokio_util::sync::CancellationToken::new();
            let server_cancel = cancel.clone();
            let server = tokio::spawn(async move {
                let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0).await.unwrap();
                let mut methods = Vec::new();
                while let Some(Ok(Message::Text(text))) = socket.next().await {
                    let request: Value = serde_json::from_str(&text).unwrap();
                    if request.get("id").is_none() { continue; }
                    let method = request["method"].as_str().unwrap();
                    methods.push(method.to_string());
                    let result = match method {
                        "initialize" => json!({"userAgent":"test","codexHome":"/tmp"}),
                        "thread/resume" => json!({"modelProvider":"openai","model":"supported"}),
                        "thread/turns/list" => {
                            assert_eq!(request["params"]["threadId"], "parent");
                            json!({"data":[{"id":"source","status":"completed"}]})
                        }
                        "thread/fork" => {
                            assert_eq!(request["params"]["experimentalPredictionMode"], true);
                            assert_eq!(request["params"]["ephemeral"], true);
                            json!({"thread":{"id":"temporary"}})
                        }
                        "turn/start" => json!({"turn":{"id":"prediction"}}),
                        "turn/interrupt" | "thread/unsubscribe" => json!({}),
                        other => panic!("unexpected request {other}"),
                    };
                    socket.send(Message::Text(json!({"id":request["id"],"result":result}).to_string().into())).await.unwrap();
                    if method == "turn/start" {
                        socket.send(Message::Text(json!({"method":"item/completed","params":{
                            "threadId":"parent","turnId":"source","completedAtMs":1,"item":{"type":"agentMessage","id":"foreign","text":"Original answer"}
                        }}).to_string().into())).await.unwrap();
                        if interrupted { server_cancel.cancel(); }
                        else {
                            for notification in [
                                json!({"method":"item/completed","params":{"threadId":"temporary","turnId":"prediction","completedAtMs":1,
                                    "item":{"type":"agentMessage","id":"answer","text":"{\"suggestion\":\"Show an example\"}"}}}),
                                json!({"method":"turn/completed","params":{"threadId":"temporary",
                                    "turn":{"id":"prediction","status":"completed","items":[],"error":null}}}),
                            ] { socket.send(Message::Text(notification.to_string().into())).await.unwrap(); }
                        }
                    }
                }
                assert_eq!(methods.iter().filter(|m| *m == "thread/fork").count(), 1);
                assert_eq!(methods.last().unwrap(), "thread/unsubscribe");
                assert_eq!(methods.iter().any(|m| m == "turn/interrupt"), interrupted);
            });
            let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
                endpoint: crate::RemoteAppServerEndpoint::WebSocket { websocket_url: format!("ws://{address}"), auth_token: None },
                client_name:"test".into(), client_version:"0".into(), experimental_api:true,
                mcp_server_openai_form_elicitation:false, opt_out_notification_methods:Vec::new(), channel_capacity:16,
            }).await.unwrap();
            let driver = CodexAppServerInterfaceDriver::new(AppServerClient::Remote(client));
            let config = PredictionConfig { is_enabled:true, prompt:"Suggest".into(), prompt_version:"test".into(),
                prediction_reasoning_effort:"high".into(), unsupported_models:Vec::new() };
            let result = tokio::time::timeout(Duration::from_secs(5), driver.predict(
                &json!({"thread_id":"parent","turn_id":"source"}), "request", &config, &cancel)).await.unwrap().unwrap();
            assert_eq!(result["suggestion"], if interrupted { Value::Null } else { json!("Show an example") });
            let event = driver.client.lock().await.next_event().await.unwrap();
            let AppServerEvent::ServerNotification(notification) = event else { panic!("foreign event lost") };
            assert_eq!(serde_json::to_value(notification).unwrap()["params"]["threadId"], "parent");
            driver.shutdown().await.unwrap();
            server.await.unwrap();
        }
    }

    #[test]
    fn prediction_wire_preserves_native_temporary_fork_and_paginated_reads() {
        for (method, params) in [
            ("thread/fork", json!({"threadId":"parent", "ephemeral":true, "experimentalPredictionMode":true,
                "excludeTurns":true, "threadSource":"composer_predictions"})),
            ("thread/turns/list", json!({"threadId":"parent", "limit":1, "sortDirection":"desc", "itemsView":"notLoaded"})),
            ("turn/start", json!({"threadId":"temporary", "turnTrigger":"composer_predictions", "effort":"high",
                "clientUserMessageId":"prediction-1", "input":[{"type":"text","text":"Suggest the next message","text_elements":[]}]})),
        ] {
            let request: ClientRequest = serde_json::from_value(json!({"id":1,"method":method,"params":params})).unwrap();
            let encoded = serde_json::to_value(request).unwrap();
            for (key, value) in params.as_object().unwrap() {
                assert_eq!(&encoded["params"][key], value, "{method} lost {key}");
            }
        }
    }
}
