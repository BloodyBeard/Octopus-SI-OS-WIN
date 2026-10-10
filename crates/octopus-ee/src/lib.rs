use std::collections::{BTreeMap, BTreeSet};

use octopus_descriptors::{
    Descriptor, EeId, EeTemplateId, InterfaceId, MarkId, ToolId, WorkerEnvironmentKindId,
};
use octopus_policy::{ArtifactVerb, CapSet, DenySet, InterfacePolicyOverrideLayer, PolicyLayer};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EeTemplateDescriptor {
    pub id: EeTemplateId,
    pub goal_class: String,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_interfaces: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_worker_environment_kinds: Option<Vec<String>>,
    /// VERIFIERS.md §2 / POLICY.md §5: mark outcomes narrow through the EE chain.
    /// `None` leaves the ceiling unchanged; an empty list denies every outcome.
    #[serde(default)]
    pub allowed_mark_outcomes: Option<Vec<String>>,
    /// POLICY.md §5 / notes2 2026-07-11 — the host root-slots the runs in this EE
    /// may reach at all. `None` = no narrowing on this axis; the EE chain's
    /// intersection is one of the three legs that FORM a worker environment's
    /// root caps (kind boundary ∩ door host-slots ∩ EE chain), never a runtime
    /// literal.
    #[serde(default)]
    pub allowed_root_slots: Option<Vec<String>>,
    /// POLICY.md §4 — the artifact object-level verbs the runs in this EE may
    /// exercise (`None` = no narrowing). Folds into the composed verb ceiling.
    #[serde(default)]
    pub allowed_artifact_verbs: Option<Vec<ArtifactVerb>>,
    #[serde(default)]
    pub hard_blocked_tools: Vec<String>,
    #[serde(default)]
    pub allowed_child_ee_templates: Vec<String>,
    #[serde(default)]
    pub interface_policy_overrides: InterfacePolicyOverrideLayer,
    #[serde(default)]
    pub bootstrap_root: bool,
}

impl Descriptor for EeTemplateDescriptor {
    const KIND: &'static str = "ee_template";
    const SCHEMA_VERSION: u32 = 1;
    const CONTRACT_VERSION: u32 = 1;
    fn declared_name(&self) -> Option<&str> {
        Some(self.id.as_str())
    }
}

impl EeTemplateDescriptor {
    pub fn as_policy_layer(&self) -> PolicyLayer {
        policy_layer(
            &self.allowed_tools,
            &self.allowed_interfaces,
            &self.allowed_worker_environment_kinds,
            &self.allowed_mark_outcomes,
            &self.allowed_artifact_verbs,
            &self.hard_blocked_tools,
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EeInstancePolicyNarrowing {
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_interfaces: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_worker_environment_kinds: Option<Vec<String>>,
    /// VERIFIERS.md §2 / POLICY.md §5: instance restrictions intersect the
    /// template and ancestor allowances, including an explicit empty list.
    #[serde(default)]
    pub allowed_mark_outcomes: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_root_slots: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_artifact_verbs: Option<Vec<ArtifactVerb>>,
    #[serde(default)]
    pub hard_blocked_tools: Vec<String>,
    #[serde(default)]
    pub interface_policy_overrides: InterfacePolicyOverrideLayer,
}

impl EeInstancePolicyNarrowing {
    pub fn as_policy_layer(&self) -> PolicyLayer {
        policy_layer(
            &self.allowed_tools,
            &self.allowed_interfaces,
            &self.allowed_worker_environment_kinds,
            &self.allowed_mark_outcomes,
            &self.allowed_artifact_verbs,
            &self.hard_blocked_tools,
        )
    }

    pub fn is_empty(&self) -> bool {
        self.allowed_tools.is_none()
            && self.allowed_interfaces.is_none()
            && self.allowed_worker_environment_kinds.is_none()
            && self.allowed_mark_outcomes.is_none()
            && self.allowed_root_slots.is_none()
            && self.allowed_artifact_verbs.is_none()
            && self.hard_blocked_tools.is_empty()
            && self.interface_policy_overrides.is_empty()
    }
}

fn policy_layer(
    allowed_tools: &Option<Vec<String>>,
    allowed_interfaces: &Option<Vec<String>>,
    allowed_worker_environment_kinds: &Option<Vec<String>>,
    allowed_mark_outcomes: &Option<Vec<String>>,
    allowed_artifact_verbs: &Option<Vec<ArtifactVerb>>,
    hard_blocked_tools: &[String],
) -> PolicyLayer {
    fn axis<T: Ord + Clone, F: Fn(&str) -> T>(
        items: &Option<Vec<String>>,
        make: F,
    ) -> Option<CapSet<T>> {
        items
            .as_ref()
            .map(|list| CapSet::from_allowed(list.iter().map(|s| make(s.as_str()))))
    }
    PolicyLayer {
        tools: axis(allowed_tools, |value| ToolId::new(value)),
        interfaces: axis(allowed_interfaces, |value| InterfaceId::new(value)),
        wek: axis(allowed_worker_environment_kinds, |value| {
            WorkerEnvironmentKindId::new(value)
        }),
        mark_outcomes: axis(allowed_mark_outcomes, |value| MarkId::new(value)),
        hard_blocks_tools: DenySet::from_denied(hard_blocked_tools.iter().map(ToolId::new)),
        artifact_verbs: allowed_artifact_verbs
            .as_ref()
            .map(|verbs| CapSet::from_allowed(verbs.iter().copied())),
        artifact_access: BTreeMap::new(),
    }
}

/// The host root-slots a run may reach, composed down an EE chain (POLICY.md §5,
/// notes2 2026-07-11 #3). `None` = unconstrained on this axis; `Some(set)` = only
/// these slots. Intersection narrows strictly inward, exactly like every other
/// policy axis.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EeRootSlotAllowance {
    allowed: Option<BTreeSet<String>>,
}

impl EeRootSlotAllowance {
    pub fn unconstrained() -> Self {
        Self { allowed: None }
    }

    fn narrow_with(&mut self, list: &[String]) {
        let incoming: BTreeSet<String> = list.iter().cloned().collect();
        self.allowed = Some(match self.allowed.take() {
            None => incoming,
            Some(existing) => existing.intersection(&incoming).cloned().collect(),
        });
    }

    pub fn allowed(&self) -> Option<&BTreeSet<String>> {
        self.allowed.as_ref()
    }

    pub fn admits(&self, slot: &str) -> bool {
        match &self.allowed {
            None => true,
            Some(set) => set.contains(slot),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EeInstance {
    pub ee_id: EeId,
    #[serde(rename = "template_name")]
    pub template: EeTemplateId,
    #[serde(rename = "parent_ee_id")]
    pub parent_ee: Option<EeId>,
    pub goal: String,
    #[serde(default)]
    pub config: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "EeInstancePolicyNarrowing::is_empty")]
    pub policy_narrowing: EeInstancePolicyNarrowing,
}

#[derive(Debug, Error)]
pub enum EeError {
    #[error("unknown EE instance: {0}")]
    UnknownEe(EeId),
    #[error("unknown EE template: {0}")]
    UnknownTemplate(EeTemplateId),
    #[error("EE nesting cycle detected at: {0}")]
    Cycle(EeId),
    #[error(
        "ee_template {parent:?} may not open child ee_template {child:?} (not in allowed_child_ee_templates)"
    )]
    ChildTemplateNotAllowed { parent: String, child: String },
    #[error("EE instance {0} already exists (open requires a fresh scope id)")]
    DuplicateEe(EeId),
    #[error("persisted EE instance {0} has no parent EE")]
    MissingParent(EeId),
    #[error("persisted EE instance {0} has an empty goal")]
    EmptyGoal(EeId),
}

pub fn bootstrap_ee_instance_id(template: &EeTemplateDescriptor) -> EeId {
    EeId::new(format!("ee_{}", template.id.as_str()))
}

#[derive(Default, Clone, Debug)]
pub struct EeTree {
    instances: BTreeMap<EeId, EeInstance>,
    templates: BTreeMap<EeTemplateId, EeTemplateDescriptor>,
}

impl EeTree {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert_template(&mut self, t: EeTemplateDescriptor) {
        self.templates.insert(t.id.clone(), t);
    }
    pub fn insert_instance(&mut self, e: EeInstance) {
        self.instances.insert(e.ee_id.clone(), e);
    }
    pub fn insert_persisted_instance(&mut self, instance: EeInstance) -> Result<(), EeError> {
        if self.instances.contains_key(&instance.ee_id) {
            return Err(EeError::DuplicateEe(instance.ee_id));
        }
        if instance.goal.trim().is_empty() {
            return Err(EeError::EmptyGoal(instance.ee_id));
        }
        if !self.templates.contains_key(&instance.template) {
            return Err(EeError::UnknownTemplate(instance.template));
        }
        let parent_ee = instance
            .parent_ee
            .as_ref()
            .ok_or_else(|| EeError::MissingParent(instance.ee_id.clone()))?;
        let parent = self
            .instances
            .get(parent_ee)
            .ok_or_else(|| EeError::UnknownEe(parent_ee.clone()))?;
        let parent_template = self
            .templates
            .get(&parent.template)
            .ok_or_else(|| EeError::UnknownTemplate(parent.template.clone()))?;
        if !parent_template
            .allowed_child_ee_templates
            .iter()
            .any(|allowed| allowed.as_str() == instance.template.as_str())
        {
            return Err(EeError::ChildTemplateNotAllowed {
                parent: parent_template.id.as_str().to_string(),
                child: instance.template.as_str().to_string(),
            });
        }
        self.instances.insert(instance.ee_id.clone(), instance);
        Ok(())
    }
    pub fn get(&self, id: &EeId) -> Option<&EeInstance> {
        self.instances.get(id)
    }
    pub fn instances(&self) -> impl Iterator<Item = (&EeId, &EeInstance)> {
        self.instances.iter()
    }
    pub fn len(&self) -> usize {
        self.instances.len()
    }
    pub fn is_empty(&self) -> bool {
        self.instances.is_empty()
    }

    pub fn open_child_scope(
        &mut self,
        ee_id: EeId,
        child_template: EeTemplateId,
        parent_ee: &EeId,
        goal: String,
    ) -> Result<EeInstance, EeError> {
        let parent_inst = self
            .instances
            .get(parent_ee)
            .ok_or_else(|| EeError::UnknownEe(parent_ee.clone()))?;
        if !self.templates.contains_key(&child_template) {
            return Err(EeError::UnknownTemplate(child_template));
        }
        let parent_tmpl = self
            .templates
            .get(&parent_inst.template)
            .ok_or_else(|| EeError::UnknownTemplate(parent_inst.template.clone()))?;
        if !parent_tmpl
            .allowed_child_ee_templates
            .iter()
            .any(|c| c.as_str() == child_template.as_str())
        {
            return Err(EeError::ChildTemplateNotAllowed {
                parent: parent_tmpl.id.as_str().to_string(),
                child: child_template.as_str().to_string(),
            });
        }
        if self.instances.contains_key(&ee_id) {
            return Err(EeError::DuplicateEe(ee_id));
        }
        let inst = EeInstance {
            ee_id: ee_id.clone(),
            template: child_template,
            parent_ee: Some(parent_ee.clone()),
            goal,
            config: BTreeMap::new(),
            policy_narrowing: EeInstancePolicyNarrowing::default(),
        };
        self.instances.insert(ee_id, inst.clone());
        Ok(inst)
    }

    pub fn ancestors(&self, ee_id: &EeId) -> Result<Vec<EeId>, EeError> {
        let mut chain = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut cur = Some(ee_id.clone());
        while let Some(id) = cur {
            if !seen.insert(id.clone()) {
                return Err(EeError::Cycle(id));
            }
            let inst = self
                .instances
                .get(&id)
                .ok_or_else(|| EeError::UnknownEe(id.clone()))?;
            chain.push(id.clone());
            cur = inst.parent_ee.clone();
        }
        chain.reverse();
        Ok(chain)
    }

    pub fn policy_layers(&self, ee_id: &EeId) -> Result<Vec<PolicyLayer>, EeError> {
        self.policy_layers_omitting_instance_narrowing(ee_id, None)
    }

    pub fn policy_layers_omitting_instance_narrowing(
        &self,
        ee_id: &EeId,
        omitted: Option<&EeId>,
    ) -> Result<Vec<PolicyLayer>, EeError> {
        let chain = self.ancestors(ee_id)?;
        let mut layers = Vec::with_capacity(chain.len() * 2);
        for id in chain {
            let inst = self
                .instances
                .get(&id)
                .expect("ancestors validated presence");
            let tmpl = self
                .templates
                .get(&inst.template)
                .ok_or_else(|| EeError::UnknownTemplate(inst.template.clone()))?;
            layers.push(tmpl.as_policy_layer());
            if omitted != Some(&id) {
                layers.push(inst.policy_narrowing.as_policy_layer());
            }
        }
        Ok(layers)
    }

    /// Fold the `allowed_root_slots` axis down the run's EE chain into one
    /// allowance — the EE-chain leg of worker-environment root-cap formation
    /// (notes2 2026-07-11 #3: caps are FORMED from kind boundary ∩ door host-slots
    /// ∩ EE chain, never a runtime literal).
    pub fn root_slot_allowance(&self, ee_id: &EeId) -> Result<EeRootSlotAllowance, EeError> {
        self.root_slot_allowance_omitting_instance_narrowing(ee_id, None)
    }

    pub fn root_slot_allowance_omitting_instance_narrowing(
        &self,
        ee_id: &EeId,
        omitted: Option<&EeId>,
    ) -> Result<EeRootSlotAllowance, EeError> {
        let chain = self.ancestors(ee_id)?;
        let mut allowance = EeRootSlotAllowance::unconstrained();
        for id in chain {
            let inst = self
                .instances
                .get(&id)
                .expect("ancestors validated presence");
            let tmpl = self
                .templates
                .get(&inst.template)
                .ok_or_else(|| EeError::UnknownTemplate(inst.template.clone()))?;
            if let Some(list) = &tmpl.allowed_root_slots {
                allowance.narrow_with(list);
            }
            if omitted != Some(&id) {
                if let Some(list) = &inst.policy_narrowing.allowed_root_slots {
                    allowance.narrow_with(list);
                }
            }
        }
        Ok(allowance)
    }

    pub fn interface_policy_override_layers(
        &self,
        ee_id: &EeId,
    ) -> Result<Vec<InterfacePolicyOverrideLayer>, EeError> {
        let chain = self.ancestors(ee_id)?;
        let mut layers = Vec::with_capacity(chain.len() * 2);
        for id in chain {
            let instance = self
                .instances
                .get(&id)
                .expect("ancestors validated presence");
            let template = self
                .templates
                .get(&instance.template)
                .ok_or_else(|| EeError::UnknownTemplate(instance.template.clone()))?;
            layers.push(template.interface_policy_overrides.clone());
            layers.push(instance.policy_narrowing.interface_policy_overrides.clone());
        }
        Ok(layers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_policy::{CapSet, ContextGrant, DenySet, FormationPin, form_policy};

    #[test]
    fn real_registry_ee_templates_parse() {
        use octopus_descriptors::loader;
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/ee_template");
        let loaded = loader::load_kind::<EeTemplateDescriptor>(&root)
            .expect("registry/ee_template must parse");
        let bootstrap_roots: Vec<_> = loaded.iter().filter(|(_, d)| d.bootstrap_root).collect();
        assert_eq!(
            bootstrap_roots.len(),
            1,
            "registry/ee_template must declare exactly one bootstrap root"
        );

        let templates: Vec<EeTemplateDescriptor> = loaded.iter().map(|(_, d)| d.clone()).collect();
        let bootstrap_root = &bootstrap_roots[0].1;
        assert!(
            bootstrap_root
                .allowed_child_ee_templates
                .iter()
                .any(|c| c == "operator_interaction")
        );
        assert!(
            templates
                .iter()
                .any(|d| d.id == EeTemplateId::new("operator_interaction"))
        );
    }

    #[test]
    fn ee_instance_config_is_opaque_object_data() {
        let legacy_empty: EeInstance = serde_json::from_value(serde_json::json!({
            "ee_id": "ee_scope",
            "template_name": "existence",
            "parent_ee_id": null,
            "goal": "exist"
        }))
        .unwrap();
        assert!(legacy_empty.config.is_empty());

        let configured: EeInstance = serde_json::from_value(serde_json::json!({
            "ee_id": "ee_scope",
            "template_name": "existence",
            "parent_ee_id": null,
            "goal": "exist",
            "config": {"attention": {"mode": "quiet"}, "weight": 3}
        }))
        .unwrap();
        assert_eq!(configured.config["attention"]["mode"], "quiet");
        assert!(
            serde_json::from_value::<EeInstance>(serde_json::json!({
                "ee_id": "ee_scope",
                "template_name": "existence",
                "parent_ee_id": null,
                "goal": "exist",
                "config": []
            }))
            .is_err()
        );
    }

    fn ceiling(tools: &[&str]) -> ContextGrant {
        ContextGrant {
            tools: CapSet::from_allowed(tools.iter().map(|s| ToolId::new(*s))),
            interfaces: CapSet::from_allowed(std::iter::empty::<InterfaceId>()),
            wek: CapSet::from_allowed(std::iter::empty::<WorkerEnvironmentKindId>()),
            mark_outcomes: CapSet::from_allowed(["a", "b", "c"].map(MarkId::new)),
            hard_blocks_tools: DenySet::default(),
            interface_policy: Default::default(),
            ..Default::default()
        }
    }

    fn tmpl(id: &str, tools: &[&str]) -> EeTemplateDescriptor {
        EeTemplateDescriptor {
            id: EeTemplateId::new(id),
            goal_class: id.into(),
            allowed_tools: Some(tools.iter().map(|s| s.to_string()).collect()),
            allowed_interfaces: None,
            allowed_worker_environment_kinds: None,
            allowed_mark_outcomes: None,
            allowed_root_slots: None,
            allowed_artifact_verbs: None,
            hard_blocked_tools: vec![],
            allowed_child_ee_templates: vec![],
            interface_policy_overrides: Default::default(),
            bootstrap_root: false,
        }
    }

    #[test]
    fn nested_ee_chain_narrows_inward_through_the_policy_fold() {
        let mut tree = EeTree::new();
        let mut root = tmpl("root_scope", &["a", "b", "c"]);
        root.allowed_mark_outcomes = Some(vec!["b".into(), "c".into()]);
        tree.insert_template(root);
        tree.insert_template(tmpl("self_improve", &["b", "c"]));
        tree.insert_instance(EeInstance {
            ee_id: EeId::new("ee_root"),
            template: EeTemplateId::new("root_scope"),
            parent_ee: None,
            goal: "exist".into(),
            config: BTreeMap::new(),
            policy_narrowing: EeInstancePolicyNarrowing::default(),
        });
        tree.insert_instance(EeInstance {
            ee_id: EeId::new("ee_child"),
            template: EeTemplateId::new("self_improve"),
            parent_ee: Some(EeId::new("ee_root")),
            goal: "improve".into(),
            config: BTreeMap::new(),
            policy_narrowing: EeInstancePolicyNarrowing {
                allowed_tools: Some(vec!["c".into(), "d".into()]),
                allowed_mark_outcomes: Some(vec!["a".into(), "c".into(), "d".into()]),
                ..Default::default()
            },
        });

        let chain = tree.ancestors(&EeId::new("ee_child")).unwrap();
        assert_eq!(
            chain,
            vec![EeId::new("ee_root"), EeId::new("ee_child")],
            "root→leaf order"
        );

        let layers = tree.policy_layers(&EeId::new("ee_child")).unwrap();
        let formed = form_policy(
            ceiling(&["a", "b", "c", "d"]),
            &layers,
            &[],
            FormationPin::default(),
        );
        let marks = CapSet::from_allowed([MarkId::new("c")]);
        assert_eq!(formed.context.mark_outcomes, marks);
        assert_eq!(
            formed.context.tools,
            CapSet::from_allowed([ToolId::new("c")]),
            "the instance narrows its template and cannot widen it with tool d"
        );
        assert!(
            formed.context.tools.is_subset(&formed.boundary.tools),
            "child ⊆ ceiling"
        );

        assert!(tree.ancestors(&EeId::new("nope")).is_err());
    }

    #[test]
    fn unspecified_axis_is_none_not_deny_all() {
        let t = tmpl("scoped", &["a"]);
        let layer = t.as_policy_layer();
        assert!(
            layer.tools.is_some(),
            "a declared allow-list narrows its axis"
        );
        assert!(
            layer.interfaces.is_none() && layer.wek.is_none(),
            "an UNSPECIFIED axis is `None` (no narrowing), never `Some(empty)` (deny-all)"
        );

        let ceiling = ContextGrant {
            tools: CapSet::from_allowed([ToolId::new("a"), ToolId::new("z")]),
            interfaces: CapSet::from_allowed([InterfaceId::new("system_fixture_episode")]),
            wek: CapSet::from_allowed([WorkerEnvironmentKindId::new("shell")]),
            mark_outcomes: CapSet::from_allowed(std::iter::empty::<octopus_descriptors::MarkId>()),
            hard_blocks_tools: DenySet::default(),
            interface_policy: Default::default(),
            ..Default::default()
        };
        let formed = form_policy(ceiling, &[layer], &[], FormationPin::default());
        assert_eq!(
            formed.context.tools,
            CapSet::from_allowed([ToolId::new("a")]),
            "the DECLARED tool axis still narrows to the intersection (no widening)"
        );
        assert_eq!(
            formed.context.interfaces,
            CapSet::from_allowed([InterfaceId::new("system_fixture_episode")]),
            "an unspecified interfaces axis must NOT be zeroed by the layer"
        );
        assert_eq!(
            formed.context.wek,
            CapSet::from_allowed([WorkerEnvironmentKindId::new("shell")]),
            "an unspecified wek axis must NOT be zeroed by the layer"
        );
    }

    #[test]
    fn three_axis_semantics_absent_vs_empty_vs_listed_are_distinct() {
        let ceiling = || ContextGrant {
            tools: CapSet::from_allowed(std::iter::empty::<ToolId>()),
            interfaces: CapSet::from_allowed([
                InterfaceId::new("system_fixture_episode"),
                InterfaceId::new("world_web_search"),
            ]),
            wek: CapSet::from_allowed(std::iter::empty::<WorkerEnvironmentKindId>()),
            mark_outcomes: CapSet::from_allowed(std::iter::empty::<octopus_descriptors::MarkId>()),
            hard_blocks_tools: DenySet::default(),
            interface_policy: Default::default(),
            ..Default::default()
        };
        let mut base = tmpl("scoped", &[]);

        base.allowed_interfaces = None;
        let layer = base.as_policy_layer();
        assert!(
            layer.interfaces.is_none(),
            "absent axis ⇒ None layer (no narrowing)"
        );
        let formed = form_policy(ceiling(), &[layer], &[], FormationPin::default());
        assert_eq!(
            formed.context.interfaces,
            CapSet::from_allowed([
                InterfaceId::new("system_fixture_episode"),
                InterfaceId::new("world_web_search"),
            ]),
            "an absent interfaces axis must leave the ceiling untouched"
        );

        base.allowed_interfaces = Some(vec![]);
        let layer = base.as_policy_layer();
        let denied = layer
            .interfaces
            .as_ref()
            .expect("explicit [] is Some(empty), not None");
        assert!(
            denied.is_empty(),
            "Some([]) projects to an empty CapSet (deny-all)"
        );
        let formed = form_policy(ceiling(), &[layer], &[], FormationPin::default());
        assert!(
            formed.context.interfaces.is_empty(),
            "an explicit empty interfaces axis denies ALL — the ceiling is intersected to nothing"
        );

        base.allowed_interfaces = Some(vec!["world_web_search".into()]);
        let layer = base.as_policy_layer();
        let formed = form_policy(ceiling(), &[layer], &[], FormationPin::default());
        assert_eq!(
            formed.context.interfaces,
            CapSet::from_allowed([InterfaceId::new("world_web_search")]),
            "a one-element allow-list narrows to exactly that interface (intersect-only)"
        );
    }

    #[test]
    fn serde_distinguishes_absent_empty_and_listed() {
        let absent: EeTemplateDescriptor = serde_json::from_str(r#"{"id":"x","goal_class":"x"}"#)
            .expect("a minimal ee_template (all axes absent) parses");
        assert_eq!(
            absent.allowed_tools, None,
            "absent key ⇒ None (no narrowing)"
        );
        assert_eq!(absent.allowed_interfaces, None);

        let empty: EeTemplateDescriptor =
            serde_json::from_str(r#"{"id":"x","goal_class":"x","allowed_tools":[]}"#)
                .expect("an explicit empty allow-list parses");
        assert_eq!(
            empty.allowed_tools,
            Some(vec![]),
            "[] ⇒ Some(empty) (deny-all)"
        );

        let listed: EeTemplateDescriptor =
            serde_json::from_str(r#"{"id":"x","goal_class":"x","allowed_tools":["t.read"]}"#)
                .expect("a populated allow-list parses");
        assert_eq!(
            listed.allowed_tools,
            Some(vec!["t.read".to_string()]),
            "[..] ⇒ Some([..])"
        );
    }

    #[test]
    fn root_slot_allowance_narrows_inward_down_the_ee_chain() {
        let mut tree = EeTree::new();
        let mut root = tmpl("root_scope", &["a"]);
        root.allowed_root_slots = Some(vec![
            "repo_root".into(),
            "worktree_root".into(),
            "run_records_root".into(),
        ]);
        root.allowed_child_ee_templates = vec!["narrow".into()];
        let mut child = tmpl("narrow", &["a"]);
        child.allowed_root_slots = Some(vec!["repo_root".into(), "worktree_root".into()]);
        tree.insert_template(root);
        tree.insert_template(child);
        tree.insert_instance(EeInstance {
            ee_id: EeId::new("ee_root"),
            template: EeTemplateId::new("root_scope"),
            parent_ee: None,
            goal: "x".into(),
            config: BTreeMap::new(),
            policy_narrowing: EeInstancePolicyNarrowing::default(),
        });
        tree.open_child_scope(
            EeId::new("ee_child"),
            EeTemplateId::new("narrow"),
            &EeId::new("ee_root"),
            "narrow".into(),
        )
        .unwrap();

        let allowance = tree.root_slot_allowance(&EeId::new("ee_child")).unwrap();
        assert!(allowance.admits("repo_root") && allowance.admits("worktree_root"));
        assert!(
            !allowance.admits("run_records_root"),
            "the child EE narrowed away a root slot its parent allowed"
        );
        assert_eq!(allowance.allowed().unwrap().len(), 2);

        let mut open_tree = EeTree::new();
        open_tree.insert_template(tmpl("open_scope", &["a"]));
        open_tree.insert_instance(EeInstance {
            ee_id: EeId::new("ee_open"),
            template: EeTemplateId::new("open_scope"),
            parent_ee: None,
            goal: "x".into(),
            config: BTreeMap::new(),
            policy_narrowing: EeInstancePolicyNarrowing::default(),
        });
        let open = open_tree
            .root_slot_allowance(&EeId::new("ee_open"))
            .unwrap();
        assert!(
            open.admits("anything") && open.allowed().is_none(),
            "an EE that declares no allowed_root_slots is unconstrained on the roots axis"
        );
    }

    #[test]
    fn open_child_respects_the_emergence_boundary_and_narrows_inward() {
        let mut tree = EeTree::new();
        let mut root_scope = tmpl("root_scope", &["a", "b", "c"]);
        root_scope.allowed_child_ee_templates = vec!["conversation".into()];
        let conversation = tmpl("conversation", &["b"]);
        tree.insert_template(root_scope);
        tree.insert_template(conversation);
        let root = EeId::new("ee_root");
        tree.insert_instance(EeInstance {
            ee_id: root.clone(),
            template: EeTemplateId::new("root_scope"),
            parent_ee: None,
            goal: "exist".into(),
            config: BTreeMap::new(),
            policy_narrowing: EeInstancePolicyNarrowing::default(),
        });

        let before = tree.ancestors(&root).unwrap().len();
        let child = tree
            .open_child_scope(
                EeId::new("ee_conv_1"),
                EeTemplateId::new("conversation"),
                &root,
                "talk to user 1".into(),
            )
            .expect(
                "the root scope may open a conversation child (it is in the emergence boundary)",
            );
        assert_eq!(
            child.parent_ee.as_ref(),
            Some(&root),
            "the child is parented under the root scope"
        );
        let chain = tree.ancestors(&EeId::new("ee_conv_1")).unwrap();
        assert_eq!(
            chain,
            vec![root.clone(), EeId::new("ee_conv_1")],
            "root→child nesting"
        );
        assert_eq!(
            chain.len(),
            before + 1,
            "the EE tree grew by exactly one scope"
        );

        let layers = tree.policy_layers(&EeId::new("ee_conv_1")).unwrap();
        let formed = form_policy(
            ceiling(&["a", "b", "c", "d"]),
            &layers,
            &[],
            FormationPin::default(),
        );
        assert_eq!(
            formed.context.tools,
            CapSet::from_allowed([ToolId::new("b")]),
            "the opened child's grant = ceiling ∩ root_scope ∩ conversation = {{b}} (narrowed inward)"
        );

        let child2 = tree
            .open_child_scope(
                EeId::new("ee_conv_2"),
                EeTemplateId::new("conversation"),
                &root,
                "talk to user 2".into(),
            )
            .expect("a second conversation child opens as a sibling under the root scope");
        assert_eq!(
            child2.parent_ee.as_ref(),
            Some(&root),
            "the second child is also under the root scope"
        );
        assert_ne!(
            child.ee_id, child2.ee_id,
            "two distinct parallel conversation scopes"
        );
    }

    #[test]
    fn open_child_fails_closed_on_boundary_phantom_unknown_and_duplicate() {
        let mut tree = EeTree::new();
        let mut root_scope = tmpl("root_scope", &["a"]);
        root_scope.allowed_child_ee_templates = vec!["conversation".into()];
        let conversation = tmpl("conversation", &[]);
        let widening = tmpl("widening_scope", &["a"]);
        tree.insert_template(root_scope);
        tree.insert_template(conversation);
        tree.insert_template(widening);
        let root = EeId::new("ee_root");
        tree.insert_instance(EeInstance {
            ee_id: root.clone(),
            template: EeTemplateId::new("root_scope"),
            parent_ee: None,
            goal: "exist".into(),
            config: BTreeMap::new(),
            policy_narrowing: EeInstancePolicyNarrowing::default(),
        });

        let err = tree
            .open_child_scope(
                EeId::new("ee_widen"),
                EeTemplateId::new("widening_scope"),
                &root,
                "widen".into(),
            )
            .unwrap_err();
        assert!(
            matches!(err, EeError::ChildTemplateNotAllowed { ref child, .. } if child == "widening_scope"),
            "opening a child the parent never declared widens its scope-creation grant — fail closed, got {err:?}"
        );
        assert!(
            tree.get(&EeId::new("ee_widen")).is_none(),
            "the refused child was NOT inserted"
        );

        let err = tree
            .open_child_scope(
                EeId::new("ee_phantom"),
                EeTemplateId::new("no_such_template"),
                &root,
                "phantom".into(),
            )
            .unwrap_err();
        assert!(
            matches!(err, EeError::UnknownTemplate(ref t) if t.as_str() == "no_such_template"),
            "a phantom child template fails closed, got {err:?}"
        );

        let err = tree
            .open_child_scope(
                EeId::new("ee_orphan"),
                EeTemplateId::new("conversation"),
                &EeId::new("ee_nonexistent_parent"),
                "orphan".into(),
            )
            .unwrap_err();
        assert!(
            matches!(err, EeError::UnknownEe(_)),
            "an unknown parent fails closed, got {err:?}"
        );

        tree.open_child_scope(
            EeId::new("ee_conv"),
            EeTemplateId::new("conversation"),
            &root,
            "talk".into(),
        )
        .expect("first open ok");
        let err = tree
            .open_child_scope(
                EeId::new("ee_conv"),
                EeTemplateId::new("conversation"),
                &root,
                "talk again".into(),
            )
            .unwrap_err();
        assert!(
            matches!(err, EeError::DuplicateEe(ref id) if id.as_str() == "ee_conv"),
            "re-using a live scope id fails closed, got {err:?}"
        );
    }
}
