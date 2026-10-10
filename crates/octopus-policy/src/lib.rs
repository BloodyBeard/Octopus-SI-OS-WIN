#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use octopus_descriptors::{
    ArtifactId, DescriptorHash, DescriptorName, InterfaceId, MarkId, ToolId,
    WorkerEnvironmentKindId,
};
use serde::{Deserialize, Serialize};

pub mod worker_interface;
pub use worker_interface::{WorkerInterfaceCeiling, WorkerInterfacePolicy};

pub type InterfacePolicyValues = BTreeMap<String, serde_json::Value>;
pub type InterfacePolicyGrant = BTreeMap<InterfaceId, InterfacePolicyValues>;
pub type InterfacePolicyOverrideLayer = BTreeMap<InterfaceId, InterfacePolicyValues>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InterfacePolicyReducer {
    Min,
    Intersection,
    DenyUnion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InterfacePolicyVariableDefinition {
    pub reduce: InterfacePolicyReducer,
    pub default: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InterfacePolicyDeclaration {
    #[serde(default)]
    pub variables: BTreeMap<String, InterfacePolicyVariableDefinition>,
    #[serde(default)]
    pub values: InterfacePolicyValues,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormedInterfacePolicy {
    pub boundary: InterfacePolicyValues,
    pub context: InterfacePolicyValues,
}

pub fn validate_interface_policy_declaration(
    declaration: &InterfacePolicyDeclaration,
) -> Result<(), String> {
    form_interface_policy_values(&declaration.variables, &[&declaration.values]).map(|_| ())
}

pub fn form_interface_policy(
    declaration: &InterfacePolicyDeclaration,
    worker_layer: Option<&InterfacePolicyValues>,
    ee_layers: &[&InterfacePolicyValues],
) -> Result<FormedInterfacePolicy, String> {
    let mut boundary_layers = Vec::new();
    if !declaration.values.is_empty() {
        boundary_layers.push(&declaration.values);
    }
    if let Some(worker_layer) = worker_layer {
        boundary_layers.push(worker_layer);
    }
    let boundary = form_interface_policy_values(&declaration.variables, &boundary_layers)?;
    let mut context_layers = boundary_layers;
    context_layers.extend_from_slice(ee_layers);
    let context = form_interface_policy_values(&declaration.variables, &context_layers)?;
    Ok(FormedInterfacePolicy { boundary, context })
}

pub fn form_interface_policy_values(
    definitions: &BTreeMap<String, InterfacePolicyVariableDefinition>,
    layers: &[&InterfacePolicyValues],
) -> Result<InterfacePolicyValues, String> {
    let mut formed = InterfacePolicyValues::new();
    for (name, definition) in definitions {
        validate_policy_variable_name(name)?;
        validate_policy_variable_value(name, definition.reduce, &definition.default)?;
        formed.insert(
            name.clone(),
            canonical_policy_value(definition.reduce, &definition.default)?,
        );
    }
    for layer in layers {
        for (name, incoming) in *layer {
            let definition = definitions.get(name).ok_or_else(|| {
                format!("interface policy override names undeclared variable '{name}'")
            })?;
            validate_policy_variable_value(name, definition.reduce, incoming)?;
            let current = formed.get(name).ok_or_else(|| {
                format!("interface policy variable '{name}' has no formed default")
            })?;
            formed.insert(
                name.clone(),
                reduce_policy_variable(name, definition.reduce, current, incoming)?,
            );
        }
    }
    Ok(formed)
}

fn validate_policy_variable_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.trim() != name
        || !name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
    {
        return Err(format!(
            "interface policy variable name '{name}' must be non-empty lowercase snake_case"
        ));
    }
    Ok(())
}

fn validate_policy_variable_value(
    name: &str,
    reducer: InterfacePolicyReducer,
    value: &serde_json::Value,
) -> Result<(), String> {
    match reducer {
        InterfacePolicyReducer::Min => {
            if value.as_u64().is_none() {
                return Err(format!(
                    "interface policy variable '{name}' uses min and requires an unsigned integer"
                ));
            }
        }
        InterfacePolicyReducer::Intersection | InterfacePolicyReducer::DenyUnion => {
            let values = value.as_array().ok_or_else(|| {
                format!(
                    "interface policy variable '{name}' uses a set reducer and requires an array of strings"
                )
            })?;
            let mut seen = BTreeSet::new();
            for item in values {
                let item = item.as_str().ok_or_else(|| {
                    format!(
                        "interface policy variable '{name}' set values must be non-empty strings"
                    )
                })?;
                if item.is_empty() || item.trim() != item {
                    return Err(format!(
                        "interface policy variable '{name}' set values must be non-empty strings without surrounding whitespace"
                    ));
                }
                if !seen.insert(item) {
                    return Err(format!(
                        "interface policy variable '{name}' contains duplicate set value '{item}'"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn canonical_policy_value(
    reducer: InterfacePolicyReducer,
    value: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match reducer {
        InterfacePolicyReducer::Min => Ok(value.clone()),
        InterfacePolicyReducer::Intersection | InterfacePolicyReducer::DenyUnion => {
            let mut values = value
                .as_array()
                .ok_or_else(|| "set policy value is not an array".to_string())?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            values.sort();
            Ok(serde_json::Value::Array(
                values.into_iter().map(serde_json::Value::String).collect(),
            ))
        }
    }
}

fn reduce_policy_variable(
    name: &str,
    reducer: InterfacePolicyReducer,
    current: &serde_json::Value,
    incoming: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match reducer {
        InterfacePolicyReducer::Min => {
            Ok(serde_json::json!(current
                .as_u64()
                .ok_or_else(|| format!("formed min variable '{name}' is not unsigned"))?
                .min(incoming.as_u64().ok_or_else(|| format!(
                    "incoming min variable '{name}' is not unsigned"
                ))?,)))
        }
        InterfacePolicyReducer::Intersection => {
            let current = current
                .as_array()
                .ok_or_else(|| format!("formed intersection variable '{name}' is not an array"))?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<BTreeSet<_>>();
            let incoming = incoming
                .as_array()
                .ok_or_else(|| format!("incoming intersection variable '{name}' is not an array"))?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<BTreeSet<_>>();
            Ok(serde_json::Value::Array(
                current
                    .intersection(&incoming)
                    .map(|value| serde_json::Value::String((*value).to_string()))
                    .collect(),
            ))
        }
        InterfacePolicyReducer::DenyUnion => {
            let current = current
                .as_array()
                .ok_or_else(|| format!("formed deny-union variable '{name}' is not an array"))?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<BTreeSet<_>>();
            let incoming = incoming
                .as_array()
                .ok_or_else(|| format!("incoming deny-union variable '{name}' is not an array"))?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<BTreeSet<_>>();
            Ok(serde_json::Value::Array(
                current
                    .union(&incoming)
                    .map(|value| serde_json::Value::String((*value).to_string()))
                    .collect(),
            ))
        }
    }
}

/// POLICY.md §4 artifact policy: object-level lifecycle rights on the artifact
/// itself, a distinct axis from the interface binding (the default flow). The
/// four verbs are the closed vocabulary the owner names (read / change / create /
/// delete); a worker interface ceiling declares which of them the worker may ever
/// exercise, and the run's authored artifact binding declares the concrete rights
/// on a specific artifact, masked down to that verb ceiling by formation.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactVerb {
    Read,
    Change,
    Create,
    Delete,
}

impl ArtifactVerb {
    pub const ALL: [ArtifactVerb; 4] = [
        ArtifactVerb::Read,
        ArtifactVerb::Change,
        ArtifactVerb::Create,
        ArtifactVerb::Delete,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ArtifactVerb::Read => "read",
            ArtifactVerb::Change => "change",
            ArtifactVerb::Create => "create",
            ArtifactVerb::Delete => "delete",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum BindingArtifactPolicy {
    All,
    Allow {
        names: Vec<String>,
        namespaces: Vec<String>,
    },
}

impl BindingArtifactPolicy {
    pub fn validate(&self) -> Result<(), String> {
        let BindingArtifactPolicy::Allow { names, namespaces } = self else {
            return Ok(());
        };
        if names.is_empty() && namespaces.is_empty() {
            return Err(
                "binding artifact policy mode=allow must name at least one artifact name or namespace"
                    .to_string(),
            );
        }
        let mut seen_names = BTreeSet::new();
        for name in names {
            if name.trim() != name || name.is_empty() || !seen_names.insert(name) {
                return Err(format!(
                    "binding artifact policy contains invalid or duplicate name '{name}'"
                ));
            }
        }
        let mut seen_namespaces = BTreeSet::new();
        for namespace in namespaces {
            if namespace.trim() != namespace
                || namespace.is_empty()
                || namespace.ends_with('.')
                || !seen_namespaces.insert(namespace)
            {
                return Err(format!(
                    "binding artifact policy contains invalid or duplicate namespace '{namespace}'"
                ));
            }
        }
        Ok(())
    }

    pub fn admits(&self, name: &str) -> bool {
        match self {
            BindingArtifactPolicy::All => true,
            BindingArtifactPolicy::Allow { names, namespaces } => {
                names.iter().any(|allowed| allowed == name)
                    || namespaces.iter().any(|namespace| {
                        name == namespace
                            || name
                                .strip_prefix(namespace)
                                .is_some_and(|suffix| suffix.starts_with('.'))
                    })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InterfaceBindingConfig {
    pub artifact_policy: BindingArtifactPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthoredInterfaceBinding {
    pub interface: InterfaceId,
    pub config: InterfaceBindingConfig,
}

impl AuthoredInterfaceBinding {
    pub fn all(interface: InterfaceId) -> Self {
        Self {
            interface,
            config: InterfaceBindingConfig {
                artifact_policy: BindingArtifactPolicy::All,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.interface.as_str().trim().is_empty() {
            return Err("interface binding names an empty interface folder".to_string());
        }
        self.config.artifact_policy.validate()
    }

    pub fn admits(&self, artifact_name: &str) -> bool {
        self.config.artifact_policy.admits(artifact_name)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAccess {
    #[serde(default = "crate::bool_true")]
    pub read: bool,
    #[serde(default)]
    pub change: bool,
    #[serde(default)]
    pub create: bool,
    #[serde(default)]
    pub delete: bool,
}

impl Default for ArtifactAccess {
    fn default() -> Self {
        Self::read_only()
    }
}

impl ArtifactAccess {
    /// A bare binding grants read of the bound artifact — that is the baseline
    /// meaning of binding an artifact as context.
    pub fn read_only() -> Self {
        Self {
            read: true,
            change: false,
            create: false,
            delete: false,
        }
    }

    pub fn none() -> Self {
        Self {
            read: false,
            change: false,
            create: false,
            delete: false,
        }
    }

    pub fn full() -> Self {
        Self {
            read: true,
            change: true,
            create: true,
            delete: true,
        }
    }

    pub fn allows(&self, verb: ArtifactVerb) -> bool {
        match verb {
            ArtifactVerb::Read => self.read,
            ArtifactVerb::Change => self.change,
            ArtifactVerb::Create => self.create,
            ArtifactVerb::Delete => self.delete,
        }
    }

    pub fn grants_any(&self) -> bool {
        self.read || self.change || self.create || self.delete
    }

    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            read: self.read && other.read,
            change: self.change && other.change,
            create: self.create && other.create,
            delete: self.delete && other.delete,
        }
    }

    /// Mask the authored rights down to a verb ceiling: a right the ceiling does
    /// not permit is stripped even if the binding authored it (POLICY.md §5
    /// tie-break — the narrower wins, widening is impossible).
    pub fn masked_by_verbs(&self, verbs: &CapSet<ArtifactVerb>) -> Self {
        Self {
            read: self.read && verbs.contains(&ArtifactVerb::Read),
            change: self.change && verbs.contains(&ArtifactVerb::Change),
            create: self.create && verbs.contains(&ArtifactVerb::Create),
            delete: self.delete && verbs.contains(&ArtifactVerb::Delete),
        }
    }
}

pub(crate) fn bool_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapSet<T: Ord + Clone>(BTreeSet<T>);

impl<T: Ord + Clone> Default for CapSet<T> {
    fn default() -> Self {
        Self(BTreeSet::new())
    }
}

impl<T: Ord + Clone> CapSet<T> {
    pub fn from_allowed(items: impl IntoIterator<Item = T>) -> Self {
        Self(items.into_iter().collect())
    }
    pub fn intersect(&self, other: &Self) -> Self {
        Self(self.0.intersection(&other.0).cloned().collect())
    }
    pub fn without(&self, deny: &DenySet<T>) -> Self {
        Self(
            self.0
                .iter()
                .filter(|x| !deny.contains(x))
                .cloned()
                .collect(),
        )
    }
    pub fn contains(&self, x: &T) -> bool {
        self.0.contains(x)
    }
    pub fn is_subset(&self, other: &Self) -> bool {
        self.0.is_subset(&other.0)
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.0.iter()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DenySet<T: Ord + Clone>(BTreeSet<T>);

impl<T: Ord + Clone> Default for DenySet<T> {
    fn default() -> Self {
        Self(BTreeSet::new())
    }
}

impl<T: Ord + Clone> DenySet<T> {
    pub fn from_denied(items: impl IntoIterator<Item = T>) -> Self {
        Self(items.into_iter().collect())
    }
    pub fn union(&self, other: &Self) -> Self {
        Self(self.0.union(&other.0).cloned().collect())
    }
    pub fn contains(&self, x: &T) -> bool {
        self.0.contains(x)
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.0.iter()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextGrant {
    pub tools: CapSet<ToolId>,
    pub interfaces: CapSet<InterfaceId>,
    pub wek: CapSet<WorkerEnvironmentKindId>,
    pub mark_outcomes: CapSet<MarkId>,
    pub hard_blocks_tools: DenySet<ToolId>,
    #[serde(default)]
    pub interface_policy: InterfacePolicyGrant,
    /// POLICY.md §4 artifact policy — the object-level verbs this grant may ever
    /// exercise on any artifact (the composable ceiling for read/change/create/
    /// delete). `boundary` carries the worker-interface ceiling; `context`
    /// carries it narrowed by the EE chain.
    #[serde(default)]
    pub artifact_verbs: CapSet<ArtifactVerb>,
    /// POLICY.md §4 artifact policy — concrete per-artifact rights formed from the
    /// run's authored artifact bindings masked by `artifact_verbs`. Distinct axis
    /// from the interface binding (the default flow); no overlap.
    #[serde(default)]
    pub artifact_access: BTreeMap<ArtifactId, ArtifactAccess>,
}

impl ContextGrant {
    pub fn denies_tool(&self, tool: &ToolId) -> bool {
        self.hard_blocks_tools.contains(tool)
    }

    pub fn allows_tool(&self, tool: &ToolId) -> bool {
        self.tools.contains(tool) && !self.denies_tool(tool)
    }

    pub fn tool_allow_set_is_subset_of(&self, parent: &ContextGrant) -> bool {
        self.tools.is_subset(&parent.tools)
    }

    pub fn allows_no_tool_hard_blocked_by(&self, parent: &ContextGrant) -> bool {
        self.tools.iter().all(|tool| !parent.denies_tool(tool))
    }
}

#[derive(Clone, Debug, Default)]
pub struct PolicyLayer {
    pub tools: Option<CapSet<ToolId>>,
    pub interfaces: Option<CapSet<InterfaceId>>,
    pub wek: Option<CapSet<WorkerEnvironmentKindId>>,
    pub mark_outcomes: Option<CapSet<MarkId>>,
    pub hard_blocks_tools: DenySet<ToolId>,
    /// POLICY.md §4/§5 — an EE layer or the worker ceiling narrows which artifact
    /// verbs are permitted at all (`None` = no narrowing on this axis).
    pub artifact_verbs: Option<CapSet<ArtifactVerb>>,
    /// The run's authored per-artifact rights (carried by the bindings layer).
    /// Formation masks each entry down to the composed `artifact_verbs`.
    pub artifact_access: BTreeMap<ArtifactId, ArtifactAccess>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FormationPin {
    pub inputs: BTreeMap<DescriptorName, DescriptorHash>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormedPolicy {
    pub boundary: ContextGrant,
    pub context: ContextGrant,
    pub pin: FormationPin,
}

fn apply_layer(acc: &mut ContextGrant, layer: &PolicyLayer) {
    if let Some(t) = &layer.tools {
        acc.tools = acc.tools.intersect(t);
    }
    if let Some(i) = &layer.interfaces {
        acc.interfaces = acc.interfaces.intersect(i);
    }
    if let Some(k) = &layer.wek {
        acc.wek = acc.wek.intersect(k);
    }
    if let Some(m) = &layer.mark_outcomes {
        acc.mark_outcomes = acc.mark_outcomes.intersect(m);
    }
    if let Some(v) = &layer.artifact_verbs {
        acc.artifact_verbs = acc.artifact_verbs.intersect(v);
    }
    acc.hard_blocks_tools = acc.hard_blocks_tools.union(&layer.hard_blocks_tools);
    for (id, access) in &layer.artifact_access {
        let masked = access.masked_by_verbs(&acc.artifact_verbs);
        acc.artifact_access
            .entry(id.clone())
            .and_modify(|existing| *existing = existing.intersect(&masked))
            .or_insert(masked);
    }
}

pub fn form_policy(
    ceiling: ContextGrant,
    ee_chain: &[PolicyLayer],
    bindings: &[PolicyLayer],
    pin: FormationPin,
) -> FormedPolicy {
    let boundary = ceiling.clone();
    let mut acc = ceiling;
    for layer in ee_chain.iter().chain(bindings.iter()) {
        apply_layer(&mut acc, layer);
    }
    acc.tools = acc.tools.without(&acc.hard_blocks_tools);
    FormedPolicy {
        boundary,
        context: acc,
        pin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(items: &[&str]) -> CapSet<ToolId> {
        CapSet::from_allowed(items.iter().map(|s| ToolId::new(*s)))
    }
    fn ceiling(tool_items: &[&str]) -> ContextGrant {
        ContextGrant {
            tools: tools(tool_items),
            interfaces: CapSet::from_allowed(std::iter::empty()),
            wek: CapSet::from_allowed(std::iter::empty()),
            mark_outcomes: CapSet::from_allowed(std::iter::empty()),
            hard_blocks_tools: DenySet::default(),
            interface_policy: Default::default(),
            artifact_verbs: CapSet::from_allowed(ArtifactVerb::ALL),
            artifact_access: Default::default(),
        }
    }

    #[test]
    fn formation_only_narrows_widening_impossible_deny_wins_deterministic() {
        let ceil = ceiling(&["a", "b", "c"]);

        let ee = PolicyLayer {
            tools: Some(tools(&["b", "c", "d"])),
            ..Default::default()
        };
        let binding = PolicyLayer {
            tools: Some(tools(&["c", "d"])),
            hard_blocks_tools: DenySet::from_denied([ToolId::new("c")]),
            ..Default::default()
        };

        let formed = form_policy(
            ceil.clone(),
            &[ee.clone()],
            &[binding.clone()],
            FormationPin::default(),
        );

        assert!(
            formed.context.tools.is_subset(&ceil.tools),
            "context ⊆ boundary"
        );
        assert!(
            !formed.context.tools.contains(&ToolId::new("d")),
            "cannot gain d (not in ceiling)"
        );
        assert!(
            formed.context.tools.is_empty(),
            "deny beats subset: c removed"
        );

        let binding2 = PolicyLayer {
            tools: Some(tools(&["c", "d"])),
            ..Default::default()
        };
        let formed2 = form_policy(
            ceil.clone(),
            &[ee.clone()],
            &[binding2.clone()],
            FormationPin::default(),
        );
        assert_eq!(formed2.context.tools, tools(&["c"]));

        let again = form_policy(ceil, &[ee], &[binding2], FormationPin::default());
        assert_eq!(formed2.context, again.context);
    }

    #[test]
    fn generic_interface_policy_forms_descriptor_worker_and_ee_layers() {
        let declaration: InterfacePolicyDeclaration = serde_json::from_value(serde_json::json!({
            "variables": {
                "available_options": {
                    "reduce": "intersection",
                    "default": ["alpha", "beta", "gamma"]
                },
                "max_attempts": {"reduce": "min", "default": 9}
            },
            "values": {"available_options": ["alpha", "beta"], "max_attempts": 8}
        }))
        .unwrap();
        let worker = serde_json::from_value(serde_json::json!({
            "available_options": ["beta", "gamma"],
            "max_attempts": 5
        }))
        .unwrap();
        let ee = serde_json::from_value(serde_json::json!({
            "available_options": ["beta"],
            "max_attempts": 3
        }))
        .unwrap();
        let formed = form_interface_policy(&declaration, Some(&worker), &[&ee]).unwrap();
        assert_eq!(
            formed.boundary,
            serde_json::from_value(serde_json::json!({
                "available_options": ["beta"],
                "max_attempts": 5
            }))
            .unwrap()
        );
        assert_eq!(
            formed.context,
            serde_json::from_value(serde_json::json!({
                "available_options": ["beta"],
                "max_attempts": 3
            }))
            .unwrap()
        );
    }

    #[test]
    fn interface_reach_is_the_interfaces_cap_narrowing_inward() {
        let ceiling = ContextGrant {
            tools: tools(&[]),
            interfaces: CapSet::from_allowed([
                InterfaceId::new("world_allowed"),
                InterfaceId::new("world_dropped"),
            ]),
            wek: CapSet::from_allowed(std::iter::empty()),
            mark_outcomes: CapSet::from_allowed(std::iter::empty()),
            hard_blocks_tools: DenySet::default(),
            interface_policy: Default::default(),
            artifact_verbs: Default::default(),
            artifact_access: Default::default(),
        };
        let layer = PolicyLayer {
            interfaces: Some(CapSet::from_allowed([InterfaceId::new("world_allowed")])),
            ..Default::default()
        };

        let formed = form_policy(ceiling, &[layer], &[], FormationPin::default());

        assert_eq!(
            formed.context.interfaces,
            CapSet::from_allowed([InterfaceId::new("world_allowed")]),
            "a run's reach is the interface-folder cap alone — there is no second binding-name axis to intersect"
        );
    }

    #[test]
    fn artifact_access_is_masked_to_the_composed_verb_ceiling() {
        let mut ceil = ceiling(&[]);
        ceil.artifact_verbs = CapSet::from_allowed([ArtifactVerb::Read, ArtifactVerb::Change]);

        let ee = PolicyLayer {
            artifact_verbs: Some(CapSet::from_allowed([ArtifactVerb::Read])),
            ..Default::default()
        };

        let mut authored = BTreeMap::new();
        authored.insert(ArtifactId::new("artifact-x"), ArtifactAccess::full());
        let binding = PolicyLayer {
            artifact_access: authored,
            ..Default::default()
        };

        let formed = form_policy(ceil, &[ee], &[binding], FormationPin::default());
        let granted = formed
            .context
            .artifact_access
            .get(&ArtifactId::new("artifact-x"))
            .expect("the authored artifact binding is formed into a per-artifact grant");
        assert_eq!(
            *granted,
            ArtifactAccess::read_only(),
            "full authored access is masked down to the ceiling ∩ EE verb set (read only)"
        );
        assert!(
            formed.boundary.artifact_access.is_empty(),
            "the boundary carries the verb ceiling, never a per-artifact grant"
        );
        assert!(formed
            .boundary
            .artifact_verbs
            .contains(&ArtifactVerb::Change));
        assert!(
            !formed
                .context
                .artifact_verbs
                .contains(&ArtifactVerb::Change),
            "the EE layer narrowed the context verb ceiling below the boundary"
        );
    }

    #[test]
    fn binding_artifact_policy_is_explicit_and_matches_only_declared_namespaces() {
        let policy: BindingArtifactPolicy = serde_json::from_value(serde_json::json!({
            "mode": "allow",
            "names": ["operator.input"],
            "namespaces": ["context.reference"]
        }))
        .unwrap();
        policy.validate().unwrap();
        assert!(policy.admits("operator.input"));
        assert!(policy.admits("context.reference.architecture"));
        assert!(!policy.admits("operator.input.extra"));
        assert!(!policy.admits("context.references"));
    }

    #[test]
    fn bare_interface_string_is_not_a_binding_contract() {
        assert!(
            serde_json::from_value::<AuthoredInterfaceBinding>(serde_json::json!(
                "world_provider_codex"
            ))
            .is_err()
        );
        let binding: AuthoredInterfaceBinding = serde_json::from_value(serde_json::json!({
            "interface": "world_provider_codex",
            "config": {"artifact_policy": {"mode": "all"}}
        }))
        .unwrap();
        binding.validate().unwrap();
        assert!(binding.admits("any.interface.owned.name"));
    }
}
