use super::*;
use serde::Serialize;

const MAX_REVISION: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ViewportSize {
    width: u32,
    height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ViewportIntent {
    revision: u64,
    viewport: Option<ViewportSize>,
    artifact_content_hash: String,
    tool_id: String,
    tool_descriptor_hash: String,
    formation_pin: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModelViewport {
    revision: u64,
    #[serde(rename = "emulatedSize")]
    emulated_size: Option<ViewportSize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ApplicationStatus { Applying, Applied, Uncertain }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ViewportApplication {
    intent: ViewportIntent,
    status: ApplicationStatus,
    debugger_epoch: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ViewportPage {
    model_viewport: Option<ModelViewport>,
    application: Option<ViewportApplication>,
    debugger_epoch: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ViewportState {
    pub(super) pending: Option<ViewportIntent>,
    last_intent: Option<ViewportIntent>,
    pages: BTreeMap<String, ViewportPage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeViewportState {
    schema_version: u32,
    worker_environment_id: WorkerEnvironmentId,
    formation_pin: String,
    host_generation: String,
    engine_handle: String,
    browser_context_id: String,
    target_id: Option<String>,
    last_intent: Option<ViewportIntent>,
    pending: Option<ViewportIntent>,
    model_viewport: Option<ModelViewport>,
    application: Option<ViewportApplication>,
    debugger_epoch: Option<u64>,
}

impl ViewportSize {
    fn validate(&self) -> Result<(), String> {
        if !(240..=4096).contains(&self.width) || !(160..=4096).contains(&self.height) {
            return Err("viewport size is outside the source-defined normalized bounds".into());
        }
        Ok(())
    }
}
impl ViewportIntent {
    fn validate(&self, formation: &WorkerEnvironmentFormation) -> Result<(), String> {
        if self.revision == 0 || self.revision > MAX_REVISION
            || self.formation_pin != formation.formation_pin
            || formation.tool_descriptor_hashes.get(&self.tool_id).map(String::as_str) != Some(self.tool_descriptor_hash.as_str())
            || self.artifact_content_hash.len() != 64 || !self.artifact_content_hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("viewport intent has invalid formed source provenance".into());
        }
        if let Some(size) = &self.viewport { size.validate()?; }
        Ok(())
    }
}
impl ViewportState {
    pub(super) fn recover(&mut self, formation: &WorkerEnvironmentFormation) -> Result<(), String> {
        for intent in [&self.pending, &self.last_intent].into_iter().flatten() { intent.validate(formation)?; }
        if self.pending.as_ref().is_some_and(|intent| intent.viewport.is_none()) { return Err("pending viewport cannot contain a reset".into()); }
        for (id, page) in &mut self.pages {
            validate_target_id(id)?;
            if let Some(application) = &mut page.application {
                application.intent.validate(formation)?;
                if application.status == ApplicationStatus::Applying { application.status = ApplicationStatus::Uncertain; }
            }
            if let Some(model) = &page.model_viewport {
                if model.revision > MAX_REVISION { return Err("invalid applied viewport revision".into()); }
                if let Some(size) = &model.emulated_size { size.validate()?; }
            }
            page.model_viewport = None;
        }
        Ok(())
    }
    pub(super) fn retain_targets(&mut self, targets: &[CdpTargetInfo]) {
        self.pages.retain(|id, _| targets.iter().any(|target| &target.target_id == id));
    }
    pub(super) fn disconnected(&mut self, id: &str) {
        if let Some(page) = self.pages.get_mut(id) {
            page.model_viewport = None;
            if let Some(application) = &mut page.application { application.status = ApplicationStatus::Uncertain; }
        }
    }
}

fn requested_viewport(decoded: &DecodedRoomInvocation) -> Result<Option<ViewportSize>, String> {
    if decoded.operation == BrowserOperation::ViewportReset {
        page_object(&decoded.call.tool_id, &decoded.call.arguments, &[])?;
        return Ok(None);
    }
    let object = page_object(&decoded.call.tool_id, &decoded.call.arguments, &["width", "height"])?;
    let dimension = |name: &str, minimum: f64| -> Result<u32, String> {
        let value = object.get(name).and_then(Value::as_f64).filter(|v| v.is_finite() && *v > 0.0 && v.fract() == 0.0)
            .ok_or_else(|| format!("viewport {name} must be a positive integer"))?;
        Ok(value.clamp(minimum, 4096.0) as u32)
    };
    Ok(Some(ViewportSize { width: dimension("width", 240.0)?, height: dimension("height", 160.0)? }))
}

impl BrowserRoom {
    pub(super) async fn viewport(&self, decoded: &DecodedRoomInvocation) -> Result<Value, String> {
        let viewport = requested_viewport(decoded)?;
        let intent = ViewportIntent {
            revision: decoded.source_movement_seq, viewport,
            artifact_content_hash: decoded.envelope.artifact.content.to_hex(),
            tool_id: decoded.call.tool_id.clone(),
            tool_descriptor_hash: decoded.formation.tool_descriptor_hashes.get(&decoded.call.tool_id).ok_or("viewport tool has no descriptor pin")?.clone(),
            formation_pin: decoded.formation.formation_pin.clone(),
        };
        intent.validate(&decoded.formation)?;
        let room = self.room(&decoded.formation).await?;
        let mut state = room.lock().await;
        let requested = requested_target(decoded)?;
        self.ensure_browser_context(&mut state, requested.is_none()).await?;
        let targets = self.context_targets(&mut state).await?;
        let target = requested.or_else(|| state.target_id.clone());
        if let Some(id) = &target {
            let member = self.context_target(&mut state, id).await?;
            self.connect_target(&mut state, member, decoded, false, None).await?;
            if matches!(self.transport, BrowserTransport::Native(_)) { self.read_native_viewport(&mut state, decoded, Some(id)).await?; }
        } else if !targets.is_empty() {
            return Err("viewport requires an exact target_id when this nonempty room has no selected tab".into());
        } else if matches!(self.transport, BrowserTransport::Native(_)) {
            self.read_native_viewport(&mut state, decoded, None).await?;
        }
        if state.viewport.last_intent.as_ref().is_some_and(|old| old.revision >= intent.revision) {
            return Err("viewport intent has already been recorded; metrics were not replayed".into());
        }
        if let Some(page) = target.as_ref().and_then(|id| state.pages.get_mut(id)) {
            page.refs.clear(); if let Some(session) = &mut page.session { session.observation = ObservationState::default(); }
        }
        state.viewport.last_intent = Some(intent.clone());
        state.viewport.pending = None;
        if matches!(self.transport, BrowserTransport::Native(_)) {
            if let Some(id) = &target { state.viewport.disconnected(id); }
            self.persist_viewport(&state, decoded)?;
            let result = if let Some(id) = &target {
                state.pages.get_mut(id).and_then(|page| page.session.as_mut()).ok_or("viewport target has no control connection")?
                    .command(&decoded.call.tool_id, "OctopusBrowser.setViewport", json!({"viewport":intent.viewport})).await?
            } else { self.browser_command(&mut state, "OctopusBrowser.setViewport", json!({"viewport":intent.viewport})).await? };
            self.accept_native_viewport(&mut state, decoded, result, Some(&intent), target.as_deref()).await?;
        } else if let Some(id) = &target {
            self.apply_external_viewport(&mut state, decoded, id, intent.clone()).await?;
        } else {
            state.viewport.pending = intent.viewport.as_ref().map(|_| intent.clone());
        }
        self.persist_viewport(&state, decoded)?;
        Ok(json!({"result":decoded.call.tool_id,"worker_environment_id":decoded.formation.worker_environment_id,"browser_context_id":state.browser_context_id,"target_id":target,"intent":intent,"viewport":state.viewport}))
    }

    fn persist_viewport(&self, state: &RoomState, decoded: &DecodedRoomInvocation) -> Result<(), String> {
        let status = presentation_status(state);
        self.persist_live_state(&decoded.formation, &self.live_state(&decoded.formation, Some(state), status, None))
    }

    pub(super) async fn take_external_viewport(&self, state: &mut RoomState, decoded: &DecodedRoomInvocation) -> Result<Option<ViewportIntent>, String> {
        if matches!(self.transport, BrowserTransport::Native(_)) { return Ok(None); }
        let pending = state.viewport.pending.take();
        if pending.is_some() { self.persist_viewport(state, decoded)?; }
        Ok(pending)
    }

    async fn apply_external_viewport(&self, state: &mut RoomState, decoded: &DecodedRoomInvocation, target_id: &str, intent: ViewportIntent) -> Result<(), String> {
        let id = target_id.to_owned();
        self.require_context_target(state, &id).await?;
        if state.viewport.pages.get(&id).and_then(|p| p.model_viewport.as_ref()).is_some_and(|m| m.emulated_size == intent.viewport) { return Ok(()); }
        state.viewport.pages.insert(id.clone(), ViewportPage { model_viewport: None, application: Some(ViewportApplication { intent: intent.clone(), status: ApplicationStatus::Applying, debugger_epoch: 0 }), debugger_epoch: 0 });
        self.persist_viewport(state, decoded)?;
        let session = state.pages.get_mut(&id).and_then(|page| page.session.as_mut()).ok_or("viewport has no page control connection")?;
        let (method, params) = match &intent.viewport {
            Some(size) => ("Emulation.setDeviceMetricsOverride", json!({"width":size.width,"height":size.height,"deviceScaleFactor":1,"mobile":false})),
            None => ("Emulation.clearDeviceMetricsOverride", json!({})),
        };
        let result = session.command(&decoded.call.tool_id, method, params).await;
        let result = match result { Ok(_) => self.require_context_target(state, &id).await, Err(error) => Err(error) };
        let page = state.viewport.pages.get_mut(&id).ok_or("viewport application lost its target")?;
        let application = page.application.as_mut().ok_or("viewport application disappeared")?;
        if let Err(error) = result { application.status = ApplicationStatus::Uncertain; self.persist_viewport(state, decoded)?; return Err(error); }
        application.status = ApplicationStatus::Applied;
        page.model_viewport = Some(ModelViewport { revision: intent.revision, emulated_size: intent.viewport });
        self.persist_viewport(state, decoded)
    }

    pub(super) async fn connect_viewport(&self, state: &mut RoomState, decoded: &DecodedRoomInvocation, target_id: &str, created: bool, pending: Option<ViewportIntent>) -> Result<(), String> {
        if matches!(self.transport, BrowserTransport::Native(_)) { self.read_native_viewport(state, decoded, Some(target_id)).await?; }
        else {
            let id = target_id.to_owned();
            if created { state.viewport.pages.insert(id, ViewportPage { model_viewport: Some(ModelViewport { revision: 0, emulated_size: None }), application: None, debugger_epoch: 0 }); }
            else { state.viewport.disconnected(&id); }
            if let Some(intent) = pending { self.apply_external_viewport(state, decoded, target_id, intent).await?; }
        }
        Ok(())
    }

    async fn read_native_viewport(&self, state: &mut RoomState, decoded: &DecodedRoomInvocation, target: Option<&str>) -> Result<(), String> {
        let result = if let Some(id) = target { state.pages.get_mut(id).and_then(|page| page.session.as_mut()).ok_or("viewport has no page control")?.command(&decoded.call.tool_id,"OctopusBrowser.getViewportState",json!({})).await? }
            else { self.browser_command(state,"OctopusBrowser.getViewportState",json!({})).await? };
        self.accept_native_viewport(state, decoded, result, None, target).await
    }

    async fn accept_native_viewport(&self, state: &mut RoomState, decoded: &DecodedRoomInvocation, result: Value, expected: Option<&ViewportIntent>, target: Option<&str>) -> Result<(), String> {
        let fields = ["schema_version", "worker_environment_id", "formation_pin", "host_generation", "engine_handle", "browser_context_id", "target_id", "last_intent", "pending", "model_viewport", "application", "debugger_epoch"];
        if strict_object("native viewport receipt", &result, &fields)?.len() != fields.len() { return Err("native viewport receipt is missing required fields".into()); }
        let receipt: NativeViewportState = serde_json::from_value(result).map_err(|error| format!("native viewport receipt is malformed: {error}"))?;
        let identity = state.native_identity.as_ref().ok_or("viewport has no native room identity")?;
        let page = target.is_some();
        if receipt.schema_version != 1 || receipt.worker_environment_id != decoded.formation.worker_environment_id
            || receipt.formation_pin != decoded.formation.formation_pin || receipt.host_generation != identity.host_generation
            || receipt.engine_handle != identity.engine_handle || Some(&receipt.browser_context_id) != state.browser_context_id.as_ref() || receipt.target_id.as_deref() != target {
            return Err("native viewport receipt changed its exact room/host/target provenance".into());
        }
        for intent in [&receipt.pending, &receipt.last_intent].into_iter().flatten() { intent.validate(&decoded.formation)?; if intent.revision > decoded.source_movement_seq { return Err("viewport receipt references a future source movement".into()); } }
        if let Some(expected) = expected {
            if receipt.last_intent.as_ref() != Some(expected)
                || (page && receipt.model_viewport.as_ref().is_none_or(|model| model.emulated_size != expected.viewport || model.revision > expected.revision))
                || (!page && receipt.pending.as_ref() != expected.viewport.as_ref().map(|_| expected)) {
                return Err("native viewport result did not confirm the admitted pending/applied intent".into());
            }
        }
        if page && receipt.pending.is_some() { return Err("native page receipt retained an unconsumed pending viewport".into()); }
        if let Some(pending) = &receipt.pending { if pending.viewport.is_none() || receipt.last_intent.as_ref() != Some(pending) { return Err("native pending viewport disagrees with its intent".into()); } }
        if let Some(id) = target.map(str::to_owned) {
            self.require_context_target(state, &id).await?;
            let epoch = receipt.debugger_epoch.filter(|v| *v <= MAX_REVISION).ok_or("native viewport has no debugger epoch")?;
            if let Some(application) = &receipt.application {
                application.intent.validate(&decoded.formation)?;
                if application.intent.revision > decoded.source_movement_seq || application.debugger_epoch > epoch { return Err("native viewport application has invalid epoch/provenance".into()); }
            }
            if let Some(model) = &receipt.model_viewport {
                if let Some(size) = &model.emulated_size { size.validate()?; }
                if model.revision == 0 {
                    if model.emulated_size.is_some() || receipt.application.is_some() { return Err("native viewport fabricated initial metrics".into()); }
                } else {
                    let application = receipt.application.as_ref().ok_or("applied viewport has no application evidence")?;
                    if application.status != ApplicationStatus::Applied || application.debugger_epoch != epoch || application.intent.revision != model.revision || application.intent.viewport != model.emulated_size {
                        return Err("native applied viewport disagrees with its application evidence".into());
                    }
                }
            }
            state.viewport.pages.insert(id, ViewportPage { model_viewport: receipt.model_viewport, application: receipt.application, debugger_epoch: epoch });
        } else if receipt.model_viewport.is_some() || receipt.application.is_some() || receipt.debugger_epoch.is_some() { return Err("empty-context viewport receipt claims page metrics".into()); }
        state.viewport.pending = receipt.pending; state.viewport.last_intent = receipt.last_intent;
        Ok(())
    }
}
