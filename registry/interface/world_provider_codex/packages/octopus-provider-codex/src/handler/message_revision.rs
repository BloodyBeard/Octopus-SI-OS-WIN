use super::*;

pub(crate) struct RevisedMessageHistory {
    pub(crate) artifacts: Vec<SessionContextArtifact>,
    pub(crate) revision: Option<ArtifactRef>,
    pub(crate) replaces: Option<ArtifactRef>,
}

pub(crate) fn revised_message_history(
    session: &[SessionContextArtifact],
    source: &ArtifactRef,
    label: &str,
    bytes: &[u8],
) -> Result<RevisedMessageHistory, InterfaceDriverError> {
    let mut history = RevisedMessageHistory { artifacts: Vec::new(), revision: None, replaces: None };
    let receipts = session.iter().filter(|entry| entry.label == INTERFACE_CONTEXT_UPDATED_LABEL
        && entry.payload["phase"] == "message_revised").map(|entry| {
        let source: ArtifactRef = serde_json::from_value(entry.payload["source_artifact"].clone())
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        let target: ArtifactRef = serde_json::from_value(entry.payload["replaces_message"].clone())
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        Ok((continuity_source_key(&source), continuity_source_key(&target)))
    }).collect::<Result<BTreeSet<_>, InterfaceDriverError>>()?;
    let incoming = if label == OPERATOR_MESSAGE_LABEL {
        let payload: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?;
        payload.get("replaces_message").is_some().then(|| SessionContextArtifact {
            label: label.into(), source_artifact: source.clone(), source_movement_seq: None,
            payload,
        })
    } else { None };
    for entry in session.iter().filter(|entry| incoming.is_none() || &entry.source_artifact != source).chain(incoming.iter()) {
        if entry.label == OPERATOR_MESSAGE_LABEL && let Some(value) = entry.payload.get("replaces_message") {
            let target: ArtifactRef = serde_json::from_value(value.clone())
                .map_err(|error| InterfaceDriverError::Protocol(format!("Invalid message revision: {error}")))?;
            if &entry.source_artifact != source && !receipts.contains(&(
                continuity_source_key(&entry.source_artifact), continuity_source_key(&target))) {
                continue;
            }
            let position = history.artifacts.iter().rposition(|item| item.label == OPERATOR_MESSAGE_LABEL)
                .filter(|position| history.artifacts[*position].source_artifact == target)
                .ok_or_else(|| InterfaceDriverError::FailedClosed("Only the latest admitted operator message can be revised".into()))?;
            if history.artifacts[position].payload.get("attachments") != entry.payload.get("attachments") {
                return Err(InterfaceDriverError::FailedClosed("Message revision changed its retained attachments".into()));
            }
            history.artifacts.truncate(position);
            history.revision = Some(entry.source_artifact.clone());
            if &entry.source_artifact == source { history.replaces = Some(target); }
        }
        history.artifacts.push(entry.clone());
    }
    Ok(history)
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::ArtifactId;
    use octopus_ledger::ContentHash;

    #[test]
    fn message_revision_replays_only_the_selected_prefix_and_preserves_evidence() {
        let entry = |name: &str, label: &str, text: &str| SessionContextArtifact {
            source_artifact: ArtifactRef { id: ArtifactId::new(name), content: ContentHash::of_bytes(name.as_bytes()) },
            label: label.into(), payload: serde_json::json!({"text":text,"attachments":[]}), source_movement_seq: None,
        };
        let prior = entry("prior", INTERFACE_ARTIFACT_OUTPUT_LABEL, "Earlier answer");
        let old = entry("old", OPERATOR_MESSAGE_LABEL, "Original");
        let answer = entry("answer", INTERFACE_ARTIFACT_OUTPUT_LABEL, "Discarded reply");
        let mut edit = entry("edit", OPERATOR_MESSAGE_LABEL, "Corrected");
        edit.payload["replaces_message"] = serde_json::to_value(&old.source_artifact).unwrap();
        let session = vec![prior.clone(), old.clone(), answer.clone()];
        let bytes = serde_json::to_vec(&edit.payload).unwrap();
        let selected = revised_message_history(&session, &edit.source_artifact, OPERATOR_MESSAGE_LABEL, &bytes).unwrap();
        assert_eq!(selected.artifacts.iter().map(|entry| &entry.source_artifact).collect::<Vec<_>>(), vec![&prior.source_artifact, &edit.source_artifact]);
        assert_eq!(session[1].payload["text"], "Original");
        let mut retained = session.clone(); retained.push(edit.clone());
        let mut receipt = entry("receipt", INTERFACE_CONTEXT_UPDATED_LABEL, "");
        receipt.payload = serde_json::json!({"phase":"message_revised", "source_artifact":edit.source_artifact,
            "replaces_message":old.source_artifact});
        retained.push(receipt);
        let next = entry("next", OPERATOR_MESSAGE_LABEL, "Follow up");
        let replay = revised_message_history(&retained, &next.source_artifact, OPERATOR_MESSAGE_LABEL, &serde_json::to_vec(&next.payload).unwrap()).unwrap();
        assert_eq!(replay.revision, Some(edit.source_artifact.clone()));
        assert_eq!(replay.artifacts.len(), 3);
        retained.push(next);
        assert!(revised_message_history(&retained, &old.source_artifact, OPERATOR_MESSAGE_LABEL, &bytes).is_err());
        edit.payload["attachments"] = serde_json::json!([{"payload_path":"/other"}]);
        assert!(revised_message_history(&session, &edit.source_artifact, OPERATOR_MESSAGE_LABEL, &serde_json::to_vec(&edit.payload).unwrap()).is_err());
    }
}
