use std::collections::BTreeSet;

use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_tool_door_runtime::{ToolProcessError, ToolProcessIo};
use octopus_tool_handlers::{ToolCall, ToolHandler, ToolHandlerContext, ToolHandlerResult};
use octopus_tool_registry::ToolArgumentValidator;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::artifact_bound::ArtifactBoundHandler;
use crate::artifact_send::{ArtifactSendError, decode_artifact_send_call, execute_artifact_send};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalogue {
    protocol_version: String,
    server_name: String,
    server_version: String,
    tools: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    description: String,
    input_schema: Value,
    operation: Operation,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Artifact { handler_config: Value },
    Emit { label: String, payload_argument: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(PartialEq)]
enum Phase { New, Initializing, Ready }

struct Server {
    catalogue: Catalogue,
    validators: Vec<ToolArgumentValidator>,
    phase: Phase,
}

enum Reply<'a> {
    Message(Value),
    Notification,
    Call { id: Value, entry: &'a Entry, arguments: Value },
}

impl Server {
    fn new(config: &Value) -> Result<Self, String> {
        let catalogue: Catalogue = serde_json::from_value(config.clone())
            .map_err(|error| format!("invalid MCP catalogue: {error}"))?;
        if catalogue.protocol_version != "2025-11-25"
            || catalogue.server_name.trim().is_empty()
            || catalogue.server_version.trim().is_empty()
            || catalogue.tools.is_empty()
        {
            return Err("MCP catalogue requires its supported protocol, server identity and tools".into());
        }
        let mut names = BTreeSet::new();
        let mut validators = Vec::new();
        for entry in &catalogue.tools {
            if entry.name.is_empty() || entry.name.trim() != entry.name
                || !names.insert(entry.name.clone()) || entry.description.trim().is_empty()
                || entry.input_schema.get("type") != Some(&json!("object"))
            {
                return Err("MCP tool names must be unique and carry descriptions and object schemas".into());
            }
            match &entry.operation {
                Operation::Artifact { handler_config } => {
                    if !matches!(handler_config.get("operation").and_then(Value::as_str), Some("read" | "list" | "search")) {
                        return Err("client MCP artifact operations are bounded read, list or search".into());
                    }
                }
                Operation::Emit { label, payload_argument } => {
                    if label.trim().is_empty() || payload_argument.trim().is_empty() {
                        return Err("MCP emission requires an exact label and payload argument".into());
                    }
                }
            }
            validators.push(ToolArgumentValidator::compile(&entry.input_schema)?);
        }
        Ok(Self { catalogue, validators, phase: Phase::New })
    }

    fn receive(&mut self, message: Value) -> Reply<'_> {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let request: Request = match serde_json::from_value(message) {
            Ok(value) => value,
            Err(error) => return Reply::Message(rpc_error(id, -32600, error.to_string())),
        };
        if request.jsonrpc != "2.0" || request.id.as_ref().is_some_and(|id| !(id.is_string() || id.is_i64() || id.is_u64())) {
            return Reply::Message(rpc_error(Value::Null, -32600, "invalid JSON-RPC version or request id"));
        }
        if request.method == "notifications/initialized" && request.id.is_none() {
            if self.phase == Phase::Initializing && (request.params.is_null() || request.params.as_object().is_some_and(|p| p.is_empty())) {
                self.phase = Phase::Ready;
            }
            return Reply::Notification;
        }
        let Some(id) = request.id else { return Reply::Notification; };
        if request.method == "initialize" {
            if self.phase != Phase::New || !request.params.get("protocolVersion").is_some_and(Value::is_string)
                || !request.params.get("capabilities").is_some_and(Value::is_object)
                || !request.params.pointer("/clientInfo/name").is_some_and(Value::is_string)
                || !request.params.pointer("/clientInfo/version").is_some_and(Value::is_string)
            {
                return Reply::Message(rpc_error(id, -32602, "invalid MCP initialization"));
            }
            self.phase = Phase::Initializing;
            return Reply::Message(rpc_result(id, json!({
                "protocolVersion": self.catalogue.protocol_version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": self.catalogue.server_name, "version": self.catalogue.server_version}
            })));
        }
        if request.method == "ping" {
            return Reply::Message(rpc_result(id, json!({})));
        }
        if self.phase != Phase::Ready {
            return Reply::Message(rpc_error(id, -32002, "MCP connection is not initialized"));
        }
        let params = match request.params.as_object() {
            Some(params) => params,
            None if request.method == "tools/list" && request.params.is_null() => {
                return Reply::Message(rpc_result(id, self.tools()));
            }
            None => return Reply::Message(rpc_error(id, -32602, "MCP params must be an object")),
        };
        match request.method.as_str() {
            "tools/list" if params.keys().all(|key| key == "_meta") => Reply::Message(rpc_result(id, self.tools())),
            "tools/list" => Reply::Message(rpc_error(id, -32602, "this catalogue has no continuation cursor")),
            "tools/call" => {
                if params.keys().any(|key| !matches!(key.as_str(), "name" | "arguments" | "_meta")) {
                    return Reply::Message(rpc_error(id, -32602, "unknown tools/call parameter"));
                }
                let Some(index) = self.catalogue.tools.iter().position(|entry| params.get("name").and_then(Value::as_str) == Some(entry.name.as_str())) else {
                    return Reply::Message(rpc_error(id, -32602, "tool is not in this MCP catalogue"));
                };
                let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                if let Err(error) = self.validators[index].validate(&arguments) {
                    return Reply::Message(rpc_error(id, -32602, error));
                }
                Reply::Call { id, entry: &self.catalogue.tools[index], arguments }
            }
            _ => Reply::Message(rpc_error(id, -32601, "MCP method is not supported")),
        }
    }

    fn tools(&self) -> Value {
        json!({"tools": self.catalogue.tools.iter().map(|entry| json!({
            "name": entry.name, "description": entry.description, "inputSchema": entry.input_schema
        })).collect::<Vec<_>>()})
    }
}

fn rpc_result(id: Value, result: Value) -> Value { json!({"jsonrpc":"2.0", "id":id, "result":result}) }
fn rpc_error(id: Value, code: i64, message: impl ToString) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message.to_string()}})
}

fn initialized_server(config: &Value) -> Result<Server, String> {
    let mut server = Server::new(config)?;
    let version = server.catalogue.protocol_version.clone();
    let initialized = server.receive(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
        "protocolVersion":version,"capabilities":{},"clientInfo":{"name":"octopus-artifact-transport","version":"1"}
    }}));
    match initialized {
        Reply::Message(response) if response.pointer("/result/protocolVersion") == Some(&json!(version)) => {}
        _ => return Err("client MCP protocol negotiation failed".into()),
    }
    server.receive(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    Ok(server)
}

pub(super) async fn execute(
    input: &InterfaceInvocationEnvelope,
    context: &ToolHandlerContext,
    call: &ToolCall,
    io: &ToolProcessIo,
) -> Result<ToolHandlerResult, ToolProcessError> {
    let mut server = initialized_server(&call.handler_config["mcp"])
        .map_err(ToolProcessError::Capability)?;
    let response = match server.receive(json!({"jsonrpc":"2.0","id":1,
        "method":call.arguments["method"],"params":call.arguments.get("params").cloned().unwrap_or_else(||json!({}))
    })) {
        Reply::Message(response) => response,
        Reply::Notification => return Err(ToolProcessError::Capability("client MCP request produced no response".into())),
        Reply::Call { id, entry, arguments } => {
            let result = match &entry.operation {
                Operation::Artifact { handler_config } => ArtifactBoundHandler.execute(context, &ToolCall {
                    tool_id: call.tool_id.clone(), arguments, handler_config: handler_config.clone(),
                }).await.map(|result| result.body).map_err(|error| error.to_string()),
                Operation::Emit { label, payload_argument } => {
                    let send = json!({"interface":arguments["interface"],"label":label,"payload":arguments[payload_argument]});
                    match decode_artifact_send_call(input, &send) {
                        Err(error) => Err(error.to_string()),
                        Ok(send) => match execute_artifact_send(input, io, send).await {
                            Ok(verdict) => Ok(verdict),
                            Err(ArtifactSendError::Capability(error)) => Err(error),
                            Err(ArtifactSendError::Unavailable(error)) => return Err(ToolProcessError::Unavailable(error)),
                        },
                    }
                }
            };
            let (body, failed) = match result {
                Ok(body) => { let refused = body.get("refused").is_some(); (body, refused) },
                Err(error) => (json!({"error":error}), true),
            };
            rpc_result(id, json!({"content":[{"type":"text","text":body.to_string()}],"structuredContent":body,"isError":failed}))
        }
    };
    Ok(ToolHandlerResult::new(response))
}

#[cfg(test)]
#[path = "client_mcp_tests.rs"]
mod tests;
