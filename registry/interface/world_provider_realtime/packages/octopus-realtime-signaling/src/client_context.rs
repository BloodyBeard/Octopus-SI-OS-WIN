use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientContextEntry {
    pub name: String,
    pub text: String,
}

pub fn validate(context: Option<&[ClientContextEntry]>) -> Result<(), String> {
    let Some(entries) = context else {
        return Ok(());
    };
    if entries.is_empty() || entries.len() > 4 {
        return Err("client_context must carry 1 to 4 entries".into());
    }
    let mut names = BTreeSet::new();
    let mut total = 0;
    for entry in entries {
        let name = entry.name.as_bytes();
        if name.is_empty()
            || name.len() > 64
            || !name[0].is_ascii_lowercase()
            || name
                .iter()
                .any(|byte| !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && *byte != b'_')
            || !names.insert(&entry.name)
        {
            return Err("client_context names must be unique lowercase tokens".into());
        }
        if entry.text.is_empty() || entry.text.trim() != entry.text {
            return Err("client_context text must be nonempty and trimmed".into());
        }
        let limit = match entry.name.as_str() {
            "client_observation" => 4 * 1024,
            "home_state" => 12 * 1024,
            _ => 16 * 1024,
        };
        if entry.text.len() > limit {
            return Err(format!(
                "client_context '{}' exceeds {limit} bytes",
                entry.name
            ));
        }
        total += entry.text.len();
        if total > 16 * 1024 {
            return Err("client_context exceeds 16384 bytes".into());
        }
    }
    Ok(())
}
