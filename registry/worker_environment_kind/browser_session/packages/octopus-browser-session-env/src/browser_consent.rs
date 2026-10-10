use octopus_descriptors::InterfaceId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
pub(crate) use octopus_app_server_interface::projection::{OperatorPermissionDecision as Decision, OperatorPermissionPersistence as Persistence, OperatorPermissionOffer as Offer, OperatorPermissionResponse as Response, OperatorPermissionChoice as Choice};
type ConsentPersistence = Persistence;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum BrowserOriginAccess { Allow, Deny }
#[derive(Clone, Copy)]
pub enum BrowserConsentOperation { Upload, Download }
#[derive(PartialEq)]
pub enum BrowserConsentPolicyDecision { Allowed, Denied }
fn string_error(error: impl std::fmt::Display) -> String { error.to_string() }
fn require(ok: bool, error: &str) -> Result<(),String> { if ok {Ok(())} else {Err(error.into())} }
fn exact_text(value:&str) -> Result<(),String> { require(!value.is_empty() && value.trim()==value && value.len()<=4096 && !value.contains('\0'),"Browser permission text is not exact") }
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

fn canonical_browser_origin(origin: &str) -> Result<reqwest::Url, String> {
    exact_text(origin)?;
    let url = reqwest::Url::parse(origin).map_err(string_error)?;
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
        let url = reqwest::Url::parse(pattern).map_err(string_error)?;
        require(matches!(url.scheme(), "http" | "https"), "browser origin pattern is not HTTP(S)")?;
        let hostname = url.host_str().ok_or("browser origin pattern has no hostname")?
            .trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_ascii_lowercase();
        require(!hostname.is_empty() && !hostname.chars().any(|c| "?[]{}\\".contains(c))
            && (!host.starts_with('[') || !hostname.contains('*')), "browser origin pattern has unsupported host syntax")?;
        Ok(Self { scheme: url.scheme().into(), port: url.port(), hostname })
    }

    fn key(&self) -> (&str, &str, Option<u16>) { (&self.scheme, &self.hostname, self.port) }

    fn matches(&self, origin: &reqwest::Url) -> bool {
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
    fn resolve(&self, origin: &reqwest::Url) -> Result<BrowserOriginPolicyValues, String> {
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


impl BrowserOriginConsentPolicy {
    pub(crate) fn offer(&self, client: &InterfaceId, origin: &str) -> Result<Offer,String> {
        let (decision, persistent) = self.resolve(origin, BrowserConsentOperation::Upload)?;
        require(decision == BrowserConsentPolicyDecision::Allowed,"Current browser origin policy denies this upload")?;
        require(!self.permitted_persistence.is_empty() && self.permitted_persistence.len()<=3
            && self.permitted_persistence.iter().enumerate().all(|(i,p)| !self.permitted_persistence[..i].contains(p)),"Browser upload persistence configuration is invalid")?;
        offer(client,self.permitted_persistence.iter().copied().filter(|p| *p!=Persistence::Always || persistent).collect())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryPolicy { pub allow_history_access:bool, pub allow_global_persistent_approval:bool }
impl HistoryPolicy {
    pub(crate) fn offer(&self, client:&InterfaceId) -> Result<Offer,String> {
        require(self.allow_history_access,"Current browser policy denies history access")?;
        let mut choices=vec![Persistence::Once]; if self.allow_global_persistent_approval {choices.push(Persistence::Always);}
        offer(client,choices)
    }
}
fn offer(client:&InterfaceId, persistence:Vec<Persistence>) -> Result<Offer,String> {
    require(!persistence.is_empty(),"Browser permission has no eligible approval choice")?;
    let mut choices=persistence.into_iter().map(|persistence| Choice{decision:Decision::Accept,persistence}).collect::<Vec<_>>();
    choices.push(Choice{decision:Decision::Decline,persistence:Persistence::Once});
    let offer=Offer{client_interface:client.clone(),offered_choices:choices};offer.validate()?;Ok(offer)
}
pub(crate) fn params(request_id:&str,call_id:&str,offer:Option<&Offer>) -> Value {
    let mut value=json!({"request_id":request_id,"call_id":call_id});
    if let Some(offer)=offer {value["offered_choices"]=json!(offer.offered_choices);}
    value
}
pub(crate) fn validate_offer(offer:Option<&Offer>,expected:Option<&Offer>) -> Result<(),String> {
    require(offer==expected,"Browser permission offer changed its current policy or exact client")?;
    if let Some(offer)=offer {offer.validate()?;} Ok(())
}
