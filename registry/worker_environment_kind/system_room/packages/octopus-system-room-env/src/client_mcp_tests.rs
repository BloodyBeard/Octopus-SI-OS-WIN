use super::*;

fn config() -> Value {
    let descriptor: Value = serde_json::from_str(include_str!("../../../../../tool/client.mcp/tool.json")).unwrap();
    descriptor["handler_config"]["mcp"].clone()
}

#[test]
fn mcp_lifecycle_catalogue_and_exact_calls_are_validated() {
    let mut server = Server::new(&config()).unwrap();
    let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
    let Reply::Message(before) = server.receive(list.clone()) else { panic!() };
    assert_eq!(before["error"]["code"], -32002);
    let Reply::Message(init) = server.receive(json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}
    }})) else { panic!() };
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert!(matches!(server.receive(json!({"jsonrpc":"2.0","method":"notifications/initialized"})), Reply::Notification));
    let Reply::Message(tools) = server.receive(list) else { panic!() };
    let names = tools["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!(names, ["list_artifacts","read_artifact","search_artifacts","publish_widget","open_client_surface"]);
    let call = |name: &str, arguments: Value| json!({"jsonrpc":"2.0","id":"call","method":"tools/call","params":{"name":name,"arguments":arguments}});
    let Reply::Call { id, entry, arguments } = server.receive(call("read_artifact", json!({"id":"visible"}))) else { panic!() };
    assert_eq!(id, "call");
    assert_eq!(entry.name, "read_artifact");
    assert_eq!(arguments, json!({"id":"visible"}));
    for request in [call("read_artifact", json!({"id":"visible","path":"/private"})),
        call("read_artifact", json!({"id":"visible","max_bytes":1})),
        call("unknown", json!({})), call("publish_widget", json!({"interface":"foreign","document":{}})),
        call("open_client_surface", json!({"interface":"target","action":{"schema":"octopus.client_action","schema_version":1,"action":"exec","surface_id":"x"}})),
        json!({"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"file:///private"}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{"cursor":"old"}})
    ] {
        let Reply::Message(response) = server.receive(request) else { panic!() };
        assert!(response.get("error").is_some(), "{response}");
    }
    let mut duplicate = config();
    let first = duplicate["tools"][0].clone();
    duplicate["tools"].as_array_mut().unwrap().push(first);
    assert!(Server::new(&duplicate).is_err());
    let mut effect = config();
    effect["tools"][0]["operation"]["handler_config"]["operation"] = json!("append");
    assert!(Server::new(&effect).is_err());
}
