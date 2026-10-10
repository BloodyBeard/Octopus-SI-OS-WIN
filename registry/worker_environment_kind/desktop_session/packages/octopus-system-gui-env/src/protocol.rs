use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const VERSION: u32 = 2;

pub mod permission {
    use serde::{Deserialize, Deserializer, Serialize};
    use std::collections::BTreeSet;
    use std::path::Path;

    pub const MAX_BYTES: usize = 128 * 1024;

    fn required_option<'de, D: Deserializer<'de>, T: Deserialize<'de>>(value: D) -> Result<Option<T>, D::Error> {
        Option::deserialize(value)
    }

    #[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum Operation { Status, Request, OpenSettings }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Action {
        Permissions {
            operation: Operation,
            #[serde(deserialize_with = "required_option")]
            permission: Option<String>,
        },
    }

    pub fn validate_request_id(value: &str) -> Result<(), String> {
        if value.len() != 36 || !value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) { byte == b'-' }
            else { byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) }
        }) {
            return Err("native permission request requires a fresh canonical UUID".into());
        }
        Ok(())
    }

    fn text(value: &str) -> bool { !value.is_empty() && value.len() <= 4096 && !value.contains('\0') }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Bundle { pub identifier: String, pub path: String }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Helper {
        pub executable_path: String,
        pub display_name: String,
        pub process_id: u32,
        #[serde(deserialize_with = "required_option")]
        pub bundle: Option<Bundle>,
        #[serde(deserialize_with = "required_option")]
        pub responsible_app: Option<Bundle>,
    }

    #[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
    #[serde(rename_all = "snake_case")]
    pub enum Kind { ScreenCapture, Accessibility }

    #[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum Status { Granted, NotGranted }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Provenance { pub process_id: u32, pub api: String }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Requirement {
        pub id: String,
        pub kind: Kind,
        pub title: String,
        pub status: Status,
        pub can_request: bool,
        pub can_open_settings: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub detail: Option<String>,
        pub provenance: Provenance,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct NativeSnapshot { pub helper: Helper, pub permissions: Vec<Requirement> }

    impl NativeSnapshot {
        pub fn validate(&self, process_id: u32) -> Result<(), String> {
            let helper = &self.helper;
            if process_id == 0 || process_id > i32::MAX as u32 || helper.process_id != process_id
                || !text(&helper.executable_path) || !Path::new(&helper.executable_path).is_absolute()
                || !text(&helper.display_name) || self.permissions.len() != 2 {
                return Err("native permission snapshot has invalid helper identity or requirements".into());
            }
            for bundle in [&helper.bundle, &helper.responsible_app].into_iter().flatten() {
                if !text(&bundle.identifier) || !text(&bundle.path) || !Path::new(&bundle.path).is_absolute() {
                    return Err("native permission bundle observation is invalid".into());
                }
            }
            let mut ids = BTreeSet::new();
            let mut kinds = BTreeSet::new();
            for row in &self.permissions {
                let api = match row.kind { Kind::ScreenCapture => "CGPreflightScreenCaptureAccess", Kind::Accessibility => "AXIsProcessTrusted" };
                if !text(&row.id) || !ids.insert(&row.id) || !kinds.insert(row.kind) || !text(&row.title)
                    || row.can_request || row.detail.as_deref().is_some_and(|value| !text(value))
                    || row.provenance.process_id != process_id || row.provenance.api != api {
                    return Err("native permission requirement lacks exact boolean API provenance or Settings-only semantics".into());
                }
            }
            Ok(())
        }

        pub fn same_identity(&self, previous: &Self) -> bool {
            self.helper == previous.helper && self.permissions.len() == previous.permissions.len()
                && self.permissions.iter().all(|row| previous.permissions.iter().any(|old|
                    row.id == old.id && row.kind == old.kind && row.provenance == old.provenance))
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
    pub enum SettingsAction { PermissionSettingsSource { permission_request_id: String } }

    #[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum SettingsState { Waiting, Observed, Ended }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SettingsIdentity {
        pub process_id: u32,
        pub executable_path: String,
        pub start_seconds: String,
        pub start_microseconds: String,
        pub display_name: String,
        #[serde(deserialize_with = "required_option")]
        pub bundle: Option<Bundle>,
        #[serde(deserialize_with = "required_option")]
        pub responsible_app: Option<Bundle>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SettingsBounds { pub x: f64, pub y: f64, pub width: f64, pub height: f64 }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SettingsWindow {
        pub id: u32,
        pub observation_sequence: u64,
        pub bounds: SettingsBounds,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SettingsSource {
        pub schema_version: u32,
        pub host_generation: String,
        pub permission_request_id: String,
        #[serde(deserialize_with = "required_option")]
        pub permission: Option<String>,
        pub state: SettingsState,
        #[serde(deserialize_with = "required_option")]
        pub settings: Option<SettingsIdentity>,
        #[serde(deserialize_with = "required_option")]
        pub window: Option<SettingsWindow>,
    }
    impl SettingsSource {
        pub fn absent(generation: &str, request_id: &str) -> Self {
            Self { schema_version: 1, host_generation: generation.into(), permission_request_id: request_id.into(),
                permission: None, state: SettingsState::Ended, settings: None, window: None }
        }
        pub fn validate(&self, generation: &str, request_id: &str) -> Result<(), String> {
            validate_request_id(&self.permission_request_id)?;
            if self.schema_version != 1 || self.host_generation != generation || self.permission_request_id != request_id
                || generation.len() != 32 || !generation.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || self.permission.as_deref().is_some_and(|value| !text(value)) {
                return Err("native Settings source has invalid request or host identity".into());
            }
            if self.state != SettingsState::Observed {
                if self.settings.is_some() || self.window.is_some() || (self.state == SettingsState::Waiting && self.permission.is_none()) {
                    return Err("native Settings source published an unavailable identity".into());
                }
                return Ok(());
            }
            let settings = self.settings.as_ref().ok_or("observed Settings source has no native application")?;
            let window = self.window.as_ref().ok_or("observed Settings source has no native window")?;
            let decimal = |value: &str| value.parse::<u64>().ok().filter(|parsed| parsed.to_string() == value);
            if self.permission.is_none() || settings.process_id == 0 || settings.process_id > i32::MAX as u32
                || !text(&settings.executable_path) || !Path::new(&settings.executable_path).is_absolute()
                || !text(&settings.display_name) || decimal(&settings.start_seconds).is_none_or(|value| value == 0)
                || decimal(&settings.start_microseconds).is_none_or(|value| value >= 1_000_000)
                || window.id == 0 || window.observation_sequence == 0 || window.observation_sequence > 9_007_199_254_740_991
                || ![window.bounds.x, window.bounds.y, window.bounds.width, window.bounds.height].iter().all(|v| v.is_finite())
                || window.bounds.width <= 0.0 || window.bounds.height <= 0.0 {
                return Err("native Settings source lacks exact process birth or window geometry".into());
            }
            for bundle in [&settings.bundle, &settings.responsible_app].into_iter().flatten() {
                if !text(&bundle.identifier) || !text(&bundle.path) || !Path::new(&bundle.path).is_absolute() {
                    return Err("native Settings bundle observation is invalid".into());
                }
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Snapshot {
        pub schema_version: u32,
        pub host_generation: String,
        pub helper: Helper,
        pub permissions: Vec<Requirement>,
    }
}

pub mod presentation {
    use super::application;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Scope {
        pub host_generation: String,
        pub worker_environment_id: String,
        pub formation_pin: String,
        pub app_instance_id: String,
        pub grant_request_id: String,
    }
    impl Scope {
        pub fn room(&self) -> application::Scope {
            application::Scope { host_generation: self.host_generation.clone(),
                worker_environment_id: self.worker_environment_id.clone(), formation_pin: self.formation_pin.clone() }
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Action {
        PresentationComplete(Completion),
        ReceiverBootstrap {
            scope: Scope,
            source_window_id: String,
            receiver_pid: i32,
            receiver_nonce: String,
            receiver_instance_nonce: String,
        },
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Completion {
        pub scope: Scope,
        pub source_window_id: String,
        pub presentation_id: String,
        pub stream_generation: u64,
        pub receiver_instance_nonce: String,
    }
    impl Completion {
        pub fn validate(&self, room: &str) -> Result<(), String> {
            if self.scope.worker_environment_id != room || self.stream_generation == 0
                || self.stream_generation > 9_007_199_254_740_991
                || self.receiver_instance_nonce.len() != 64
                || !self.receiver_instance_nonce.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || [&self.scope.host_generation, &self.scope.worker_environment_id, &self.scope.formation_pin,
                    &self.scope.app_instance_id, &self.scope.grant_request_id, &self.source_window_id, &self.presentation_id]
                    .iter().any(|value| value.is_empty() || value.len() > 512 || value.contains('\0')) {
                return Err("native presentation completion identity is invalid".into());
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum CompletionStatus { Ended, Absent }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CompletionResult {
        pub schema_version: u32,
        pub status: CompletionStatus,
        pub target: Completion,
    }
    impl CompletionResult {
        pub fn validate_for(&self, target: &Completion) -> Result<(), String> {
            self.target.validate(&target.scope.worker_environment_id)?;
            if self.schema_version != 1 || &self.target != target {
                return Err("native presentation completion acknowledgement changed its exact target".into());
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PreparedSource {
        pub schema_version: u32,
        pub producer_pid: i32,
        pub presentation_id: String,
        pub stream_generation: u64,
        pub native_window_incarnation: String,
        pub native_cg_window_id: u32,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Binding {
        pub schema_version: u32,
        pub host_generation: String,
        pub bootstrap_id: String,
        pub request_id: String,
        pub receiver_nonce: String,
        pub receiver_instance_nonce: String,
        pub scope: Scope,
        pub source: application::Source,
        pub presentation_id: String,
        pub stream_generation: u64,
        pub source_window_id: String,
        pub native_window_incarnation: String,
        pub native_cg_window_id: u32,
        pub receiver_pid: i32,
        pub producer_pid: i32,
        pub expires_at_mach_continuous_ticks: String,
    }

    #[derive(Clone, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Bootstrap {
        pub schema_version: u32,
        pub bootstrap_id: String,
        pub bootstrap_key_base64: String,
        pub binding_bytes_base64: String,
    }
    impl std::fmt::Debug for Bootstrap {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Bootstrap").field("schema_version", &self.schema_version)
                .field("bootstrap_id", &self.bootstrap_id).finish_non_exhaustive()
        }
    }
}

pub const MAX_MEDIA_ITEMS: usize = 16;
pub const MAX_SOURCE_MEDIA_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_NATIVE_TEXT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_NATIVE_RESULT_BYTES: usize =
    MAX_SOURCE_MEDIA_BYTES.div_ceil(3) * 4 + MAX_MEDIA_ITEMS * 256 + MAX_NATIVE_TEXT_BYTES;

pub mod application {
    use std::collections::BTreeSet;

    use serde::{Deserialize, Serialize};

    pub const VERSION: u32 = 3;
    const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Scope {
        pub host_generation: String,
        pub worker_environment_id: String,
        pub formation_pin: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct InputLease {
        pub scope: Scope,
        pub control_generation: u64,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct AppRef {
        pub host_generation: String,
        pub app_instance_id: String,
    }

    impl AppRef {
        pub fn validate(&self, scope: &Scope) -> Result<(), String> {
            for value in [&scope.host_generation, &scope.worker_environment_id, &scope.formation_pin, &self.app_instance_id] {
                identity(value)?;
            }
            if self.host_generation != scope.host_generation {
                return Err("application reference belongs to another native host generation".into());
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(tag = "kind", content = "value", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Selector {
        BundleId(String),
        BundlePath(String),
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct AppInfo {
        pub app: AppRef,
        pub bundle_id: String,
        pub bundle_path: String,
        pub display_name: String,
        pub process_id: i32,
        pub launch_time_unix_ms: Option<u64>,
        pub active: bool,
        pub hidden: bool,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Resolution {
        pub candidates: Vec<AppInfo>,
        pub selected: Option<AppRef>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    pub struct Rect {
        pub x: f64,
        pub y: f64,
        pub width: f64,
        pub height: f64,
    }

    impl Rect {
        fn validate(&self) -> Result<(), String> {
            if ![self.x, self.y, self.width, self.height].iter().all(|v| v.is_finite())
                || self.width < 0.0 || self.height < 0.0 {
                return Err("application rectangle has an invalid point coordinate domain".into());
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct AxAction {
        pub action_id: String,
        pub name: String,
        pub description: Option<String>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct PresentationBase {
        pub app: AppRef,
        pub observation_id: String,
        pub revision: u64,
    }

    impl PresentationBase {
        pub fn validate(&self) -> Result<(), String> {
            identity(&self.app.host_generation)?;
            identity(&self.app.app_instance_id)?;
            identity(&self.observation_id)?;
            if self.revision == 0 || self.revision > MAX_SEQUENCE { return Err("presentation base revision is outside its exact domain".into()); }
            Ok(())
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct AxTextPresentation {
        pub source: String,
        pub format: String,
        pub text: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct AxPresentation {
        pub schema_version: u32,
        pub format: String,
        pub context_type: String,
        pub mode: String,
        pub base_revision: Option<u64>,
        pub text: String,
        pub rendered_element_ids: Vec<String>,
        pub focused_element_id: Option<String>,
        pub selected_text: Option<String>,
        pub selected_element_ids: Vec<String>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    pub struct AxNode {
        pub text_presentation: Option<AxTextPresentation>,
        pub element_id: String,
        pub parent_id: Option<String>,
        pub window_id: Option<String>,
        pub window_unavailable_reason: Option<String>,
        pub role: String,
        pub subrole: Option<String>,
        pub title: Option<String>,
        pub description: Option<String>,
        pub value: Option<String>,
        pub value_type: Option<String>,
        pub value_input: Option<AxValueInput>,
        pub enabled: Option<bool>,
        pub focused: Option<bool>,
        pub frame_screen_points: Option<Rect>,
        pub settable_attributes: Vec<String>,
        pub actions: Vec<AxAction>,
        pub unavailable_attributes: Vec<String>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum AxValueKind { String, Float, Integer, Boolean, Date }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct AxValueInput {
        pub kind: AxValueKind,
        pub format: Option<String>,
        pub date_components_mask: Option<u64>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct AxOmission {
        pub parent_id: Option<String>,
        pub reason: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum ObservationMode { Full, Diff }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct AxObservation {
        pub presentation: AxPresentation,
        pub context: AxObservationContext,
        pub observation_id: String,
        pub app: AppRef,
        pub revision: u64,
        pub captured_at_unix_ms: u64,
        pub mode: ObservationMode,
        pub base_revision: Option<u64>,
        pub nodes: Vec<AxNode>,
        pub removed_ids: Vec<String>,
        pub complete: bool,
        pub omissions: Vec<AxOmission>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct AxObservationContext {
        pub root_kind: String,
        pub root_element_id: String,
        pub menu_bar_element_id: Option<String>,
        pub display_omissions: Vec<AxDisplayOmission>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct AxDisplayOmission {
        pub element_id: String,
        pub reason: String,
    }

    impl AxObservation {
        pub fn validate(&self, scope: &Scope, app: &AppRef) -> Result<(), String> {
            self.app.validate(scope)?;
            identity(&self.observation_id)?;
            if &self.app != app || self.revision == 0 || self.revision > MAX_SEQUENCE
                || self.nodes.len() > 4096 || self.complete != self.omissions.is_empty() {
                return Err("accessibility observation identity, revision or completeness is invalid".into());
            }
            match (&self.mode, self.base_revision) {
                (ObservationMode::Full, None) if self.removed_ids.is_empty() => {},
                (ObservationMode::Diff, Some(base)) if self.complete && base > 0 && base < self.revision => {},
                _ => return Err("accessibility observation does not identify an exact full or diff revision".into()),
            }
            identity(&self.context.root_element_id)?;
            let root_role = match self.context.root_kind.as_str() {
                "application" => "AXApplication", "menu_bar_item" => "AXMenuBarItem", "menu" => "AXMenu",
                _ => return Err("accessibility observation has an unknown native root context".into()),
            };
            if let Some(id) = &self.context.menu_bar_element_id { identity(id)?; }
            if self.context.display_omissions.len() > 12288 { return Err("accessibility display omissions exceed the native node bound".into()); }
            let mut display = BTreeSet::new();
            for omission in &self.context.display_omissions {
                identity(&omission.element_id)?;
                if !["first_menu_item", "excluded_menu_title", "immediate_menu_actions", "immediate_menu_descendants"].contains(&omission.reason.as_str())
                    || !display.insert((&omission.element_id, &omission.reason)) {
                    return Err("accessibility display omission is unknown or repeated".into());
                }
            }
            let mut elements = BTreeSet::new();
            let mut actions = BTreeSet::new();
            for node in &self.nodes {
                identity(&node.element_id)?;
                identity(&node.role)?;
                match (&node.window_id, &node.window_unavailable_reason) {
                    (Some(id), None) => identity(id)?,
                    (None, Some(reason)) => identity(reason)?,
                    _ => return Err("accessibility node must identify its native window or explain its unavailability".into()),
                }
                if !elements.insert(node.element_id.as_str()) || node.actions.len() > 128 {
                    return Err("accessibility node identity is duplicated or its action bound was exceeded".into());
                }
                for text in [&node.value, &node.title, &node.description, &node.subrole].into_iter().flatten() {
                    if text.encode_utf16().count() > 65_536 { return Err("accessibility text exceeds its native bound".into()); }
                }
                if let Some(text) = &node.text_presentation {
                    if !["ax_value", "ax_attributed_range", "ax_text_marker_range"].contains(&text.source.as_str())
                        || !["plain", "attributed_markdown"].contains(&text.format.as_str()) || text.text.len() > 4 * 1024 * 1024
                        || (text.source == "ax_value" && text.format != "plain") {
                        return Err("accessibility text presentation has an invalid native source or format".into());
                    }
                }
                if let Some(input) = &node.value_input {
                    if !node.settable_attributes.iter().any(|name| name == "AXValue") { return Err("native value conversion requires an observed writable AXValue".into()); }
                    let valid = match input.kind {
                        AxValueKind::String => (node.value_type.as_deref() == Some("string") || (node.value_type.is_none() && matches!(node.role.as_str(), "AXTextField" | "AXTextArea"))) && input.format.is_none() && input.date_components_mask.is_none(),
                        AxValueKind::Float | AxValueKind::Integer => node.value_type.as_deref() == Some("number") && input.format.as_deref() == Some("en_US decimal") && input.date_components_mask.is_none(),
                        AxValueKind::Boolean => (node.value_type.as_deref() == Some("boolean") || (node.role == "AXCheckBox" && matches!(node.subrole.as_deref(),Some("AXSwitch" | "AXToggle")))) && input.format.is_none() && input.date_components_mask.is_none(),
                        AxValueKind::Date => node.value_type.as_deref() == Some("date") && input.format.as_ref().is_some_and(|format| !format.is_empty() && format.len() <= 2048) && input.date_components_mask.is_some_and(|mask| mask <= MAX_SEQUENCE),
                    };
                    if !valid { return Err("native value conversion differs from the observed AX value type or format".into()); }
                }
                if let Some(frame) = &node.frame_screen_points { frame.validate()?; }
                for action in &node.actions {
                    identity(&action.action_id)?;
                    identity(&action.name)?;
                    if !actions.insert(action.action_id.as_str()) { return Err("accessibility action identity is duplicated".into()); }
                }
            }
            let presentation = &self.presentation;
            if presentation.schema_version != 1 || presentation.format != "macos_ax"
                || !["full_ui", "event_stream", "informational"].contains(&presentation.context_type.as_str())
                || presentation.text.len() > 4 * 1024 * 1024 || self.mode != ObservationMode::Full {
                return Err("native presentation requires a bounded full raw observation and its declared format".into());
            }
            match (presentation.mode.as_str(), presentation.base_revision) {
                ("full", None) => {},
                ("diff" | "no_change", Some(base)) if self.complete && base > 0 && base < self.revision => {},
                _ => return Err("native presentation has no exact complete compression base".into()),
            }
            let mut rendered = BTreeSet::new();
            for id in &presentation.rendered_element_ids {
                if !elements.contains(id.as_str()) || !rendered.insert(id.as_str()) { return Err("native presentation contains a foreign or repeated current element".into()); }
            }
            let mut selected = BTreeSet::new();
            for id in &presentation.selected_element_ids {
                if !elements.contains(id.as_str()) || !selected.insert(id.as_str()) { return Err("native selection contains a foreign or repeated element".into()); }
            }
            if presentation.focused_element_id.as_ref().is_some_and(|id| !elements.contains(id.as_str()))
                || presentation.selected_text.as_ref().is_some_and(|text| text.encode_utf16().count() > 65_536) {
                return Err("native focused or selected content exceeds its retained observation".into());
            }
            let mut removed = BTreeSet::new();
            for id in &self.removed_ids {
                identity(id)?;
                if !removed.insert(id) || elements.contains(id.as_str()) { return Err("accessibility diff removes duplicate or included node identities".into()); }
            }
            if self.mode == ObservationMode::Full {
                if !self.nodes.iter().any(|node| node.element_id == self.context.root_element_id && node.role == root_role)
                    || self.context.menu_bar_element_id.as_ref().is_some_and(|id| !self.nodes.iter().any(|node| &node.element_id == id && node.role == "AXMenuBar")) {
                    return Err("accessibility context does not identify its actual observed native roots".into());
                }
                for node in &self.nodes {
                    if node.parent_id.as_deref().is_some_and(|id| !elements.contains(id)) {
                        return Err("full accessibility tree has an unresolved parent".into());
                    }
                }
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct AxGuard {
        pub app: AppRef,
        pub observation_id: String,
        pub revision: u64,
        pub element_id: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct AxEffect {
        pub dispatch_state: String,
        pub mechanism: String,
        pub before_revision: u64,
        pub native_error_code: i32,
        pub readback_matches: Option<bool>,
        pub readback_error: Option<String>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct ActionTarget {
        pub grant_request_id: String,
        pub app: AppRef,
        pub observation_id: String,
        pub revision: u64,
        pub control_generation: u64,
        pub window_id: Option<String>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
    #[serde(tag = "type", content = "params", rename_all = "snake_case", deny_unknown_fields)]
    pub enum ActionParameters {
        AppClick { element_id: Option<String>, image_id: Option<String>, x: Option<f64>, y: Option<f64>, button: String, count: u8 },
        AppMove { image_id: String, x: f64, y: f64 },
        AppDrag { image_id: String, from_x: f64, from_y: f64, to_x: f64, to_y: f64 },
        AppScroll { element_id: Option<String>, image_id: Option<String>, x: Option<f64>, y: Option<f64>,
            direction: Option<String>, pages: Option<f64>, delta_x: Option<i32>, delta_y: Option<i32> },
        AppKey {
            #[serde(default, deserialize_with = "present_option", skip_serializing_if = "Option::is_none")]
            key: Option<String>,
            #[serde(default, deserialize_with = "present_option", skip_serializing_if = "Option::is_none")]
            key_code: Option<u16>,
            #[serde(default, deserialize_with = "present_option", skip_serializing_if = "Option::is_none")]
            modifiers: Option<Vec<String>>,
        },
        AppType { text: String },
        AppSetValue { element_id: String, value: String },
        AppSelectText { element_id: String, text: String, prefix: Option<String>, suffix: Option<String>, selection_type: String },
        AppSecondaryAction { element_id: String, action_id: String },
        AppPaste { text: String, format: String },
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    pub struct ActionCall {
        pub target: ActionTarget,
        pub operation: ActionParameters,
    }

    impl ActionCall {
        pub fn parse(operation: &str, arguments: &serde_json::Value) -> Result<Self, String> {
            let mut params = arguments.as_object().cloned().ok_or("application action arguments must be an object")?;
            let mut target = serde_json::Map::new();
            for key in ["grant_request_id", "app", "observation_id", "revision", "control_generation", "window_id"] {
                target.insert(key.into(), params.remove(key).ok_or_else(|| format!("application action is missing {key}"))?);
            }
            let call: Self = serde_json::from_value(serde_json::json!({"target":target,"operation":{"type":operation,"params":params}}))
                .map_err(|error| format!("application action schema: {error}"))?;
            call.validate()?;
            Ok(call)
        }

        pub fn validate(&self) -> Result<(), String> {
            let target = &self.target;
            for value in [&target.app.host_generation, &target.app.app_instance_id, &target.observation_id] { identity(value)?; }
            if let Some(window) = &target.window_id { identity(window)?; }
            else if !matches!(self.operation, ActionParameters::AppKey { .. } | ActionParameters::AppType { .. } | ActionParameters::AppPaste { .. } | ActionParameters::AppClick { element_id: Some(_), .. } | ActionParameters::AppSetValue { .. } | ActionParameters::AppSelectText { .. } | ActionParameters::AppSecondaryAction { .. }) {
                return Err("coordinate input requires an exact observed application window".into());
            }
            crate::app_authorization::record_path(std::path::Path::new("/"), &target.grant_request_id, "request")?;
            if target.revision == 0 || target.revision > MAX_SEQUENCE || target.control_generation > MAX_SEQUENCE {
                return Err("application action revision or control generation is outside its exact domain".into());
            }
            let text = |value: &str| if value.encode_utf16().count() <= 65_536 && !value.contains('\0') { Ok(()) } else { Err("application action text exceeds its native bound".to_string()) };
            let point = |image: &str, values: &[f64]| -> Result<(), String> {
                identity(image)?;
                if values.iter().any(|v| !v.is_finite() || *v < 0.0) { return Err("application image coordinates must be finite and nonnegative".into()); }
                Ok(())
            };
            match &self.operation {
                ActionParameters::AppClick { element_id, image_id, x, y, button, count } => {
                    match (element_id,image_id,x,y) {
                        (Some(element),None,None,None) => identity(element)?,
                        (None,Some(image),Some(x),Some(y)) => point(image, &[*x,*y])?,
                        _ => return Err("application click requires exactly one element or original-image point".into()),
                    }
                    if !["left","right","middle"].contains(&button.as_str()) || !(1..=3).contains(count) { return Err("application click has an unsupported button or count".into()); }
                },
                ActionParameters::AppMove { image_id, x, y } => point(image_id, &[*x,*y])?,
                ActionParameters::AppDrag { image_id, from_x, from_y, to_x, to_y } => point(image_id, &[*from_x,*from_y,*to_x,*to_y])?,
                ActionParameters::AppScroll { element_id, image_id, x, y, direction, pages, delta_x, delta_y } => {
                    match (element_id,image_id,x,y) {
                        (Some(element),None,None,None) => identity(element)?,
                        (None,Some(image),Some(x),Some(y)) => point(image, &[*x,*y])?,
                        _ => return Err("application scroll requires exactly one native element or original-image point".into()),
                    }
                    match (direction,pages,delta_x,delta_y) {
                        (Some(direction),Some(pages),None,None) if ["up","down","left","right"].contains(&direction.as_str()) && pages.is_finite() && *pages > 0.0 => {},
                        (None,None,Some(x),Some(y)) if element_id.is_none() && x.unsigned_abs() <= 10000 && y.unsigned_abs() <= 10000 && (*x != 0 || *y != 0) => {},
                        _ => return Err("application scroll requires positive finite directional pages, or explicit bounded point-pixel deltas".into()),
                    }
                },
                ActionParameters::AppKey { key, key_code, modifiers } => {
                    match (key, key_code, modifiers) {
                        (Some(key), None, None) => {
                            identity(key)?;
                            if key.len() > 256 { return Err("named application key exceeds its native parser bound".into()); }
                        },
                        (None, Some(_), Some(modifiers)) => {
                            let unique: BTreeSet<_> = modifiers.iter().collect();
                            if unique.len() != modifiers.len() || modifiers.iter().any(|m| !["command","control","option","shift"].contains(&m.as_str())) { return Err("application key modifiers are unsupported or duplicated".into()); }
                        },
                        _ => return Err("application key requires exactly one named key or native code and modifier array".into()),
                    }
                },
                ActionParameters::AppType { text: value } => {
                    if value.is_empty() { return Err("application typing requires nonempty text".into()); }
                    text(value)?;
                },
                ActionParameters::AppSetValue { element_id, value } => { identity(element_id)?; text(value)?; },
                ActionParameters::AppSelectText { element_id, text: value, prefix, suffix, selection_type } => {
                    identity(element_id)?; text(value)?;
                    for value in [prefix,suffix].into_iter().flatten() { text(value)?; }
                    if value.is_empty() || !["text","cursor_before","cursor_after"].contains(&selection_type.as_str()) { return Err("application selection requires exact text and a supported selection type".into()); }
                },
                ActionParameters::AppSecondaryAction { element_id, action_id } => { identity(element_id)?; identity(action_id)?; },
                ActionParameters::AppPaste { text: value, format } => {
                    if value.len() > 1_048_576 || value.contains('\0') || !["text","md","html"].contains(&format.as_str()) { return Err("application paste format or text bound is unsupported".into()); }
                },
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ActionError {
        pub code: String,
        pub message: String,
        pub dispatch_attempted: bool,
        pub dispatched_events: u32,
        pub effect_uncertain: bool,
        pub native_error_code: Option<i32>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct InputDelivery {
        pub mechanism: String,
        pub dispatched_events: u32,
        pub application_effect_verified: bool,
    }

    impl InputDelivery {
        fn validate(&self, maximum_events: u32) -> Result<(), String> {
            if self.mechanism != "CGEventPostToPid" || self.dispatched_events == 0
                || self.dispatched_events > maximum_events || self.application_effect_verified {
                return Err("native event delivery does not prove bounded app-targeted dispatch".into());
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct FocusEffect {
        pub mechanism: String,
        pub target_pid: i32,
        pub event_type: Option<u32>,
        pub subtype: Option<u16>,
        pub dispatched_events: u32,
        pub native_status: Option<i32>,
        pub application_effect_verified: bool,
        pub original_target_pid: Option<i32>,
        pub focus_id: Option<u32>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum CursorMilestoneKind { CloseEnough, Finished }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum CursorCallbackProvenance { DisplayLink, SynchronousCompletion }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CursorMilestone {
        pub app: AppRef,
        pub window_id: String,
        pub window_generation: String,
        pub action_id: String,
        pub movement_id: String,
        pub milestone: CursorMilestoneKind,
        pub callback_provenance: CursorCallbackProvenance,
        pub callback_timestamp: f64,
        pub x: f64,
        pub y: f64,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PasteEvidence {
        pub delivery: InputDelivery,
        pub clipboard_read_acknowledged: bool,
        pub clipboard_restored: bool,
        pub application_effect_verified: bool,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ClipboardFailure {
        pub clipboard_read_acknowledged: bool,
        pub clipboard_restored: bool,
        pub restoration_error: Option<String>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct AxPreparationEffect {
        pub source_element_id: String,
        pub affected_element_id: Option<String>,
        pub mechanism: String,
        pub name: String,
        pub value: Option<f64>,
        pub dispatch_state: String,
        pub native_error_code: Option<i32>,
        pub dispatched_events: Option<u32>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ActionResult {
        pub menu_effects: Vec<AxMenuEffect>,
        pub scope: Scope,
        pub call: ActionCall,
        pub completed: bool,
        pub ax_effect: Option<AxEffect>,
        pub input_delivery: Option<InputDelivery>,
        pub focus_effects: Vec<FocusEffect>,
        pub cursor_milestones: Vec<CursorMilestone>,
        pub preparation_effects: Vec<AxPreparationEffect>,
        pub paste_evidence: Option<PasteEvidence>,
        pub clipboard_failure: Option<ClipboardFailure>,
        pub error: Option<ActionError>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct AxMenuEffect {
        pub reason: String,
        pub transition: String,
        pub source_element_id: String,
        pub menu_bar_item_id: String,
        pub menu_id: Option<String>,
        pub before_revision: u64,
        pub changed: bool,
    }

    impl ActionResult {
        pub fn validate(&self, scope: &Scope, call: &ActionCall) -> Result<(), String> {
            call.validate()?; call.target.app.validate(scope)?;
            if &self.scope != scope || &self.call != call || self.completed != self.error.is_none() { return Err("native application result changed its exact call or completion evidence".into()); }
            let typing_events = match &call.operation {
                ActionParameters::AppType { text } => Some(text.encode_utf16().count() as u32 * 4),
                _ => None,
            };
            if let Some(error) = &self.error {
                identity(&error.code)?;
                if error.message.is_empty() || error.dispatched_events > 8192 + typing_events.unwrap_or(0) || (!error.dispatch_attempted && (error.dispatched_events != 0 || error.effect_uncertain)) { return Err("native application failure has inconsistent dispatch evidence".into()); }
            }
            if let Some(effect) = &self.ax_effect {
                if effect.before_revision != call.target.revision || effect.dispatch_state != "returned" || effect.native_error_code != 0 { return Err("native AX effect does not identify the successful exact dispatch".into()); }
                let expected = match &call.operation {
                    ActionParameters::AppSetValue { .. } => "ax_value",
                    ActionParameters::AppSelectText { .. } => "ax_selection",
                    ActionParameters::AppSecondaryAction { .. } if effect.mechanism == "ax_disclosing" => "ax_disclosing",
                    ActionParameters::AppSecondaryAction { .. } => "ax_action",
                    ActionParameters::AppClick { element_id: Some(_), .. } => "ax_click",
                    _ => return Err("non-AX operation returned AX effect evidence".into()),
                };
                if effect.mechanism != expected { return Err("AX mechanism does not match the exact operation".into()); }
            }
            if self.preparation_effects.len() > 600 { return Err("native scroll preparation exceeds its finite effect bound".into()); }
            if !self.preparation_effects.is_empty() && self.error.as_ref().is_some_and(|error| !error.dispatch_attempted) {
                return Err("native scroll preparation cannot be reported as no attempted effect".into());
            }
            for effect in &self.preparation_effects {
                let element = match &call.operation {
                    ActionParameters::AppClick { element_id: Some(element), .. } | ActionParameters::AppScroll { element_id: Some(element), .. }
                    | ActionParameters::AppSetValue { element_id: element, .. } | ActionParameters::AppSelectText { element_id: element, .. }
                    | ActionParameters::AppSecondaryAction { element_id: element, .. } => element,
                    _ => return Err("native preparation requires an exact retained element action".into()),
                };
                if &effect.source_element_id != element { return Err("scroll preparation belongs to a different element".into()); }
                if let Some(id) = &effect.affected_element_id { identity(id)?; }
                let field = matches!(call.operation, ActionParameters::AppSetValue { .. } | ActionParameters::AppSelectText { .. });
                if effect.mechanism == "CGEventPostToPid" {
                    if !field || effect.name != "field_focus" || effect.affected_element_id.as_ref() != Some(element) || effect.value.is_some() || effect.native_error_code.is_some()
                        || !effect.dispatched_events.is_some_and(|count| count <= 2)
                        || !matches!(effect.dispatch_state.as_str(),"attempted" | "returned")
                        || (effect.dispatch_state == "returned" && effect.dispatched_events != Some(2))
                        || (self.completed && effect.dispatch_state != "returned") {
                        return Err("native field focus does not identify its exact bounded mouse dispatch".into());
                    }
                } else {
                    let permitted = matches!((effect.mechanism.as_str(),effect.name.as_str()), ("ax_action","AXScrollToVisible") | ("ax_attribute","AXScrollToVisible") | ("ax_attribute","AXValue"))
                        || (field && matches!((effect.mechanism.as_str(),effect.name.as_str()),("ax_attribute","AXFocused") | ("ax_action","AXPick" | "AXPress")));
                    if !permitted || effect.dispatched_events.is_some()
                        || (effect.name == "AXValue" && !effect.value.is_some_and(|value| value.is_finite() && (0.0..=1.0).contains(&value)))
                        || (effect.name != "AXValue" && effect.value.is_some()) {
                        return Err("native preparation does not identify a supported exact effect".into());
                    }
                    if !matches!((effect.dispatch_state.as_str(),effect.native_error_code), ("attempted",None) | ("returned",Some(_)))
                        || (self.completed && (effect.dispatch_state != "returned" || effect.native_error_code != Some(0))) {
                        return Err("native preparation outcome is inconsistent with operation completion".into());
                    }
                }
            }
            let preparation_events: u32 = self.preparation_effects.iter().filter_map(|effect| effect.dispatched_events).sum();
            if self.error.as_ref().is_some_and(|error| error.dispatched_events < preparation_events) {
                return Err("native application failure omitted dispatched preparation events".into());
            }
            if self.focus_effects.len() > 4096 || self.cursor_milestones.len() > 4096 {
                return Err("native application event evidence exceeds its bound".into());
            }
            for effect in &self.focus_effects {
                identity(&effect.mechanism)?;
                if effect.target_pid <= 0 || effect.dispatched_events > 4096 || effect.application_effect_verified {
                    return Err("native focus event evidence claims an invalid target or unobserved application effect".into());
                }
            }
            for milestone in &self.cursor_milestones {
                if milestone.app != call.target.app || Some(milestone.window_id.as_str()) != call.target.window_id.as_deref()
                    || ![milestone.x, milestone.y, milestone.callback_timestamp].iter().all(|n| n.is_finite()) {
                    return Err("cursor animation milestone differs from the exact application-window action".into());
                }
                for id in [&milestone.window_generation, &milestone.action_id, &milestone.movement_id] { identity(id)?; }
            }
            if let Some(delivery) = &self.input_delivery {
                delivery.validate(typing_events.unwrap_or(4096))?;
                if self.completed && typing_events.is_some_and(|expected| delivery.dispatched_events != expected) {
                    return Err("completed typing did not dispatch every native keystroke".into());
                }
            }
            if let Some(paste) = &self.paste_evidence {
                paste.delivery.validate(4096)?;
                if !matches!(call.operation, ActionParameters::AppPaste { .. }) || !paste.clipboard_read_acknowledged
                    || paste.application_effect_verified || self.input_delivery.as_ref() != Some(&paste.delivery) {
                    return Err("native paste evidence does not prove the exact dispatched clipboard read".into());
                }
            }
            if self.clipboard_failure.is_some() && (self.completed || !matches!(call.operation, ActionParameters::AppPaste { .. })) {
                return Err("clipboard failure belongs only to a failed native paste".into());
            }
            if self.menu_effects.len() > 2 { return Err("native menu transitions exceed the exact call bound".into()); }
            if !self.menu_effects.is_empty() && self.error.as_ref().is_some_and(|error| !error.dispatch_attempted) {
                return Err("a retained menu transition cannot be reported as no attempted effect".into());
            }
            let source_element = match &call.operation {
                ActionParameters::AppClick { element_id, .. } => element_id.as_deref(),
                ActionParameters::AppSetValue { element_id, .. } | ActionParameters::AppSelectText { element_id, .. }
                    | ActionParameters::AppSecondaryAction { element_id, .. } => Some(element_id.as_str()),
                _ => None,
            };
            let mut requested_menu = false;
            let mut cleanup_menu = false;
            for effect in &self.menu_effects {
                identity(&effect.source_element_id)?; identity(&effect.menu_bar_item_id)?;
                if let Some(id) = &effect.menu_id { identity(id)?; }
                if source_element != Some(effect.source_element_id.as_str()) || effect.before_revision != call.target.revision {
                    return Err("native menu effect changed its exact source or revision".into());
                }
                match (effect.reason.as_str(), effect.transition.as_str()) {
                    ("requested", "select" | "clear") => {
                        let eligible = matches!(&call.operation, ActionParameters::AppSecondaryAction { .. })
                            || matches!(&call.operation, ActionParameters::AppClick { element_id: Some(_), button, count: 1, .. } if button == "left" && effect.transition == "select");
                        if requested_menu || !eligible || self.ax_effect.is_some() || self.input_delivery.is_some()
                            || effect.menu_bar_item_id != effect.source_element_id || (effect.transition == "select" && effect.menu_id.is_none()) {
                            return Err("logical native menu transition has contradictory operation evidence".into());
                        }
                        requested_menu = true;
                    }
                    ("after_native_action", "clear") => {
                        let eligible = matches!(&call.operation, ActionParameters::AppSecondaryAction { .. }
                            | ActionParameters::AppClick { element_id: Some(_), .. });
                        if cleanup_menu || !eligible || (self.ax_effect.is_none() && self.input_delivery.is_none()) {
                            return Err("menu cleanup lacks the exact successful native action".into());
                        }
                        cleanup_menu = true;
                    }
                    _ => return Err("native menu transition has an unsupported reason or transition".into()),
                }
            }
            let is_ax = matches!(call.operation, ActionParameters::AppSetValue { .. } | ActionParameters::AppSelectText { .. } | ActionParameters::AppSecondaryAction { .. });
            let element_click = matches!(call.operation, ActionParameters::AppClick { element_id: Some(_), .. });
            if (is_ax && (self.input_delivery.is_some() || self.paste_evidence.is_some()))
                || (!is_ax && !element_click && self.ax_effect.is_some())
                || (element_click && self.ax_effect.is_some() && self.input_delivery.is_some()) {
                return Err("application action returned evidence for a different native mechanism".into());
            }
            if self.completed && ((is_ax && self.ax_effect.is_none() && !requested_menu)
                || (element_click && self.ax_effect.is_none() && self.input_delivery.is_none() && !requested_menu)
                || (!is_ax && !element_click && self.input_delivery.is_none())
                || (matches!(call.operation, ActionParameters::AppPaste { .. }) && self.paste_evidence.is_none())) {
                return Err("application action completed without its exact native dispatch evidence".into());
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    pub struct CaptureWindow {
        pub window_id: String,
        pub cg_window_id: u32,
        pub app: AppRef,
        pub title: Option<String>,
        pub frame_screen_points: Rect,
        pub on_screen: bool,
        pub window_layer: i64,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CaptureGeometry {
        pub pixel_width: u32,
        pub pixel_height: u32,
        pub pixel_to_screen: [f64; 6],
        pub filter_content_rect: Rect,
        pub point_pixel_scale: f64,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Image {
        pub image_id: String,
        pub media_type: String,
        pub data_base64: String,
        pub sha256: String,
        pub byte_length: usize,
        pub width: u32,
        pub height: u32,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Capture {
        pub source: CaptureSource,
        pub app: AppRef,
        pub window: CaptureWindow,
        pub geometry: CaptureGeometry,
        pub image: Image,
        pub captured_at_unix_ms: u64,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum PrimarySelection { AxFocusedWindow, AxMainWindow, AxWindowsLast }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PrimaryCapture {
        pub observation_id: String,
        pub revision: u64,
        pub source_window_id: String,
        pub source_window_incarnation: String,
        pub selection: PrimarySelection,
        #[serde(deserialize_with = "required_option")]
        pub image_id: Option<String>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum CaptureFilterMode { DesktopIndependentWindow, DisplayIncludingWindows, WindowserverRect }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum CaptureContributor {
        ApplicationWindow { window_id: String, window_incarnation: String },
        OwnedCursor { cg_window_id: u32, host_generation: String },
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CaptureSource {
        pub primary_window_incarnation: String,
        pub contributors: Vec<CaptureContributor>,
        pub crop_screen_points: Rect,
        pub filter_mode: CaptureFilterMode,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CaptureOmission {
        pub window_id: String,
        pub reason: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Instructions {
        pub bundle_id: String,
        pub bundle_path: String,
        pub content_sha256: String,
        pub text: String,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum CaptureSelection { Primary, AllWindows }

    impl CaptureSelection {
        pub fn from_arguments(arguments: &serde_json::Value) -> Result<Self, String> {
            arguments.get("capture_selection").map(|value| serde_json::from_value(value.clone())
                .map_err(|error| format!("invalid application capture selection: {error}")))
                .unwrap_or(Ok(Self::AllWindows))
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct State {
        pub scope: Scope,
        pub control_generation: u64,
        pub application: AppInfo,
        pub instructions: Option<Instructions>,
        pub accessibility: AxObservation,
        pub windows: Vec<CaptureWindow>,
        pub captures: Vec<Capture>,
        #[serde(deserialize_with = "required_option")]
        pub primary_capture: Option<PrimaryCapture>,
        pub capture_omissions: Vec<CaptureOmission>,
        pub preparation_effects: Vec<FocusEffect>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct MetadataAppInfo {
        pub app: AppRef,
        pub bundle_id: String,
        pub bundle_path: String,
        pub display_name: String,
        pub process_id: i32,
        pub launch_time_unix_ms: Option<u64>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Source {
        pub tool: String,
        pub correlation_id: String,
        pub source_movement_seq: u64,
        pub source_artifact: octopus_interfaces::ArtifactRef,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct MetadataObservation {
        pub id: String,
        pub revision: u64,
        pub control_generation: u64,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SourceImage {
        pub image_id: String,
        pub sha256: String,
        pub width: u32,
        pub height: u32,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct MetadataWindow {
        pub window_id: String,
        pub cg_window_id: u32,
        pub title: Option<String>,
        pub frame_screen_points: Rect,
        pub on_screen: bool,
        pub window_layer: i64,
        pub capture_geometry: Option<CaptureGeometry>,
        pub source_image: Option<SourceImage>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct LiveAppSource {
        pub scope: Scope,
        pub grant_request_id: String,
        pub application: MetadataAppInfo,
        pub source: Source,
        pub observation: MetadataObservation,
        pub windows: Vec<MetadataWindow>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum PresentationSource { Tool }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PresentationIntent {
        pub id: u64,
        pub source: PresentationSource,
        pub app_instance_id: String,
        pub activate: bool,
    }

    impl PresentationIntent {
        pub fn from_source(source: &LiveAppSource) -> Self {
            Self { id: source.source.source_movement_seq, source: PresentationSource::Tool,
                app_instance_id: source.application.app.app_instance_id.clone(), activate: true }
        }
    }

    impl LiveAppSource {
        pub fn from_state(state: &State, grant_request_id: &str, source: Source) -> Result<Self, String> {
            state.validate(&state.scope, &state.application.app)?;
            let app = &state.application;
            let value = Self {
                scope: state.scope.clone(), grant_request_id: grant_request_id.into(), source,
                application: MetadataAppInfo { app: app.app.clone(), bundle_id: app.bundle_id.clone(),
                    bundle_path: app.bundle_path.clone(), display_name: app.display_name.clone(),
                    process_id: app.process_id, launch_time_unix_ms: app.launch_time_unix_ms },
                observation: MetadataObservation { id: state.accessibility.observation_id.clone(),
                    revision: state.accessibility.revision, control_generation: state.control_generation },
                windows: state.windows.iter().map(|window| {
                    let capture = state.captures.iter().find(|capture| capture.window.window_id == window.window_id);
                    MetadataWindow { window_id: window.window_id.clone(), cg_window_id: window.cg_window_id,
                        title: window.title.clone(), frame_screen_points: window.frame_screen_points.clone(),
                        on_screen: window.on_screen, window_layer: window.window_layer,
                        capture_geometry: capture.map(|capture| capture.geometry.clone()),
                        source_image: capture.map(|capture| SourceImage { image_id: capture.image.image_id.clone(),
                            sha256: capture.image.sha256.clone(), width: capture.image.width, height: capture.image.height }) }
                }).collect(),
            };
            value.validate()?;
            Ok(value)
        }

        pub fn validate(&self) -> Result<(), String> {
            self.application.app.validate(&self.scope)?;
            for value in [&self.grant_request_id, &self.application.bundle_id, &self.application.bundle_path,
                &self.application.display_name, &self.source.tool, &self.source.correlation_id, &self.observation.id] { identity(value)?; }
            let digest = self.grant_request_id.strip_prefix("sha256:").ok_or("application source has no grant request digest")?;
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                || self.application.process_id <= 0 || self.application.launch_time_unix_ms.is_some_and(|n| n == 0 || n > MAX_SEQUENCE)
                || !std::path::Path::new(&self.application.bundle_path).is_absolute()
                || self.source.source_movement_seq == 0 || self.source.source_movement_seq > MAX_SEQUENCE
                || self.observation.revision == 0 || self.observation.revision > MAX_SEQUENCE
                || self.observation.control_generation > MAX_SEQUENCE || self.windows.len() > 256 {
                return Err("application source has invalid native or movement coordinates".into());
            }
            let mut ids = BTreeSet::new();
            let mut cg_ids = BTreeSet::new();
            let mut images = BTreeSet::new();
            for window in &self.windows {
                identity(&window.window_id)?;
                window.frame_screen_points.validate()?;
                if window.cg_window_id == 0 || !ids.insert(&window.window_id) || !cg_ids.insert(window.cg_window_id) {
                    return Err("application source has duplicated or invalid window identity".into());
                }
                match (&window.capture_geometry, &window.source_image) {
                    (None, None) => {},
                    (Some(geometry), Some(image)) => {
                        identity(&image.image_id)?;
                        geometry.validate(&window.frame_screen_points, image.width, image.height)?;
                        if !images.insert(&image.image_id) || image.sha256.len() != 64
                            || !image.sha256.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) {
                            return Err("application source has an invalid image identity".into());
                        }
                    },
                    _ => return Err("application source image and geometry must identify the same capture".into()),
                }
            }
            Ok(())
        }
    }

    impl CaptureGeometry {
        fn validate(&self, frame: &Rect, width: u32, height: u32) -> Result<(), String> {
            frame.validate()?;
            self.filter_content_rect.validate()?;
            if self.pixel_width == 0 || self.pixel_height == 0 || self.pixel_width != width || self.pixel_height != height
                || !self.point_pixel_scale.is_finite() || self.point_pixel_scale <= 0.0 || frame.width <= 0.0 || frame.height <= 0.0 {
                return Err("application capture original pixel domain is inconsistent".into());
            }
            let expected = [frame.width / f64::from(width), 0.0, 0.0, frame.height / f64::from(height), frame.x, frame.y];
            if self.pixel_to_screen.iter().zip(expected).any(|(actual, expected)| !actual.is_finite() || (*actual - expected).abs() > 1e-9) {
                return Err("application capture matrix does not describe the exact window image".into());
            }
            Ok(())
        }
    }

    impl State {
        pub fn validate_capture_selection(&self, selection: CaptureSelection, include_screenshots: bool) -> Result<(), String> {
            if !include_screenshots && !self.captures.is_empty() {
                return Err("native application captured images when screenshots were not requested".into());
            }
            if selection == CaptureSelection::Primary && self.captures.iter().any(|capture| {
                self.primary_capture.as_ref().map(|primary| primary.source_window_id.as_str()) != Some(capture.window.window_id.as_str())
            }) {
                return Err("native application captured a supplemental window outside primary selection".into());
            }
            Ok(())
        }

        pub fn validate(&self, scope: &Scope, app: &AppRef) -> Result<(), String> {
            self.application.app.validate(scope)?;
            self.accessibility.validate(scope, app)?;
            if &self.scope != scope || self.control_generation > MAX_SEQUENCE || &self.application.app != app || self.application.process_id <= 0
                || self.application.launch_time_unix_ms == Some(0) || self.windows.len() > 256
                || self.captures.len() > super::MAX_MEDIA_ITEMS {
                return Err("application state identity or observation bounds are invalid".into());
            }
            if let Some(instructions) = &self.instructions {
                if instructions.bundle_id != self.application.bundle_id || instructions.bundle_path != self.application.bundle_path {
                    return Err("application instructions identify a different approved target".into());
                }
                crate::config::ApplicationInstructionText { text:instructions.text.clone(), content_sha256:instructions.content_sha256.clone() }.validate()?;
            }
            if self.preparation_effects.len() > 4096 { return Err("native read preparation exceeded its evidence bound".into()); }
            for effect in &self.preparation_effects {
                identity(&effect.mechanism)?;
                if effect.target_pid != self.application.process_id || effect.application_effect_verified || effect.dispatched_events > 4096 {
                    return Err("native read preparation has foreign or unproved effect evidence".into());
                }
            }
            let mut windows = BTreeSet::new();
            let mut cg_windows = BTreeSet::new();
            for window in &self.windows {
                identity(&window.window_id)?;
                window.frame_screen_points.validate()?;
                if &window.app != app || window.cg_window_id == 0 || !windows.insert(&window.window_id)
                    || !cg_windows.insert(window.cg_window_id) {
                    return Err("application state has a duplicated or foreign native window".into());
                }
            }
            let mut accounted = BTreeSet::new();
            for node in &self.accessibility.nodes {
                if node.window_id.as_ref().is_some_and(|id| !windows.contains(id)) {
                    return Err("accessibility node names a window outside its native application inventory".into());
                }
            }
            let mut image_ids = BTreeSet::new();
            let mut total_bytes = 0usize;
            for capture in &self.captures {
                if !self.windows.contains(&capture.window) || !accounted.insert(&capture.window.window_id)
                    || !image_ids.insert(&capture.image.image_id)
                    || capture.image.byte_length > super::MAX_SOURCE_MEDIA_BYTES - total_bytes {
                    return Err("application capture ownership, media identity or total size is invalid".into());
                }
                capture.validate(scope, app, &capture.window.window_id)?;
                if capture.source.contributors.iter().any(|contributor| matches!(contributor,
                    CaptureContributor::OwnedCursor { cg_window_id, .. } if cg_windows.contains(cg_window_id))) {
                    return Err("an application window cannot be relabeled as an owned cursor contributor".into());
                }
                total_bytes += capture.image.byte_length;
            }
            for omission in &self.capture_omissions {
                identity(&omission.reason)?;
                if !windows.contains(&omission.window_id) || !accounted.insert(&omission.window_id) {
                    return Err("application capture omission does not identify one uncaptured window".into());
                }
            }
            if accounted != windows {
                return Err("application state does not account for every inventoried window capture".into());
            }
            if let Some(primary) = &self.primary_capture {
                identity(&primary.source_window_id)?;
                incarnation(&primary.source_window_incarnation)?;
                if primary.observation_id != self.accessibility.observation_id || primary.revision != self.accessibility.revision
                    || !windows.contains(&primary.source_window_id) {
                    return Err("primary capture does not identify this native observation and window inventory".into());
                }
                match &primary.image_id {
                    Some(image) => {
                        identity(image)?;
                        let capture = self.captures.iter().find(|capture| capture.image.image_id == *image)
                            .ok_or("primary capture image is absent from this operation")?;
                        if capture.window.window_id != primary.source_window_id
                            || capture.source.primary_window_incarnation != primary.source_window_incarnation {
                            return Err("primary image belongs to a different native source incarnation".into());
                        }
                    },
                    None if self.capture_omissions.iter().any(|omission| omission.window_id == primary.source_window_id) => {},
                    None => return Err("an uncaptured primary window requires its explicit capture omission".into()),
                }
            }
            Ok(())
        }
    }

    impl Capture {
        pub fn validate(&self, scope: &Scope, app: &AppRef, window_id: &str) -> Result<(), String> {
            self.app.validate(scope)?;
            identity(&self.image.image_id)?;
            identity(window_id)?;
            self.window.frame_screen_points.validate()?;
            let frame = &self.window.frame_screen_points;
            let geometry = &self.geometry;
            geometry.validate(frame, self.image.width, self.image.height)?;
            incarnation(&self.source.primary_window_incarnation)?;
            self.source.crop_screen_points.validate()?;
            if &self.source.crop_screen_points != frame || self.source.contributors.is_empty() || self.source.contributors.len() > 2 {
                return Err("capture source must preserve the exact primary native window crop and bounded contributor set".into());
            }
            match &self.source.contributors[0] {
                CaptureContributor::ApplicationWindow { window_id, window_incarnation }
                    if window_id == &self.window.window_id && window_incarnation == &self.source.primary_window_incarnation => {},
                _ => return Err("capture contributors do not start with the exact primary application window".into()),
            }
            if let Some(contributor) = self.source.contributors.get(1) {
                match contributor {
                    CaptureContributor::OwnedCursor { cg_window_id, host_generation }
                        if *cg_window_id != 0 && *cg_window_id != self.window.cg_window_id && host_generation == &app.host_generation => {},
                    _ => return Err("additional still-image contributor is not an owned native cursor".into()),
                }
            }
            match self.source.filter_mode {
                CaptureFilterMode::DesktopIndependentWindow if self.window.on_screen && self.source.contributors.len() == 1 => {},
                CaptureFilterMode::DisplayIncludingWindows if self.window.on_screen && self.source.contributors.len() == 2 => {},
                CaptureFilterMode::WindowserverRect if !self.window.on_screen => {},
                _ => return Err("native capture mode does not match the observed primary and contributor state".into()),
            }
            if &self.app != app || self.window.app != self.app || self.window.window_id != window_id
            {
                return Err("application capture target or original pixel domain is inconsistent".into());
            }
            let screen = super::Screen { id: window_id.into(), x: frame.x, y: frame.y, width: frame.width, height: frame.height,
                pixel_width: geometry.pixel_width, pixel_height: geometry.pixel_height };
            super::validate_image(&super::Image { media_type: self.image.media_type.clone(), data_base64: self.image.data_base64.clone(),
                sha256: self.image.sha256.clone(), byte_length: self.image.byte_length, width: self.image.width, height: self.image.height }, &screen)
        }
    }

    fn required_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
        Option::<T>::deserialize(deserializer)
    }

    fn present_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
        T::deserialize(deserializer).map(Some)
    }

    fn incarnation(value: &str) -> Result<(), String> {
        let parsed = value.parse::<u64>().map_err(|_| "native window incarnation must be an exact positive sequence")?;
        if parsed == 0 || parsed > MAX_SEQUENCE || parsed.to_string() != value {
            return Err("native window incarnation exceeds its canonical sequence domain".into());
        }
        Ok(())
    }

    fn identity(value: &str) -> Result<(), String> {
        if value.is_empty() || value.trim() != value || value.contains('\0') || value.len() > 2048 {
            return Err("application identity must be a bounded nonempty exact string".into());
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn app_observations_bind_native_instance_and_exact_diff_base() {
            let scope = Scope { host_generation: "host".into(), worker_environment_id: "room".into(), formation_pin: "formation".into() };
            let app = AppRef { host_generation: "host".into(), app_instance_id: "native-app".into() };
            let mut observation: AxObservation = serde_json::from_value(serde_json::json!({
                "context":{"root_kind":"application","root_element_id":"root","menu_bar_element_id":null,"display_omissions":[]},
                "observation_id":"observation","app":app,"revision":2,"captured_at_unix_ms":1,"mode":"full","base_revision":null,
                "nodes":[{"element_id":"root","parent_id":null,"window_id":null,"window_unavailable_reason":"ax_window_attribute_unavailable",
                    "role":"AXApplication","subrole":null,"title":null,"description":null,"value":null,"value_type":null,"value_input":null,
                    "enabled":null,"focused":null,"frame_screen_points":null,"settable_attributes":[],"actions":[],"unavailable_attributes":[]}],
                "removed_ids":[],"complete":true,"omissions":[],
                "presentation":{"schema_version":1,"format":"macos_ax","context_type":"full_ui","mode":"diff","base_revision":1,
                    "text":"~root application","rendered_element_ids":["root"],"focused_element_id":null,"selected_text":null,"selected_element_ids":[]}
            })).unwrap();
            assert!(observation.validate(&scope, &app).is_ok());
            observation.presentation.base_revision = Some(2);
            assert!(observation.validate(&scope, &app).is_err());
            observation.presentation.base_revision = Some(1);
            observation.presentation.rendered_element_ids.push("foreign".into());
            assert!(observation.validate(&scope, &app).is_err());
            observation.presentation.rendered_element_ids.pop();
            observation.complete = false; observation.omissions.push(AxOmission { parent_id: None, reason: "node_limit".into() });
            assert!(observation.validate(&scope, &app).is_err());
            observation.presentation.mode = "full".into(); observation.presentation.base_revision = None;
            assert!(observation.validate(&scope, &app).is_ok());
            observation.app.app_instance_id = "replacement-app".into();
            assert!(observation.validate(&scope, &app).is_err());
            assert!(serde_json::from_value::<AppRef>(serde_json::json!({"host_generation":"host","app_instance_id":"native-app","approved":true})).is_err());
            assert_eq!(super::super::VERSION, 2);
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Screen {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Focus {
    pub app_id: String,
    pub process_id: u32,
    pub window_id: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub media_type: String,
    pub data_base64: String,
    pub sha256: String,
    pub byte_length: usize,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub id: String,
    pub screen: Screen,
    pub focus: Option<Focus>,
    pub input_revision: String,
    pub captured_at_unix_ms: u64,
    pub image: Option<Image>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    pub screen_capture: bool,
    pub accessibility: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub mode: String,
    pub owner: Option<String>,
    pub generation: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub status: String,
    pub permissions: Permissions,
    pub control: Control,
    pub screens: Vec<Screen>,
    pub observation: Option<Observation>,
    pub effect: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_image_ref: Option<ViewImageRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applications: Vec<application::LiveAppSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_intent: Option<application::PresentationIntent>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ViewImageRef {
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub schema_version: u32,
    pub worker_environment_id: String,
    pub request_id: String,
    pub operation: String,
    pub accepted: bool,
    pub evidence: Option<Evidence>,
    pub error: Option<BackendError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_bootstrap: Option<presentation::Bootstrap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_result: Option<presentation::CompletionResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_result: Option<permission::Snapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_settings_source: Option<permission::SettingsSource>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema_version: u32,
    pub worker_environment_id: String,
    pub request_id: String,
    pub tool: String,
    pub action: Value,
}

pub fn is_app_effect(operation: &str) -> bool {
    matches!(operation, "app_click" | "app_move" | "app_drag" | "app_scroll" | "app_key" | "app_type"
        | "app_set_value" | "app_select_text" | "app_secondary_action" | "app_paste")
}

pub fn is_application(operation: &str) -> bool {
    matches!(operation, "app_state" | "app_resume" | "apps") || is_app_effect(operation)
}

pub fn is_input(operation: &str) -> bool {
    matches!(
        operation,
        "click" | "move" | "type" | "key" | "scroll" | "drag"
    )
}

pub fn validate_screen(screen: &Screen) -> Result<(), String> {
    if screen.id.is_empty()
        || ![screen.x, screen.y, screen.width, screen.height]
            .iter()
            .all(|v| v.is_finite())
        || screen.width <= 0.0
        || screen.height <= 0.0
        || screen.pixel_width == 0
        || screen.pixel_height == 0
    {
        return Err("desktop screen identity/coordinate domain is invalid".into());
    }
    Ok(())
}

pub fn validate_image(image: &Image, screen: &Screen) -> Result<(), String> {
    if image.width != screen.pixel_width || image.height != screen.pixel_height {
        return Err("desktop image dimensions differ from the observed screen".into());
    }
    if image.byte_length == 0 || image.byte_length > MAX_SOURCE_MEDIA_BYTES
        || image.data_base64.len() > image.byte_length.div_ceil(3) * 4 {
        return Err("desktop image exceeds the supported evidence size".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data_base64)
        .map_err(|_| "desktop image is not valid base64")?;
    if bytes.len() != image.byte_length || format!("{:x}", Sha256::digest(&bytes)) != image.sha256 {
        return Err("desktop image content does not match its integrity evidence".into());
    }
    let valid = match image.media_type.as_str() {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        _ => false,
    };
    if !valid {
        return Err("desktop image carrier does not match its media type".into());
    }
    let format = if image.media_type == "image/png" {
        image::ImageFormat::Png
    } else {
        image::ImageFormat::Jpeg
    };
    let decoded = image::load_from_memory_with_format(&bytes, format)
        .map_err(|_| "desktop image cannot be decoded")?;
    if decoded.width() != image.width || decoded.height() != image.height {
        return Err("desktop image encoded dimensions do not match its evidence".into());
    }
    Ok(())
}

pub fn validate_response(response: &Response, request: &Request) -> Result<(), String> {
    let operation = request.action["type"]
        .as_str()
        .ok_or("desktop action has no type")?;
    if response.schema_version != VERSION
        || response.worker_environment_id != request.worker_environment_id
        || response.request_id != request.request_id
        || response.operation != operation
    {
        return Err(
            "desktop backend returned mismatched protocol, room, request or operation identity"
                .into(),
        );
    }
    if !response.accepted {
        let error = response
            .error
            .as_ref()
            .ok_or("desktop refusal has no typed error")?;
        if error.code.is_empty() || error.message.is_empty() {
            return Err("desktop refusal has an empty error".into());
        }
        return Err(format!("{}: {}", error.code, error.message));
    }
    if response.error.is_some() {
        return Err("desktop accepted an action while returning an error".into());
    }
    if response.permission_result.is_some() || response.permission_settings_source.is_some()
        || matches!(operation, "permissions" | "permission_settings_source") {
        return Err("operator permission status is not model evidence".into());
    }
    if is_application(operation) {
        crate::app_authorization::validate_result(response.application_result.as_ref()
            .ok_or("application operation returned no typed result")?, request)?;
    } else if response.application_result.is_some() {
        return Err("display operation returned unexpected application evidence".into());
    }
    let evidence = response
        .evidence
        .as_ref()
        .ok_or("desktop accepted an action without evidence")?;
    if evidence.view_image_ref.is_some() {
        return Err("passive viewer image references are not model evidence".into());
    }
    let mut applications = std::collections::BTreeSet::new();
    if evidence.applications.len() > 1024 { return Err("desktop application source inventory exceeds its bound".into()); }
    for source in &evidence.applications {
        source.validate()?;
        if source.scope.worker_environment_id != request.worker_environment_id
            || !applications.insert((&source.scope.formation_pin, &source.application.app.app_instance_id)) {
            return Err("desktop returned a foreign or duplicated application source".into());
        }
    }
    if let Some(intent) = &evidence.presentation_intent {
        if !intent.activate || !evidence.applications.iter().any(|source|
            source.application.app.app_instance_id == intent.app_instance_id && source.source.source_movement_seq == intent.id) {
            return Err("desktop presentation intent does not identify an observed application source".into());
        }
    }
    if !matches!(
        evidence.status.as_str(),
        "ready" | "permission_required" | "stopped"
    ) || !matches!(
        evidence.control.mode.as_str(),
        "idle" | "model" | "human" | "stopped"
    ) || (evidence.control.mode == "model") != evidence.control.owner.is_some()
        || evidence
            .control
            .owner
            .as_deref()
            .is_some_and(|v| v.is_empty())
    {
        return Err("desktop backend returned an invalid readiness/control state".into());
    }
    if evidence.status == "ready" && !evidence.permissions.screen_capture {
        return Err("desktop readiness lacks screen-capture permission".into());
    }
    for screen in &evidence.screens {
        validate_screen(screen)?;
    }
    if let Some(observation) = &evidence.observation {
        validate_screen(&observation.screen)?;
        if observation.id.is_empty()
            || observation.input_revision.is_empty()
            || observation.captured_at_unix_ms == 0
            || !evidence.screens.contains(&observation.screen)
        {
            return Err("desktop observation has no valid screen/freshness identity".into());
        }
        if let Some(image) = &observation.image {
            validate_image(image, &observation.screen)?;
        }
    }
    if matches!(operation, "observe" | "screenshot") {
        let observation = evidence
            .observation
            .as_ref()
            .ok_or("desktop observation is missing")?;
        if request.action["screen_id"].as_str() != Some(&observation.screen.id) {
            return Err("desktop observation belongs to a different screen".into());
        }
        if (operation == "screenshot"
            || request.action["include_screenshot"].as_bool() == Some(true))
            && observation.image.is_none()
        {
            return Err("desktop screenshot action produced no image evidence".into());
        }
    }
    if is_input(operation) {
        let effect = evidence
            .effect
            .as_ref()
            .ok_or("desktop input has no execution receipt")?;
        if effect["observation_id"] != request.action["observation_id"]
            || effect["control_generation"] != request.action["control_generation"]
            || effect["screen_id"] != request.action["screen_id"]
            || effect["events_posted"].as_u64().unwrap_or(0) == 0
        {
            return Err(
                "desktop execution receipt does not prove the requested observed action".into(),
            );
        }
    }
    Ok(())
}

pub fn failure(request: &Request, code: &str, message: impl Into<String>) -> Response {
    Response {
        schema_version: VERSION,
        worker_environment_id: request.worker_environment_id.clone(),
        request_id: request.request_id.clone(),
        operation: request.action["type"].as_str().unwrap_or("").into(),
        accepted: false,
        evidence: None,
        error: Some(BackendError {
            code: code.into(),
            message: message.into(),
        }),
        application_result: None,
        receiver_bootstrap: None,
        presentation_result: None,
        permission_result: None,
        permission_settings_source: None,
    }
}

pub fn action_target(action: &Value) -> Result<(&str, &str, u64), String> {
    let screen = action["screen_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or("screen_id is required")?;
    let observation = action["observation_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or("observation_id is required")?;
    let generation = action["control_generation"]
        .as_u64()
        .ok_or("control_generation is required")?;
    Ok((screen, observation, generation))
}

pub fn screen_point(screen: &Screen, x: f64, y: f64) -> Result<Value, String> {
    validate_screen(screen)?;
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x >= f64::from(screen.pixel_width)
        || y >= f64::from(screen.pixel_height)
    {
        return Err("desktop point is outside the observed image pixel domain".into());
    }
    Ok(
        json!({"x":screen.x + x * screen.width / f64::from(screen.pixel_width),
        "y":screen.y + y * screen.height / f64::from(screen.pixel_height)}),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAppPolicyDecision { Allowed, Denied, Forbidden }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAppRisk { High, Low }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAppPolicyTarget {
    pub bundle_id: String,
    pub bundle_path: String,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAppPolicy {
    pub decision: NativeAppPolicyDecision,
    pub allow_persistent_approval: bool,
    pub risk: NativeAppRisk,
    pub warning_subtitle: Option<String>,
    pub target: NativeAppPolicyTarget,
}

impl NativeAppPolicy {
    pub(crate) fn validate(&self, target: &crate::app_authorization::ApplicationTarget) -> Result<(), String> {
        if self.target.bundle_id != target.bundle_id || self.target.bundle_path != target.bundle_path
            || self.target.display_name != target.display_name
            || self.warning_subtitle.as_ref().is_some_and(|text| text.is_empty() || text.len() > 4096 || text.contains('\0'))
            || (self.risk == NativeAppRisk::High) != self.warning_subtitle.is_some() {
            return Err("native application policy does not describe its exact resolved target and risk".into());
        }
        Ok(())
    }

    pub(crate) fn require_allowed(&self) -> Result<(), String> {
        match self.decision {
            NativeAppPolicyDecision::Allowed => Ok(()),
            NativeAppPolicyDecision::Denied => Err("application_policy_denied: configured application access does not permit this target".into()),
            NativeAppPolicyDecision::Forbidden => Err("application_policy_forbidden: the declared application eligibility policy excludes this target".into()),
        }
    }
}
