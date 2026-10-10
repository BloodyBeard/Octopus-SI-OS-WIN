use std::io::{Read, Write};
use std::path::PathBuf;

use octopus_secrets::{
    LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind, SecretsManager,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    interface_home: PathBuf,
    interface: String,
    name: String,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Serialize)]
struct Response {
    configured: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("octopus-secret-store failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|error| format!("stdin could not be read: {error}"))?;
    let request: Request =
        serde_json::from_slice(&input).map_err(|error| format!("request is malformed: {error}"))?;
    if !request.interface_home.is_absolute() {
        return Err("interface_home must be absolute".to_string());
    }
    let scope = SecretScope::named(&request.interface).map_err(|error| error.to_string())?;
    if request
        .interface_home
        .file_name()
        .and_then(|name| name.to_str())
        != Some(request.interface.as_str())
    {
        return Err("interface_home must end in the exact interface folder id".to_string());
    }
    let name = SecretName::new(&request.name).map_err(|error| error.to_string())?;
    let keyring = request.interface_home.join("secrets").join("keyring");
    let manager = SecretsManager::new_with_keyring_store(
        request.interface_home,
        SecretsBackendKind::Local,
        std::sync::Arc::new(LocalFileKeyringStore::new(keyring)),
    );
    let configured = match request.operation.as_str() {
        "status" => manager
            .get(&scope, &name)
            .map_err(|error| error.to_string())?
            .is_some(),
        "set" => {
            let value = request
                .value
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "set requires a non-empty value".to_string())?;
            manager
                .set(&scope, &name, value)
                .map_err(|error| error.to_string())?;
            true
        }
        "delete" => {
            manager
                .delete(&scope, &name)
                .map_err(|error| error.to_string())?;
            false
        }
        _ => return Err("operation must be status, set, or delete".to_string()),
    };
    let output = serde_json::to_vec(&Response { configured })
        .map_err(|error| format!("response could not be encoded: {error}"))?;
    std::io::stdout()
        .write_all(&output)
        .map_err(|error| format!("stdout could not be written: {error}"))
}
