use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

pub async fn run_example_server() -> Result<(), String> {
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let stdout = Arc::new(tokio::sync::Mutex::new(tokio::io::BufWriter::new(
        tokio::io::stdout(),
    )));
    let mut stderr = tokio::io::BufWriter::new(tokio::io::stderr());
    stderr
        .write_all(b"octopus MCP stdio example server ready\n")
        .await
        .map_err(|error| error.to_string())?;
    stderr.flush().await.map_err(|error| error.to_string())?;
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| format!("example MCP stdin failed: {error}"))?
    {
        let request: Value = serde_json::from_str(&line)
            .map_err(|error| format!("example MCP request is malformed: {error}"))?;
        let object = request
            .as_object()
            .ok_or_else(|| "example MCP request is not an object".to_string())?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err("example MCP request jsonrpc is not '2.0'".to_string());
        }
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let Some(id) = object.get("id").cloned() else {
            if method == "notifications/initialized" {
                write_message(
                    &mut *stdout.lock().await,
                    &json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/tools/list_changed",
                        "params": {}
                    }),
                )
                .await?;
            }
            continue;
        };
        let stdout = Arc::clone(&stdout);
        tokio::spawn(async move {
            let delay_ms = request
                .pointer("/params/arguments/delay_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(10_000);
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            if method == "tools/call" {
                let event = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": {
                        "progressToken": id.clone(),
                        "progress": 1,
                    }
                });
                let _ = write_message(&mut *stdout.lock().await, &event).await;
            }
            let result = match method.as_str() {
                "initialize" => Ok(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {
                        "tools": { "listChanged": true },
                        "resources": { "subscribe": false, "listChanged": false },
                        "prompts": { "listChanged": false }
                    },
                    "serverInfo": { "name": "octopus-mcp-stdio-example", "version": "1" },
                    "instructions": "Use echo to verify the persistent MCP stdio room."
                })),
                "tools/list" => Ok(json!({
                    "tools": [{
                        "name": "echo",
                        "description": "Return the supplied text with a monotonically process-local invocation count.",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "text": { "type": "string" } },
                            "required": ["text"],
                            "additionalProperties": false
                        }
                    }]
                })),
                "tools/call" => {
                    let name = request.pointer("/params/name").and_then(Value::as_str);
                    let text = request
                        .pointer("/params/arguments/text")
                        .and_then(Value::as_str);
                    match (name, text) {
                        (Some("echo"), Some(text)) => Ok(json!({
                            "content": [{ "type": "text", "text": text }],
                            "structuredContent": { "echo": text },
                            "isError": false
                        })),
                        (Some(other), _) => Err((-32602, format!("unknown tool '{other}'"))),
                        _ => Err((-32602, "echo requires arguments.text".to_string())),
                    }
                }
                "resources/list" => Ok(json!({
                    "resources": [{
                        "uri": "octopus://room/readme",
                        "name": "room-readme",
                        "mimeType": "text/plain"
                    }]
                })),
                "resources/templates/list" => Ok(json!({
                    "resourceTemplates": [{
                        "uriTemplate": "octopus://room/{name}",
                        "name": "room-resource",
                        "mimeType": "text/plain"
                    }]
                })),
                "resources/read" => {
                    let uri = request.pointer("/params/uri").and_then(Value::as_str);
                    match uri {
                        Some("octopus://room/readme") => Ok(json!({
                            "contents": [{
                                "uri": "octopus://room/readme",
                                "mimeType": "text/plain",
                                "text": "persistent MCP stdio room"
                            }]
                        })),
                        Some(other) => Err((-32002, format!("resource '{other}' is unavailable"))),
                        None => Err((-32602, "resources/read requires uri".to_string())),
                    }
                }
                "prompts/list" => Ok(json!({
                    "prompts": [{
                        "name": "echo_prompt",
                        "description": "Create one user message for the echo tool.",
                        "arguments": [{ "name": "text", "required": true }]
                    }]
                })),
                "prompts/get" => {
                    let name = request.pointer("/params/name").and_then(Value::as_str);
                    let text = request
                        .pointer("/params/arguments/text")
                        .and_then(Value::as_str);
                    match (name, text) {
                        (Some("echo_prompt"), Some(text)) => Ok(json!({
                            "description": "Echo prompt",
                            "messages": [{
                                "role": "user",
                                "content": { "type": "text", "text": text }
                            }]
                        })),
                        _ => Err((
                            -32602,
                            "prompts/get requires echo_prompt and text".to_string(),
                        )),
                    }
                }
                "ping" => Ok(json!({})),
                other => Err((-32601, format!("method '{other}' is unavailable"))),
            };
            let response = match result {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err((code, message)) => {
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
                }
            };
            let _ = write_message(&mut *stdout.lock().await, &response).await;
        });
    }
    Ok(())
}

async fn write_message<W>(writer: &mut W, value: &Value) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| format!("example MCP response did not serialize: {error}"))?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| format!("example MCP stdout failed: {error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("example MCP stdout flush failed: {error}"))
}
