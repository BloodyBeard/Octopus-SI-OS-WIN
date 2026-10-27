use std::collections::HashMap;
use std::path::PathBuf;

use crate::diff_model::FileChange;
use octopus_substrate_contract::approvals_substrate::NetworkPolicyRuleAction;
use octopus_substrate_contract::permission_profile::FileSystemPermissions;
use octopus_substrate_contract::permission_profile::NetworkPermissions;
#[cfg(test)]
use octopus_world_terminal_client::ApprovalDetail;
#[cfg(test)]
use octopus_world_terminal_client::ApprovalOptionItem;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestPermissionProfile {
    pub network: Option<NetworkPermissions>,
    pub file_system: Option<FileSystemPermissions>,
}

impl RequestPermissionProfile {
    pub(crate) fn is_empty(&self) -> bool {
        self.network.is_none() && self.file_system.is_none()
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionGrantScope {
    #[default]
    Turn,
    Session,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct RequestPermissionsResponse {
    pub permissions: RequestPermissionProfile,
    #[serde(default)]
    pub scope: PermissionGrantScope,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict_auto_review: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub(crate) struct ExecPolicyAmendment {
    pub command: Vec<String>,
}

impl ExecPolicyAmendment {
    pub(crate) fn new(command: Vec<String>) -> Self {
        Self { command }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct NetworkPolicyAmendment {
    pub host: String,
    pub action: NetworkPolicyRuleAction,
}

#[cfg(test)]
pub(crate) fn default_approval_options(detail: &ApprovalDetail) -> Vec<ApprovalOptionItem> {
    let mut options = vec![ApprovalOptionItem {
        kind: "accept".to_string(),
        label: None,
        command_prefix: None,
        decision: serde_json::json!("accept"),
    }];
    if detail.network.is_some() {
        options.push(ApprovalOptionItem {
            kind: "accept_for_session".to_string(),
            label: None,
            command_prefix: None,
            decision: serde_json::json!("acceptForSession"),
        });
    }
    options.push(ApprovalOptionItem {
        kind: "cancel".to_string(),
        label: None,
        command_prefix: None,
        decision: serde_json::json!("cancel"),
    });
    options
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ExecApprovalRequestEvent {
    pub(crate) request_id: String,
    pub(crate) command: Vec<String>,
    #[serde(default)]
    pub(crate) cwd: String,
    pub(crate) reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) environment_id: Option<String>,
    #[serde(skip, default)]
    pub(crate) detail: ApprovalDetail,
    #[serde(skip, default)]
    pub(crate) options: Vec<ApprovalOptionItem>,
}

#[cfg(test)]
impl ExecApprovalRequestEvent {
    #[cfg(test)]
    pub(crate) fn effective_options(&self) -> Vec<ApprovalOptionItem> {
        if self.options.is_empty() {
            default_approval_options(&self.detail)
        } else {
            self.options.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ApplyPatchApprovalRequestEvent {
    pub(crate) request_id: String,
    pub(crate) changes: HashMap<PathBuf, FileChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) grant_root: Option<PathBuf>,
}

pub(crate) fn file_changes_from_detail(detail: &serde_json::Value) -> HashMap<PathBuf, FileChange> {
    let mut out = HashMap::new();
    if let Some(changes) = detail
        .get("fileChanges")
        .or_else(|| detail.get("file_changes"))
        .and_then(serde_json::Value::as_object)
    {
        for (path, change) in changes {
            if let Ok(change) = serde_json::from_value::<FileChange>(change.clone()) {
                out.insert(PathBuf::from(path), change);
            }
        }
    }
    let Some(changes) = detail.get("changes").and_then(serde_json::Value::as_array) else {
        return out;
    };
    for change in changes {
        let Some(path) = change.get("path").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let diff = change
            .get("diff")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let kind = change
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("update");
        let file_change = match kind {
            "add" => FileChange::Add { content: diff },
            "delete" => FileChange::Delete { content: diff },
            _ => FileChange::Update {
                unified_diff: diff,
                move_path: change
                    .get("move_path")
                    .and_then(serde_json::Value::as_str)
                    .map(PathBuf::from),
            },
        };
        out.insert(PathBuf::from(path), file_change);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_cover_plain_and_network_approvals() {
        let plain = default_approval_options(&ApprovalDetail::default());
        assert_eq!(
            plain.iter().map(|o| o.kind.as_str()).collect::<Vec<_>>(),
            vec!["accept", "cancel"]
        );

        let network = default_approval_options(&ApprovalDetail {
            network: Some(octopus_world_terminal_client::NetworkApprovalScope {
                host: "example.com".into(),
                protocol: "https".into(),
                port: None,
            }),
            ..Default::default()
        });
        assert_eq!(
            network.iter().map(|o| o.kind.as_str()).collect::<Vec<_>>(),
            vec!["accept", "accept_for_session", "cancel"]
        );
    }

    #[test]
    fn decodes_file_changes_from_detail() {
        let detail = serde_json::json!({
            "changes": [
                {"path": "a.txt", "kind": "add", "diff": "hello\n"},
                {"path": "b.txt", "kind": "update", "diff": "@@ -1 +1 @@", "move_path": "c.txt"},
                {"path": "d.txt", "kind": "delete", "diff": "bye\n"},
            ]
        });
        let changes = file_changes_from_detail(&detail);
        assert_eq!(changes.len(), 3);
        assert!(matches!(
            changes.get(std::path::Path::new("a.txt")),
            Some(FileChange::Add { content }) if content == "hello\n"
        ));
        assert!(matches!(
            changes.get(std::path::Path::new("b.txt")),
            Some(FileChange::Update { move_path: Some(p), .. }) if p == std::path::Path::new("c.txt")
        ));
    }
}
