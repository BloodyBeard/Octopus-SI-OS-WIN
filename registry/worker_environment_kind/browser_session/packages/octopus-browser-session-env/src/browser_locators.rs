use super::*;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BrowserLoadWait {
    scope: BrowserLoadScope,
    classification_timeout_ms: u64,
    completion_timeout_ms: u64,
    start_event: Option<String>,
    completion_event: Option<String>,
    outcome: BrowserLoadOutcome,
    phase: BrowserLoadPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
#[derive(serde::Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum BrowserLoadScope { Page, Frame }
#[derive(serde::Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum BrowserLoadPhase { Classification, Completion }
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum BrowserLoadOutcome { ClassificationDeadline, CompletionDeadline, LoadCompleted, ObservationError }
impl BrowserLoadWait {
    fn valid(&self) -> bool {
        let start = self.start_event.as_deref().is_some_and(|event| matches!(event, "Page.frameStartedLoading" | "Page.frameNavigated" | "Page.navigatedWithinDocument"));
        let end = self.completion_event.as_deref().is_some_and(|event| matches!(event, "Page.domContentEventFired" | "Page.loadEventFired"));
        self.classification_timeout_ms == 250 && self.completion_timeout_ms <= 3000 && match self.outcome {
            BrowserLoadOutcome::ClassificationDeadline => self.phase == BrowserLoadPhase::Classification && self.start_event.is_none() && self.completion_event.is_none() && self.error.is_none(),
            BrowserLoadOutcome::CompletionDeadline => self.phase == BrowserLoadPhase::Completion && start && self.completion_event.is_none() && self.error.is_none(),
            BrowserLoadOutcome::LoadCompleted => self.phase == BrowserLoadPhase::Completion && start && end && self.error.is_none(),
            BrowserLoadOutcome::ObservationError => self.completion_event.is_none() && self.error.as_ref().is_some_and(|value| !value.is_empty() && value.chars().count() <= 4096) && if self.phase == BrowserLoadPhase::Classification { self.start_event.is_none() } else { start },
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BrowserSnapshotObservation {
    iframe_budget_ms: u64,
    per_frame_timeout_ms: u64,
    frames: Vec<BrowserSnapshotFrame>,
}
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BrowserSnapshotFrame {
    selector: String,
    outcome: BrowserSnapshotFrameOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
#[derive(serde::Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum BrowserSnapshotFrameOutcome { Expanded, Unavailable, Deadline }
impl BrowserSnapshotObservation {
    fn valid(&self) -> bool {
        self.iframe_budget_ms == 1000 && self.per_frame_timeout_ms == 500
            && self.frames.iter().all(|frame| !frame.selector.is_empty()
                && match frame.outcome {
                    BrowserSnapshotFrameOutcome::Unavailable => frame.error.as_ref().is_some_and(|error| !error.is_empty() && error.chars().count() <= 4096),
                    _ => frame.error.is_none(),
                })
    }
}

impl BrowserRoom {
    pub(super) async fn browser_family(&self, decoded: &DecodedRoomInvocation, recorder: &dyn WorkerEnvironmentRecorder) -> Result<Value, String> {
        if !matches!(self.transport, BrowserTransport::Native(_)) {
            return Err("This typed browser operation requires the declared native browser transport".into());
        }
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let context_id = room.browser_context_id.clone();
        let (_, page) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = page.session.as_mut().ok_or("Browser operation has no existing owned page")?;
        let result = session.command(&decoded.call.tool_id, "OctopusBrowser.perform", json!({
            "arguments":decoded.call.arguments,
            "page_editing_profile":self.page_editing_profile,
            "limits":{"max_type_chars":self.limits.max_type_chars,"max_eval_chars":self.limits.max_eval_chars},
        })).await?;
        if result["browser_context_id"].as_str() != context_id.as_deref()
            || result["target_id"].as_str() != Some(page.target_id.as_str())
            || result.get("value").is_none()
            || result.get("effective_timeout_ms").is_none()
            || !(result["effective_timeout_ms"].is_null() || result["effective_timeout_ms"].as_u64().is_some_and(|value| value <= 5000))
            || !result["frame_resolution_may_scroll"].is_boolean()
            || !result["url"].is_string() || !result["title"].is_string() {
            return Err("Typed browser result has invalid page or operation evidence".into());
        }
        let load_waits: Vec<BrowserLoadWait> = serde_json::from_value(result["load_waits"].clone()).map_err(|error| format!("Invalid browser load-wait evidence: {error}"))?;
        let load_scopes_match = match load_waits.as_slice() {
            [] => !matches!(decoded.operation, BrowserOperation::Click | BrowserOperation::DomClick),
            [page] => page.scope == BrowserLoadScope::Page,
            [frame, page] => frame.scope == BrowserLoadScope::Frame && page.scope == BrowserLoadScope::Page,
            _ => false,
        };
        if !load_scopes_match || load_waits.iter().any(|wait| !wait.valid()) { return Err("Invalid browser load-wait evidence".into()); }
        let snapshot_observation: Option<BrowserSnapshotObservation> = match decoded.operation {
            BrowserOperation::PlaywrightDomSnapshot => {
                if !result["value"].is_string() { return Err("Playwright DOM snapshot must return its actual string observation".into()); }
                let observation: BrowserSnapshotObservation = serde_json::from_value(result["snapshot_observation"].clone()).map_err(|error| format!("Invalid Playwright DOM snapshot evidence: {error}"))?;
                if !observation.valid() { return Err("Invalid conditional frame snapshot observation".into()); }
                Some(observation)
            }
            _ => {
                if result.get("snapshot_observation").is_some() { return Err("Unexpected snapshot evidence for this browser operation".into()); }
                None
            }
        };
        page.target_url = Some(result["url"].as_str().ok_or("Browser result omitted current URL")?.to_string());
        page.target_title = Some(result["title"].as_str().ok_or("Browser result omitted current title")?.to_string());
        Ok(json!({"result":decoded.call.tool_id,"worker_environment_id":decoded.formation.worker_environment_id,
            "target_id":page.target_id,"browser_context_id":result["browser_context_id"],"value":result["value"],
            "effective_timeout_ms":result["effective_timeout_ms"],"load_waits":load_waits,"frame_resolution_may_scroll":result["frame_resolution_may_scroll"],
            "snapshot_observation":snapshot_observation,"url":page.target_url,"title":page.target_title}))
    }

    pub(super) async fn browser_family_control(&self, decoded: &DecodedRoomInvocation) -> Result<Value, String> {
        if !matches!(self.transport, BrowserTransport::Native(_)) {
            return Err("This browser room operation requires the declared native browser transport".into());
        }
        let fields: &[&str] = match decoded.operation {
            BrowserOperation::ClipboardRead | BrowserOperation::VisibilityGet => &[],
            BrowserOperation::ClipboardWrite => &["items"],
            BrowserOperation::VisibilitySet => &["visible"],
            _ => return Err("Invalid room browser operation".into()),
        };
        let object = page_object(&decoded.call.tool_id, &decoded.call.arguments, fields)?;
        if decoded.operation == BrowserOperation::VisibilitySet && optional_bool(object, "visible")?.is_none() {
            return Err("browser.visibility_set requires visible".into());
        }
        if decoded.operation == BrowserOperation::ClipboardWrite {
            let items = object.get("items").filter(|value| value.is_array()).ok_or("browser.clipboard_write requires items")?;
            if serde_json::to_vec(items).map_err(|error| error.to_string())?.len() > self.limits.max_type_chars {
                return Err("Virtual clipboard exceeds the declared room byte limit".into());
            }
        }
        let room = self.room(&decoded.formation).await?;
        let mut state = room.lock().await;
        let requested = requested_target(decoded)?;
        self.ensure_browser_context(&mut state, requested.is_none()).await?;
        let target = if matches!(decoded.operation, BrowserOperation::ClipboardRead | BrowserOperation::ClipboardWrite) { requested } else { requested.or_else(|| state.target_id.clone()) };
        if let Some(id) = &target { self.require_context_target(&mut state, id).await?; }
        let result = self.browser_command(&mut state, "OctopusBrowser.roomOperation", json!({"arguments":decoded.call.arguments,"target_id":target,"max_clipboard_bytes":self.limits.max_type_chars})).await?;
        if result["browser_context_id"].as_str() != state.browser_context_id.as_deref()
            || result["target_id"].as_str() != target.as_deref()
            || result.get("value").is_none() {
            return Err("Browser room operation returned another context or target".into());
        }
        Ok(json!({"result":decoded.call.tool_id,"worker_environment_id":decoded.formation.worker_environment_id,
            "browser_context_id":state.browser_context_id,"target_id":target,"value":result["value"]}))
    }
}
