use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use octopus_descriptors::{CheckpointId, InterfaceId, RunId};
use octopus_ledger::ArtifactRef;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentBounds {
    pub max_policy_bytes: u64,
    pub max_binding_bytes: u64,
    pub max_state_bytes: u64,
    pub max_records: usize,
    pub max_pending: usize,
    pub ledger_batch_rows: usize,
    pub max_proof_bytes: u64,
}

impl ConsentBounds {
    pub fn validate(&self) -> Result<(), String> {
        for (value, maximum) in [
            (self.max_policy_bytes, 65536), (self.max_binding_bytes, 65536),
            (self.max_state_bytes, 1048576), (self.max_records as u64, 4096),
            (self.max_pending as u64, 256), (self.ledger_batch_rows as u64, 256),
            (self.max_proof_bytes, 16777216),
        ] {
            require(value > 0 && value <= maximum, "consent bound exceeds the owner contract")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentPersistence { Once, Session, Always }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentDecision { Accept, Decline }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentChoice {
    pub decision: ConsentDecision,
    pub persistence: ConsentPersistence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserConsentOperation { Upload, Download }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserConsentPolicyDecision { Allowed, Denied, Unavailable }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserOriginAccess { Allow, Deny }

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserOriginPolicyValues {
    pub access: Option<BrowserOriginAccess>,
    pub uploads: Option<BrowserOriginAccess>,
    pub downloads: Option<BrowserOriginAccess>,
    pub persistent_approval: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserOriginPolicyRule {
    pub pattern: String,
    pub policy: BrowserOriginPolicyValues,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserOriginPolicyLayer {
    pub default_policy: BrowserOriginPolicyValues,
    pub origins: Vec<BrowserOriginPolicyRule>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserOriginConsentPolicy {
    pub permitted_persistence: Vec<ConsentPersistence>,
    pub requirements: BrowserOriginPolicyLayer,
    pub configuration: BrowserOriginPolicyLayer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserHistoryConsentPolicy {
    pub sources: Vec<ArtifactRef>,
    pub allow_history_access: bool,
    pub allow_global_persistent_approval: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConsentSubject {
    NativeApplication { bundle_id: String },
    BrowserOrigin { origin: String, operation: BrowserConsentOperation },
    BrowserHistory { source: ArtifactRef },
}

impl ConsentSubject {
    pub fn key(&self) -> Result<String, String> {
        self.validate()?;
        digest_json(self)
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::NativeApplication { bundle_id } => exact_text(bundle_id),
            Self::BrowserOrigin { origin, .. } => canonical_browser_origin(origin).map(|_| ()),
            Self::BrowserHistory { source } => exact_text(source.id.as_str()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentPolicy {
    pub schema_version: u32,
    pub permitted_persistence: Vec<ConsentPersistence>,
    pub browser_origin: Option<BrowserOriginConsentPolicy>,
    pub browser_history: Option<BrowserHistoryConsentPolicy>,
    pub bounds: ConsentBounds,
}

impl ConsentPolicy {
    pub fn validate(&self) -> Result<(), String> {
        self.bounds.validate()?;
        require(self.schema_version == 2 && !self.permitted_persistence.is_empty()
            && self.permitted_persistence.contains(&ConsentPersistence::Session)
            && !self.permitted_persistence.contains(&ConsentPersistence::Once)
            && self.permitted_persistence.iter().collect::<BTreeSet<_>>().len() == self.permitted_persistence.len(),
            "consent policy must declare a distinct supported native persistence set")?;
        if let Some(browser) = &self.browser_origin {
            require(browser.permitted_persistence.contains(&ConsentPersistence::Once)
                && browser.permitted_persistence.iter().collect::<BTreeSet<_>>().len() == browser.permitted_persistence.len(),
                "browser consent must declare distinct choices including once")?;
            for layer in [&browser.requirements, &browser.configuration] {
                require(layer.origins.len() <= self.bounds.max_records, "browser origin policy exceeds its rule bound")?;
                for rule in &layer.origins { BrowserOriginPattern::parse(&rule.pattern)?; }
            }
            require(browser.configuration.default_policy.persistent_approval.is_none()
                && browser.configuration.origins.iter().all(|rule| rule.policy.persistent_approval.is_none()),
                "browser configuration cannot invent a native requirements persistence field")?;
        }
        if let Some(history) = &self.browser_history {
            let mut ids = BTreeSet::new();
            require(!history.sources.is_empty() && history.sources.len() <= self.bounds.max_records,
                "browser history policy must declare bounded exact sources")?;
            for source in &history.sources {
                exact_text(source.id.as_str())?;
                require(ids.insert(&source.id), "browser history source identity is duplicated")?;
            }
        }
        Ok(())
    }
}

fn canonical_browser_origin(origin: &str) -> Result<url::Url, String> {
    exact_text(origin)?;
    let url = url::Url::parse(origin).map_err(string_error)?;
    require(matches!(url.scheme(), "http" | "https") && url.origin().ascii_serialization() == origin,
        "browser consent subject is not the actual canonical HTTP(S) origin")?;
    Ok(url)
}

struct BrowserOriginPattern {
    scheme: String,
    port: Option<u16>,
    hostname: String,
}

impl BrowserOriginPattern {
    fn parse(pattern: &str) -> Result<Self, String> {
        exact_text(pattern)?;
        require(!pattern.chars().any(char::is_whitespace), "browser origin pattern contains whitespace")?;
        let (_, authority) = pattern.split_once("://").ok_or("browser origin pattern has no scheme")?;
        require(!authority.is_empty() && !authority.chars().any(|c| "/?#\\@".contains(c)),
            "browser origin pattern contains a path, user information or query")?;
        let host = if authority.starts_with('[') {
            let end = authority.find(']').ok_or("browser origin pattern has malformed IPv6")?;
            let rest = &authority[end + 1..];
            require(rest.is_empty() || rest.strip_prefix(':').is_some_and(|port| !port.is_empty() && port.bytes().all(|v| v.is_ascii_digit())),
                "browser origin pattern has malformed port")?;
            &authority[..=end]
        } else {
            let host = if let Some((host, port)) = authority.split_once(':') {
                require(!port.is_empty() && port.bytes().all(|v| v.is_ascii_digit()), "browser origin pattern has malformed port")?;
                host
            } else { authority };
            require(!host.is_empty() && !host.contains('%') && !host.ends_with("..")
                && host.strip_suffix('.').unwrap_or(host).split('.').all(|label| !label.is_empty() && (!label.contains('*') || label.is_ascii())),
                "browser origin pattern has malformed hostname labels")?;
            host
        };
        let url = url::Url::parse(pattern).map_err(string_error)?;
        require(matches!(url.scheme(), "http" | "https"), "browser origin pattern is not HTTP(S)")?;
        let hostname = url.host_str().ok_or("browser origin pattern has no hostname")?
            .trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_ascii_lowercase();
        require(!hostname.is_empty() && !hostname.chars().any(|c| "?[]{}\\".contains(c))
            && (!host.starts_with('[') || !hostname.contains('*')), "browser origin pattern has unsupported host syntax")?;
        Ok(Self { scheme: url.scheme().into(), port: url.port(), hostname })
    }

    fn key(&self) -> (&str, &str, Option<u16>) { (&self.scheme, &self.hostname, self.port) }

    fn matches(&self, origin: &url::Url) -> bool {
        if self.scheme != origin.scheme() || self.port != origin.port() { return false; }
        let host = origin.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_ascii_lowercase();
        let (pattern, apex, subdomains) = if let Some(pattern) = self.hostname.strip_prefix("**.") { (pattern, true, true) }
            else if let Some(pattern) = self.hostname.strip_prefix("*.") { (pattern, false, true) }
            else { (self.hostname.as_str(), true, false) };
        (apex && hostname_glob(pattern, &host)) || (subdomains && host.match_indices('.').any(|(at, _)| at > 0 && hostname_glob(pattern, &host[at + 1..])))
    }
}

fn hostname_glob(pattern: &str, host: &str) -> bool {
    let (pattern, host) = (pattern.as_bytes(), host.as_bytes());
    let (mut p, mut h, mut star, mut restart) = (0usize, 0usize, None, 0usize);
    while h < host.len() {
        if pattern.get(p) == host.get(h) { p += 1; h += 1; }
        else if pattern.get(p) == Some(&b'*') { star = Some(p); p += 1; restart = h; }
        else if let Some(at) = star { p = at + 1; restart += 1; h = restart; }
        else { return false; }
    }
    while pattern.get(p) == Some(&b'*') { p += 1; }
    p == pattern.len()
}

impl BrowserOriginPolicyLayer {
    fn resolve(&self, origin: &url::Url) -> Result<BrowserOriginPolicyValues, String> {
        let patterns = self.origins.iter().map(|rule| BrowserOriginPattern::parse(&rule.pattern)).collect::<Result<Vec<_>, _>>()?;
        let mut counts = BTreeMap::new();
        for pattern in &patterns { *counts.entry(pattern.key()).or_insert(0usize) += 1; }
        let mut value = BrowserOriginPolicyValues::default();
        for (rule, pattern) in self.origins.iter().zip(&patterns) {
            if !pattern.matches(origin) { continue; }
            let unique = counts[&pattern.key()] == 1;
            for (current, candidate) in [(&mut value.access, rule.policy.access), (&mut value.uploads, rule.policy.uploads), (&mut value.downloads, rule.policy.downloads)] {
                if *current != Some(BrowserOriginAccess::Deny) && (candidate == Some(BrowserOriginAccess::Deny) || unique && candidate.is_some()) { *current = candidate; }
            }
            if value.persistent_approval != Some(false) && (rule.policy.persistent_approval == Some(false) || unique && rule.policy.persistent_approval.is_some()) {
                value.persistent_approval = rule.policy.persistent_approval;
            }
        }
        value.access = value.access.or(self.default_policy.access);
        value.uploads = value.uploads.or(self.default_policy.uploads);
        value.downloads = value.downloads.or(self.default_policy.downloads);
        value.persistent_approval = value.persistent_approval.or(self.default_policy.persistent_approval);
        Ok(value)
    }
}

impl BrowserOriginConsentPolicy {
    pub fn resolve(&self, origin: &str, operation: BrowserConsentOperation) -> Result<(BrowserConsentPolicyDecision, bool), String> {
        let origin = canonical_browser_origin(origin)?;
        let requirements = self.requirements.resolve(&origin)?;
        let configuration = self.configuration.resolve(&origin)?;
        let transfer = |value: &BrowserOriginPolicyValues| match operation { BrowserConsentOperation::Upload => value.uploads, BrowserConsentOperation::Download => value.downloads };
        let denied = [requirements.access, configuration.access, transfer(&requirements), transfer(&configuration)].contains(&Some(BrowserOriginAccess::Deny));
        Ok((if denied { BrowserConsentPolicyDecision::Denied } else { BrowserConsentPolicyDecision::Allowed }, requirements.persistent_approval != Some(false)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentSession {
    pub run: RunId,
    pub client_interface: InterfaceId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAppPolicyDecision { Allowed, Denied, Forbidden }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAppRisk { High, Low }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeConsentTarget {
    pub bundle_id: String,
    pub bundle_path: String,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAppPolicy {
    pub decision: NativeAppPolicyDecision,
    pub allow_persistent_approval: bool,
    pub risk: NativeAppRisk,
    pub warning_subtitle: Option<String>,
    pub target: NativeConsentTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentOffer {
    pub authority: ArtifactRef,
    pub session: ConsentSession,
    pub subject: ConsentSubject,
    pub eligibility: ConsentEligibility,
    pub offered_choices: Vec<ConsentChoice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConsentEligibility {
    NativeApplication { native_policy: NativeAppPolicy },
    BrowserOrigin {
        policy: ArtifactRef,
        origin: String,
        operation: BrowserConsentOperation,
        decision: BrowserConsentPolicyDecision,
        allow_persistent_approval: bool,
    },
    BrowserHistory {
        policy: ArtifactRef,
        source: ArtifactRef,
        source_payload_digest: String,
        source_epoch: String,
        decision: BrowserConsentPolicyDecision,
    },
}

impl ConsentOffer {
    pub fn new(authority: ArtifactRef, session: ConsentSession, subject: ConsentSubject,
        eligibility: ConsentEligibility, policy: &ConsentPolicy) -> Result<Self, String> {
        let mut offer = Self { authority, session, subject, eligibility, offered_choices: Vec::new() };
        offer.offered_choices = offer.choices(policy)?;
        offer.validate(&offer.authority, policy)?;
        Ok(offer)
    }

    pub fn native_application(authority: ArtifactRef, session: ConsentSession,
        native_policy: NativeAppPolicy, policy: &ConsentPolicy) -> Result<Self, String> {
        Self::new(authority, session, ConsentSubject::NativeApplication { bundle_id: native_policy.target.bundle_id.clone() },
            ConsentEligibility::NativeApplication { native_policy }, policy)
    }

    pub fn native_policy(&self) -> Option<&NativeAppPolicy> {
        match &self.eligibility { ConsentEligibility::NativeApplication { native_policy } => Some(native_policy), _ => None }
    }

    pub fn allows(&self, decision: ConsentDecision, persistence: ConsentPersistence) -> bool {
        self.offered_choices.contains(&ConsentChoice { decision, persistence })
    }

    pub fn policy_denied(&self) -> bool {
        match &self.eligibility {
            ConsentEligibility::NativeApplication { .. } => false,
            ConsentEligibility::BrowserOrigin { decision, .. } | ConsentEligibility::BrowserHistory { decision, .. } =>
                *decision != BrowserConsentPolicyDecision::Allowed,
        }
    }

    fn choices(&self, policy: &ConsentPolicy) -> Result<Vec<ConsentChoice>, String> {
        let (persistence, accept, persistent_accept, persistent_decline) = match &self.eligibility {
            ConsentEligibility::NativeApplication { native_policy } =>
                (policy.permitted_persistence.clone(), true, native_policy.allow_persistent_approval, false),
            ConsentEligibility::BrowserOrigin { decision, allow_persistent_approval, .. } => {
                let policy = policy.browser_origin.as_ref().ok_or("browser origin consent was not selected in this authority")?;
                (policy.permitted_persistence.clone(), *decision == BrowserConsentPolicyDecision::Allowed, *allow_persistent_approval, true)
            }
            ConsentEligibility::BrowserHistory { decision, .. } => {
                let policy = policy.browser_history.as_ref().ok_or("browser history consent was not selected in this authority")?;
                (vec![ConsentPersistence::Once, ConsentPersistence::Always],
                    *decision == BrowserConsentPolicyDecision::Allowed, policy.allow_global_persistent_approval, false)
            }
        };
        let mut choices = Vec::new();
        for persistence in persistence {
            if accept && (persistence != ConsentPersistence::Always || persistent_accept) {
                choices.push(ConsentChoice { decision: ConsentDecision::Accept, persistence });
            }
            if persistence != ConsentPersistence::Always || persistent_decline {
                choices.push(ConsentChoice { decision: ConsentDecision::Decline, persistence });
            }
        }
        Ok(choices)
    }

    pub fn validate(&self, authority: &ArtifactRef, policy: &ConsentPolicy) -> Result<(), String> {
        policy.validate()?;
        self.subject.validate()?;
        exact_text(self.session.run.as_str())?;
        exact_text(self.session.client_interface.as_str())?;
        require(self.authority == *authority, "consent offer names another bound authority")?;
        match (&self.subject, &self.eligibility) {
            (ConsentSubject::NativeApplication { bundle_id }, ConsentEligibility::NativeApplication { native_policy }) => {
                for value in [&native_policy.target.bundle_id, &native_policy.target.bundle_path, &native_policy.target.display_name] { exact_text(value)?; }
                require(*bundle_id == native_policy.target.bundle_id && normal_path(Path::new(&native_policy.target.bundle_path))
                    && native_policy.decision == NativeAppPolicyDecision::Allowed,
                    "native consent requires its freshly resolved allowed target")?;
                if let Some(warning) = &native_policy.warning_subtitle {
                    require(warning.len() <= 4096 && !warning.contains('\0'), "native policy warning exceeds its text contract")?;
                }
            }
            (ConsentSubject::BrowserOrigin { origin, operation }, ConsentEligibility::BrowserOrigin {
                policy: reference, origin: observed, operation: observed_operation, decision, allow_persistent_approval }) => {
                exact_text(reference.id.as_str())?;
                let browser = policy.browser_origin.as_ref().ok_or("authority has no selected browser origin policy")?;
                let (resolved, persistent) = browser.resolve(origin, *operation)?;
                require(origin == observed && operation == observed_operation
                    && (*decision != BrowserConsentPolicyDecision::Allowed || resolved == BrowserConsentPolicyDecision::Allowed)
                    && (!allow_persistent_approval || persistent),
                    "browser consent changed its observed origin, operation or current source policy")?;
            }
            (ConsentSubject::BrowserHistory { source }, ConsentEligibility::BrowserHistory {
                policy: reference, source: observed, source_payload_digest, source_epoch, decision }) => {
                exact_text(reference.id.as_str())?;
                exact_text(source_epoch)?;
                require(source_payload_digest.len() == 64 && source_payload_digest.bytes().all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
                    "history eligibility has no exact source payload digest")?;
                let history = policy.browser_history.as_ref().ok_or("authority has no selected history policy")?;
                require(source == observed && history.sources.iter().filter(|r| r.id == source.id).eq([source])
                    && (*decision != BrowserConsentPolicyDecision::Allowed || history.allow_history_access),
                    "history consent changed its exact selected source or current read policy")?;
            }
            _ => return Err("consent subject and eligibility tags do not match".into()),
        }
        require(self.offered_choices == self.choices(policy)? && !self.offered_choices.is_empty(),
            "consent offer differs from the exact source-supported decision and persistence pairs")
    }
}

#[doc(hidden)]
pub fn normal_path(path: &Path) -> bool {
    path.is_absolute() && path.components().all(|p| matches!(p, Component::RootDir | Component::Normal(_)))
}

#[doc(hidden)]
pub fn exact_text(value: &str) -> Result<(), String> {
    require(!value.is_empty() && value.trim() == value && value.len() <= 4096 && !value.contains('\0'), "consent field is not bounded exact text")
}

#[doc(hidden)]
pub fn digest_json(value: &impl Serialize) -> Result<String, String> {
    let value = serde_json::to_value(value).map_err(string_error)?;
    Ok(format!("{:x}", Sha256::digest(octopus_descriptors::canonical_bytes(&value))))
}

#[doc(hidden)]
pub fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message.into()) }
}

#[doc(hidden)]
pub fn string_error(error: impl std::fmt::Display) -> String { error.to_string() }

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedRunWriter {
    pub owner: InterfaceId,
    pub source: InterfaceId,
    pub door: InterfaceId,
    pub tool: String,
    pub call_label: String,
    pub result_label: String,
}


#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentEvidenceRef {
    pub artifact: ArtifactRef,
    pub movement_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentHumanEvidence {
    pub response: ConsentEvidenceRef,
    pub mark_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConsentIntake {
    Query { authority: ArtifactRef, request: ConsentEvidenceRef, original_call: ConsentEvidenceRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected_return: Option<SelectedConsentReturn> },
    RecordHuman { authority: ArtifactRef, request_id: String, human: ConsentHumanEvidence,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        human_required: Option<ConsentEvidenceRef> },
    ListAlways { authority: ArtifactRef },
    RemoveAlways { authority: ArtifactRef, request_id: String, subject: ConsentSubject, receipt_id: String, human: ConsentHumanEvidence },
}

impl ConsentIntake {
    pub fn authority(&self) -> &ArtifactRef {
        match self { Self::Query { authority, .. } | Self::RecordHuman { authority, .. }
            | Self::ListAlways { authority } | Self::RemoveAlways { authority, .. } => authority }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentCommand {
    pub authority: ArtifactRef,
    pub intake: ConsentEvidenceRef,
    pub before_hash: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentPolicyAuthorization {
    pub authority: ArtifactRef,
    pub command: ConsentEvidenceRef,
    pub writer_call: ConsentEvidenceRef,
    pub writer_result: ConsentEvidenceRef,
    pub committed_run_hash: String,
    pub record_id: String,
}


#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedRunInstall {
    pub call: ConsentEvidenceRef,
    pub result: ConsentEvidenceRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedConsentBinding {
    pub authority: ArtifactRef,
    pub policy: ArtifactRef,
    pub home_interface: InterfaceId,
    pub client_interface: InterfaceId,
}

impl SelectedConsentBinding {
    pub fn intake_checkpoint(&self, owner: &InterfaceId) -> Result<(String, octopus_interfaces::CheckpointBindingPolicySnapshot), String> {
        self.validate()?;
        require(owner != &self.home_interface && owner != &self.client_interface, "selected consent intake requires its original independent owner")?;
        let binding: octopus_interfaces::CheckpointBindingPolicySnapshot = serde_json::from_value(serde_json::json!({
            "inbound":{"interface":owner,"config":{"artifact_policy":{"mode":"allow","names":["application.consent.intake"],"namespaces":[]}}},
            "outbound":[{"interface":self.home_interface,"config":{"artifact_policy":{"mode":"allow","names":["application.consent.intake"],"namespaces":[]}}}]
        })).map_err(|e|e.to_string())?;
        let digest = Sha256::digest(octopus_descriptors::canonical_bytes(&serde_json::to_value(&binding).map_err(|e|e.to_string())?));
        Ok((format!("application_consent_intake_{digest:x}"), binding))
    }

    pub fn validate(&self) -> Result<(), String> {
        for value in [self.authority.id.as_str(), self.policy.id.as_str(), self.home_interface.as_str(), self.client_interface.as_str()] {
            exact_text(value)?;
        }
        require(self.authority.id != self.policy.id && self.home_interface != self.client_interface,
            "selected consent requires distinct immutable authority/policy and Home/task client")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedConsentReturn {
    pub run_path: PathBuf,
    pub provider_writer: CheckedRunWriter,
    pub owner_writer: Option<CheckedRunWriter>,
    pub original_install: CheckedRunInstall,
    pub human_required: CheckpointId,
    pub approved: CheckpointId,
    pub declined: CheckpointId,
    pub selected_artifacts: Vec<ArtifactRef>,
}
