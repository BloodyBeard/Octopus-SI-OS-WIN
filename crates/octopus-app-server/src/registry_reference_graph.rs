use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use octopus_descriptors::{ArtifactId, Descriptor, DescriptorHash, DescriptorName, InterfaceId};
use octopus_ee::EeTemplateDescriptor;
use octopus_interfaces::{
    InterfaceDescriptor, InterfaceRuntimeDisposition, form_interface_runtime_disposition,
    scan_worker_interface_policies,
};
use octopus_marks::MarkDescriptor;
use octopus_policy::WorkerInterfacePolicy;
use octopus_tool_registry::ToolDescriptor;
use octopus_verifiers::VerifierDescriptor;
use octopus_worker_env::{
    WorkerEnvKindSpec, WorkerEnvironmentCeilingView, WorkerEnvironmentRegistrySubject,
    WorkerEnvironmentToolView, worker_environment_registry_issues,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RegistryNodeKind {
    Interface,
    Tool,
    WorkerEnvironmentKind,
    EeTemplate,
    Mark,
    Verifier,
    Artifact,
}

impl RegistryNodeKind {
    fn label(self) -> &'static str {
        match self {
            Self::Interface => "interface",
            Self::Tool => "tool",
            Self::WorkerEnvironmentKind => "worker_environment_kind",
            Self::EeTemplate => "ee_template",
            Self::Mark => "mark",
            Self::Verifier => "verifier",
            Self::Artifact => "artifact",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RegistryNode {
    pub(crate) kind: RegistryNodeKind,
    pub(crate) id: String,
}

impl RegistryNode {
    pub(crate) fn interface(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::Interface, id)
    }

    pub(crate) fn tool(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::Tool, id)
    }

    pub(crate) fn wek(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::WorkerEnvironmentKind, id)
    }

    pub(crate) fn ee_template(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::EeTemplate, id)
    }

    pub(crate) fn mark(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::Mark, id)
    }

    pub(crate) fn verifier(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::Verifier, id)
    }

    pub(crate) fn artifact(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::Artifact, id)
    }

    fn new(kind: RegistryNodeKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

impl std::fmt::Display for RegistryNode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.kind.label(), self.id)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RegistryReference {
    pub(crate) source: RegistryNode,
    pub(crate) field: String,
    pub(crate) target: RegistryNode,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RegistryReferenceGraph {
    available: BTreeSet<RegistryNode>,
    unavailable: BTreeMap<RegistryNode, String>,
    edges: BTreeMap<RegistryNode, Vec<RegistryReference>>,
    artifact_descriptor_hashes: BTreeMap<ArtifactId, DescriptorHash>,
    artifact_descriptor_bytes: BTreeMap<ArtifactId, Vec<u8>>,
}

impl RegistryReferenceGraph {
    fn register(&mut self, node: RegistryNode) {
        if !self.unavailable.contains_key(&node) {
            self.available.insert(node);
        }
    }

    fn refuse(&mut self, node: RegistryNode, reason: impl Into<String>) {
        self.available.remove(&node);
        self.unavailable.insert(node, reason.into());
    }

    fn link(&mut self, source: RegistryNode, field: impl Into<String>, target: RegistryNode) {
        self.edges
            .entry(source.clone())
            .or_default()
            .push(RegistryReference {
                source,
                field: field.into(),
                target,
            });
    }

    pub(crate) fn is_available(&self, node: &RegistryNode) -> bool {
        self.available.contains(node) && !self.unavailable.contains_key(node)
    }

    pub(crate) fn artifact_descriptor_hash(
        &self,
        artifact: &ArtifactId,
    ) -> Result<DescriptorHash, String> {
        let node = RegistryNode::artifact(artifact.as_str());
        if let Some(reason) = self.unavailable.get(&node) {
            return Err(format!("unavailable {node}: {reason}"));
        }
        if !self.available.contains(&node) {
            return Err(format!("missing {node}"));
        }
        self.artifact_descriptor_hashes
            .get(artifact)
            .copied()
            .ok_or_else(|| format!("available {node} has no loaded descriptor hash"))
    }

    pub(crate) fn artifact_descriptor_bytes(&self, artifact: &ArtifactId) -> Result<&[u8], String> {
        self.artifact_descriptor_hash(artifact)?;
        self.artifact_descriptor_bytes
            .get(artifact)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("artifact '{}' has no loaded descriptor bytes", artifact.as_str()))
    }

    pub(crate) fn validate_active_references(
        &self,
        source: &str,
        references: impl IntoIterator<Item = (String, RegistryNode)>,
    ) -> Result<BTreeSet<RegistryNode>, String> {
        let mut closure = BTreeSet::new();
        for (field, root) in references {
            let mut pending = vec![ReferenceVisit {
                node: &root,
                via: None,
            }];
            let mut queued = BTreeSet::from([&root]);
            let mut cursor = 0;
            while cursor < pending.len() {
                let node = pending[cursor].node;
                if !closure.contains(node) {
                    let unavailable = self.unavailable.get(node);
                    if unavailable.is_some() || !self.available.contains(node) {
                        if node.kind != RegistryNodeKind::Interface {
                            let path =
                                reference_visit_path(source, &field, &root, &pending, cursor);
                            return Err(match unavailable {
                                Some(reason) => format!(
                                    "active registry reference closure reached unavailable {node}: {reason}; path: {path}"
                                ),
                                None => format!(
                                    "active registry reference closure reached missing {node}; path: {path}"
                                ),
                            });
                        }
                    } else {
                        closure.insert(node.clone());
                        for edge in self.edges.get(node).into_iter().flatten() {
                            if !closure.contains(&edge.target) && queued.insert(&edge.target) {
                                pending.push(ReferenceVisit {
                                    node: &edge.target,
                                    via: Some((cursor, edge)),
                                });
                            }
                        }
                    }
                }
                cursor += 1;
            }
        }
        Ok(closure)
    }
}

struct ReferenceVisit<'a> {
    node: &'a RegistryNode,
    via: Option<(usize, &'a RegistryReference)>,
}

fn reference_visit_path(
    source: &str,
    field: &str,
    root: &RegistryNode,
    visits: &[ReferenceVisit<'_>],
    mut cursor: usize,
) -> String {
    let mut path = Vec::new();
    while let Some((parent, edge)) = visits[cursor].via {
        path.push(format!("{}.{} -> {}", edge.source, edge.field, edge.target));
        cursor = parent;
    }
    path.push(format!("{source}.{field} -> {root}"));
    path.reverse();
    path.join(" -> ")
}

#[derive(Clone, Debug)]
pub(crate) struct ActivationReferenceSet {
    pub(crate) source: String,
    pub(crate) references: Vec<(String, RegistryNode)>,
}

#[derive(Clone, Debug)]
pub(crate) struct DanglingCeilingRef {
    pub(crate) worker: String,
    pub(crate) field: String,
    pub(crate) target: RegistryNode,
}

#[derive(Clone, Debug)]
pub(crate) struct RegistryPackages {
    pub(crate) interfaces: Vec<(DescriptorName, InterfaceDescriptor)>,
    pub(crate) interface_dispositions: BTreeMap<InterfaceId, InterfaceRuntimeDisposition>,
    pub(crate) worker_policies: BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    pub(crate) tools: Vec<(DescriptorName, ToolDescriptor)>,
    pub(crate) worker_environment_kinds: Vec<(DescriptorName, WorkerEnvKindSpec)>,
    pub(crate) ee_templates: Vec<(DescriptorName, EeTemplateDescriptor)>,
    pub(crate) graph: RegistryReferenceGraph,
    pub(crate) activations: Vec<ActivationReferenceSet>,
    pub(crate) dangling_ceiling_refs: Vec<DanglingCeilingRef>,
}

struct KindLoad<T> {
    loaded: Vec<(DescriptorName, T)>,
    unavailable: BTreeMap<RegistryNode, String>,
}

impl RegistryPackages {
    pub(crate) fn load(registry_root: &Path) -> Result<Self, String> {
        let mut interface_load = load_kind::<InterfaceDescriptor>(
            &registry_root.join("interface"),
            RegistryNodeKind::Interface,
            false,
        )?;
        let tool_load = load_kind::<ToolDescriptor>(
            &registry_root.join("tool"),
            RegistryNodeKind::Tool,
            false,
        )?;
        let wek_load = load_kind::<WorkerEnvKindSpec>(
            &registry_root.join("worker_environment_kind"),
            RegistryNodeKind::WorkerEnvironmentKind,
            false,
        )?;
        let ee_load = load_kind::<EeTemplateDescriptor>(
            &registry_root.join("ee_template"),
            RegistryNodeKind::EeTemplate,
            false,
        )?;
        let mark_load =
            load_kind::<MarkDescriptor>(&registry_root.join("mark"), RegistryNodeKind::Mark, true)?;
        let verifier_load = load_kind::<VerifierDescriptor>(
            &registry_root.join("verifier"),
            RegistryNodeKind::Verifier,
            true,
        )?;

        let mut graph = RegistryReferenceGraph::default();
        for unavailable in [
            &interface_load.unavailable,
            &tool_load.unavailable,
            &wek_load.unavailable,
            &ee_load.unavailable,
            &mark_load.unavailable,
            &verifier_load.unavailable,
        ] {
            for (node, reason) in unavailable {
                graph.refuse(node.clone(), reason.clone());
            }
        }

        let policy_scan = scan_worker_interface_policies(
            &registry_root.join("interface"),
            &interface_load.loaded,
        );
        for (interface, reason) in policy_scan.refusals {
            graph.refuse(RegistryNode::interface(interface.as_str()), reason);
        }
        let worker_policies = policy_scan.policies;

        for (_, interface) in &interface_load.loaded {
            graph.register(RegistryNode::interface(interface.id.as_str()));
        }
        for (_, tool) in &tool_load.loaded {
            graph.register(RegistryNode::tool(tool.id.as_str()));
        }
        for (_, wek) in &wek_load.loaded {
            graph.register(RegistryNode::wek(wek.worker_environment_kind_id.as_str()));
        }
        for (_, template) in &ee_load.loaded {
            graph.register(RegistryNode::ee_template(template.id.as_str()));
        }
        for (_, mark) in &mark_load.loaded {
            graph.register(RegistryNode::mark(mark.id.as_str()));
        }
        for (_, verifier) in &verifier_load.loaded {
            graph.register(RegistryNode::verifier(verifier.id.as_str()));
        }
        load_artifacts(registry_root, &mut graph)?;

        link_tools(&tool_load.loaded, &mut graph);
        let interface_dispositions = form_interface_dispositions(
            registry_root,
            &interface_load.loaded,
            &worker_policies,
            &mut graph,
        );
        refuse_cross_package_mismatches(
            &worker_policies,
            &tool_load.loaded,
            &wek_load.loaded,
            &interface_load.loaded,
            &interface_dispositions,
            &mut graph,
        );
        let dangling_ceiling_refs = link_worker_ceilings(&worker_policies, &mut graph);
        let activations =
            load_activation_references(registry_root, &interface_load.loaded, &mut graph)?;

        interface_load.loaded.retain(|(_, descriptor)| {
            graph.is_available(&RegistryNode::interface(descriptor.id.as_str()))
        });
        let worker_policies = worker_policies
            .into_iter()
            .filter(|(id, _)| graph.is_available(&RegistryNode::interface(id.as_str())))
            .collect();
        let interface_dispositions = interface_dispositions
            .into_iter()
            .filter(|(id, _)| graph.is_available(&RegistryNode::interface(id.as_str())))
            .collect();
        let tools = tool_load
            .loaded
            .into_iter()
            .filter(|(_, descriptor)| {
                graph.is_available(&RegistryNode::tool(descriptor.id.as_str()))
            })
            .collect();
        let worker_environment_kinds = wek_load
            .loaded
            .into_iter()
            .filter(|(_, descriptor)| {
                graph.is_available(&RegistryNode::wek(
                    descriptor.worker_environment_kind_id.as_str(),
                ))
            })
            .collect();
        let ee_templates = ee_load
            .loaded
            .into_iter()
            .filter(|(_, descriptor)| {
                graph.is_available(&RegistryNode::ee_template(descriptor.id.as_str()))
            })
            .collect();
        Ok(Self {
            interfaces: interface_load.loaded,
            interface_dispositions,
            worker_policies,
            tools,
            worker_environment_kinds,
            ee_templates,
            graph,
            activations,
            dangling_ceiling_refs,
        })
    }
}

fn form_interface_dispositions(
    registry_root: &Path,
    interfaces: &[(DescriptorName, InterfaceDescriptor)],
    policies: &BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    graph: &mut RegistryReferenceGraph,
) -> BTreeMap<InterfaceId, InterfaceRuntimeDisposition> {
    let interface_root = registry_root.join("interface");
    let mut dispositions = BTreeMap::new();
    for (_, interface) in interfaces {
        let node = RegistryNode::interface(interface.id.as_str());
        if !graph.is_available(&node) {
            continue;
        }
        match form_interface_runtime_disposition(
            interface,
            &interface_root,
            policies.contains_key(&interface.id),
        ) {
            Ok(InterfaceRuntimeDisposition::Inert) => {
                graph.refuse(
                    node,
                    "interface declares neither an external reader nor a worker policy relay",
                );
            }
            Ok(disposition) => {
                dispositions.insert(interface.id.clone(), disposition);
            }
            Err(error) => graph.refuse(node, error),
        }
    }
    dispositions
}

pub fn registry_tool_output_sources(
    registry_root: &Path,
    tool_id: &str,
) -> Result<BTreeSet<InterfaceId>, String> {
    let packages = RegistryPackages::load(registry_root)?;
    let tool_node = RegistryNode::tool(tool_id);
    packages.graph.validate_active_references(
        &format!("tool/{tool_id}"),
        [("descriptor".to_string(), tool_node)],
    )?;
    let owner = packages
        .tools
        .iter()
        .find_map(|(_, tool)| (tool.id.as_str() == tool_id).then(|| tool.interface.clone()))
        .ok_or_else(|| format!("registry has no available '{tool_id}' tool descriptor"))?;
    let owner_node = RegistryNode::interface(owner.as_str());
    packages.graph.validate_active_references(
        &format!("interface/{}", owner.as_str()),
        [("descriptor".to_string(), owner_node)],
    )?;
    Ok(BTreeSet::from([owner]))
}

fn load_kind<T: Descriptor>(
    root: &Path,
    kind: RegistryNodeKind,
    optional_root: bool,
) -> Result<KindLoad<T>, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if optional_root && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(KindLoad {
                loaded: Vec::new(),
                unavailable: BTreeMap::new(),
            });
        }
        Err(error) => {
            return Err(format!(
                "registry kind root '{}' cannot be read: {error}",
                root.display()
            ));
        }
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "registry kind root '{}' entry cannot be read: {error}",
                root.display()
            )
        })?;
        if entry
            .file_type()
            .map_err(|error| {
                format!(
                    "registry entry '{}' type failed: {error}",
                    entry.path().display()
                )
            })?
            .is_dir()
        {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    let mut loaded = Vec::new();
    let mut unavailable = BTreeMap::new();
    for dir in dirs {
        let Some(folder) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let node = RegistryNode::new(kind, folder);
        let path = dir.join(format!("{}.json", T::KIND));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                unavailable.insert(
                    node,
                    format!("descriptor '{}' cannot be read: {error}", path.display()),
                );
                continue;
            }
        };
        let descriptor = match octopus_descriptors::parse_strict::<T>(&bytes) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                unavailable.insert(
                    node,
                    format!("descriptor '{}' is invalid: {error}", path.display()),
                );
                continue;
            }
        };
        if let Some(declared) = descriptor.declared_name() {
            if declared != folder {
                unavailable.insert(
                    node,
                    format!(
                        "descriptor '{}' declares '{}' instead of folder id '{}'",
                        path.display(),
                        declared,
                        folder
                    ),
                );
                continue;
            }
        }
        loaded.push((
            DescriptorName::new(format!("{}/{folder}", T::KIND)),
            descriptor,
        ));
    }
    Ok(KindLoad {
        loaded,
        unavailable,
    })
}

fn load_artifacts(registry_root: &Path, graph: &mut RegistryReferenceGraph) -> Result<(), String> {
    let root = registry_root.join("artifact");
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "artifact registry '{}' cannot be read: {error}",
                root.display()
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("artifact registry entry failed: {error}"))?;
        if !entry
            .file_type()
            .map_err(|error| {
                format!(
                    "artifact entry '{}' type failed: {error}",
                    entry.path().display()
                )
            })?
            .is_dir()
        {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let node = RegistryNode::artifact(&id);
        let path = entry.path().join("artifact.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                graph.refuse(
                    node,
                    format!(
                        "artifact descriptor '{}' cannot be read: {error}",
                        path.display()
                    ),
                );
                continue;
            }
        };
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(error) => {
                graph.refuse(
                    node,
                    format!(
                        "artifact descriptor '{}' is malformed: {error}",
                        path.display()
                    ),
                );
                continue;
            }
        };
        if value.get("id").and_then(serde_json::Value::as_str) != Some(id.as_str()) {
            graph.refuse(
                node,
                format!(
                    "artifact descriptor '{}' does not declare folder id '{}'",
                    path.display(),
                    id
                ),
            );
            continue;
        }
        let bytes = octopus_descriptors::canonical_bytes(&value);
        let hash = DescriptorHash::of_bytes(&bytes);
        graph.artifact_descriptor_bytes.insert(ArtifactId::new(&id), bytes);
        graph.register(node);
        graph
            .artifact_descriptor_hashes
            .insert(ArtifactId::new(id), hash);
    }
    Ok(())
}

fn link_tools(tools: &[(DescriptorName, ToolDescriptor)], graph: &mut RegistryReferenceGraph) {
    for (_, tool) in tools {
        let source = RegistryNode::tool(tool.id.as_str());
        graph.link(
            source.clone(),
            "interface",
            RegistryNode::interface(tool.interface.as_str()),
        );
        for (index, kind) in tool.supported_worker_environment_kinds.iter().enumerate() {
            graph.link(
                source.clone(),
                format!("supported_worker_environment_kinds[{index}]"),
                RegistryNode::wek(kind.as_str()),
            );
        }
    }
}

/// Edge every worker pin to each member of its ceiling — interfaces, tools,
/// worker-environment kinds, mark outcomes, role artifacts — so the active
/// reference closure walks a pin's whole granted surface and validates it
/// transitively, and run the dangling detector over those edges. The two kinds of
/// absence are NOT the same fact:
/// - a ceiling INTERFACE reference absent from the live registry is a typed
///   dangling FACT, never boot poison — the pin still forms and emission toward
///   the absent interface refuses at send time through the delivery protocol
///   (owner decision, notes2 2026-07-06); it stays a warning, no closure edge;
/// - a ceiling tool / wek / mark outcome / role artifact absent from the live
///   registry is an unsatisfiable grant — fail-closed: the pin is refused and
///   forms no grant (a worker referencing a mark id absent from `registry/mark`
///   fail-closes at boot, notes2 2026-07-05; a ceiling tool must resolve to a
///   tool descriptor, WORKER_TYPES.md §Tools).
///
/// Pass 1 applies the fail-closed refusals first so a pin refused for its own
/// unsatisfiable member reads as unavailable in pass 2 — a sibling granting it as
/// a ceiling interface then folds to a warning, never a fail-closed closure
/// dependency.
fn link_worker_ceilings(
    policies: &BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    graph: &mut RegistryReferenceGraph,
) -> Vec<DanglingCeilingRef> {
    for (worker, policy) in policies {
        let source = RegistryNode::interface(worker.as_str());
        if !graph.is_available(&source) {
            continue;
        }
        let unsatisfiable = policy
            .ceiling
            .tools
            .iter()
            .map(|tool| ("ceiling.tools", RegistryNode::tool(tool.as_str())))
            .chain(
                policy
                    .ceiling
                    .wek
                    .iter()
                    .map(|wek| ("ceiling.wek", RegistryNode::wek(wek.as_str()))),
            )
            .chain(
                policy
                    .ceiling
                    .mark_outcomes
                    .iter()
                    .map(|mark| ("ceiling.mark_outcomes", RegistryNode::mark(mark.as_str()))),
            )
            .chain(
                policy
                    .role_artifacts
                    .iter()
                    .map(|artifact| ("role_artifacts", RegistryNode::artifact(artifact.as_str()))),
            )
            .find(|(_, target)| !graph.is_available(target));
        if let Some((field, target)) = unsatisfiable {
            graph.refuse(
                source,
                format!(
                    "worker ceiling {field} grants '{target}', absent from the live registry — an unsatisfiable grant, fail-closed"
                ),
            );
        }
    }

    let mut dangling = Vec::new();
    for (worker, policy) in policies {
        let source = RegistryNode::interface(worker.as_str());
        if !graph.is_available(&source) {
            continue;
        }
        link_ceiling_members(
            &source,
            worker,
            "ceiling.interfaces",
            policy
                .ceiling
                .interfaces
                .iter()
                .map(|interface| RegistryNode::interface(interface.as_str())),
            true,
            graph,
            &mut dangling,
        );
        link_ceiling_members(
            &source,
            worker,
            "ceiling.tools",
            policy
                .ceiling
                .tools
                .iter()
                .map(|tool| RegistryNode::tool(tool.as_str())),
            false,
            graph,
            &mut dangling,
        );
        link_ceiling_members(
            &source,
            worker,
            "ceiling.wek",
            policy
                .ceiling
                .wek
                .iter()
                .map(|wek| RegistryNode::wek(wek.as_str())),
            false,
            graph,
            &mut dangling,
        );
        link_ceiling_members(
            &source,
            worker,
            "ceiling.mark_outcomes",
            policy
                .ceiling
                .mark_outcomes
                .iter()
                .map(|mark| RegistryNode::mark(mark.as_str())),
            false,
            graph,
            &mut dangling,
        );
        link_ceiling_members(
            &source,
            worker,
            "role_artifacts",
            policy
                .role_artifacts
                .iter()
                .map(|artifact| RegistryNode::artifact(artifact.as_str())),
            false,
            graph,
            &mut dangling,
        );
    }
    dangling
}

/// Draw a closure edge from `source` to every available ceiling member. An absent
/// member is a warning only for `warn_when_absent` fields (ceiling interfaces —
/// the send-time-validated reach axis); on any other field the pin was already
/// refused in pass 1 (fail-closed), so a still-available pin never reaches the
/// skip branch there.
fn link_ceiling_members(
    source: &RegistryNode,
    worker: &InterfaceId,
    field: &str,
    targets: impl Iterator<Item = RegistryNode>,
    warn_when_absent: bool,
    graph: &mut RegistryReferenceGraph,
    dangling: &mut Vec<DanglingCeilingRef>,
) {
    for target in targets {
        if graph.is_available(&target) {
            graph.link(source.clone(), field, target);
        } else if warn_when_absent {
            dangling.push(DanglingCeilingRef {
                worker: worker.as_str().to_string(),
                field: field.to_string(),
                target,
            });
        }
    }
}

fn refuse_cross_package_mismatches(
    policies: &BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    tools: &[(DescriptorName, ToolDescriptor)],
    kinds: &[(DescriptorName, WorkerEnvKindSpec)],
    interfaces: &[(DescriptorName, InterfaceDescriptor)],
    interface_dispositions: &BTreeMap<InterfaceId, InterfaceRuntimeDisposition>,
    graph: &mut RegistryReferenceGraph,
) {
    let worker_views = policies
        .iter()
        .map(|(worker, policy)| WorkerEnvironmentCeilingView {
            worker,
            wek: policy.ceiling.wek.iter().map(|wek| wek.as_str()).collect(),
            tools: policy
                .ceiling
                .tools
                .iter()
                .map(|tool| tool.as_str())
                .collect(),
        })
        .collect::<Vec<_>>();
    let tool_views = tools
        .iter()
        .map(|(_, tool)| WorkerEnvironmentToolView {
            tool: tool.id.as_str(),
            interface: &tool.interface,
            process_bin: &tool.process.bin,
            worker_environment_kinds: tool
                .supported_worker_environment_kinds
                .iter()
                .map(|kind| kind.as_str())
                .collect(),
        })
        .collect::<Vec<_>>();
    let mut registry_issues = BTreeMap::<RegistryNode, BTreeSet<String>>::new();
    for issue in worker_environment_registry_issues(
        &worker_views,
        kinds,
        &tool_views,
        interfaces,
        interface_dispositions,
    ) {
        let node = match issue.subject {
            WorkerEnvironmentRegistrySubject::Worker(worker) => {
                RegistryNode::interface(worker.as_str())
            }
            WorkerEnvironmentRegistrySubject::Tool(tool) => RegistryNode::tool(tool),
            WorkerEnvironmentRegistrySubject::Kind(kind) => RegistryNode::wek(kind),
            WorkerEnvironmentRegistrySubject::Interface(interface) => {
                RegistryNode::interface(interface.as_str())
            }
        };
        registry_issues
            .entry(node)
            .or_default()
            .insert(issue.reason);
    }
    for (node, reasons) in registry_issues {
        graph.refuse(node, reasons.into_iter().collect::<Vec<_>>().join("; "));
    }
}

fn load_activation_references(
    registry_root: &Path,
    interfaces: &[(DescriptorName, InterfaceDescriptor)],
    graph: &mut RegistryReferenceGraph,
) -> Result<Vec<ActivationReferenceSet>, String> {
    let mut activations = Vec::new();
    for (_, interface) in interfaces {
        if !graph.is_available(&RegistryNode::interface(interface.id.as_str())) {
            continue;
        }
        let interface_dir = registry_root.join("interface").join(interface.id.as_str());
        let path = interface_dir.join("activation.json");
        if !path.exists() {
            continue;
        }
        let loaded = match octopus_interface_projection::load_interface_activation_file_in_dir(
            &interface_dir,
            &interface.id,
        ) {
            Ok(loaded) => loaded,
            Err(error) => {
                graph.refuse(
                    RegistryNode::interface(interface.id.as_str()),
                    format!(
                        "activation sidecar '{}' is invalid: {error}",
                        path.display()
                    ),
                );
                continue;
            }
        };
        for (index, activation) in loaded.descriptor.activations.iter().enumerate() {
            if !activation.enabled {
                continue;
            }
            let source = format!("activation/{}#{index}", interface.id.as_str());
            let references = vec![(
                "interface".to_string(),
                RegistryNode::interface(interface.id.as_str()),
            )];
            activations.push(ActivationReferenceSet { source, references });
        }
    }
    Ok(activations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_preserves_first_breadth_first_refusal_through_diamonds_and_cycles() {
        let mut graph = RegistryReferenceGraph::default();
        let nodes = ["root", "left", "right", "shared"].map(RegistryNode::tool);
        for node in &nodes {
            graph.register(node.clone());
        }
        for (from, field, to) in [
            (0, "left", 1),
            (0, "right", 2),
            (1, "shared", 3),
            (2, "shared", 3),
            (3, "cycle", 0),
        ] {
            graph.link(nodes[from].clone(), field, nodes[to].clone());
        }
        let roots = [
            ("first".into(), nodes[0].clone()),
            ("second".into(), nodes[2].clone()),
        ];
        assert_eq!(
            graph
                .validate_active_references("run", roots.clone())
                .unwrap(),
            nodes.into_iter().collect()
        );
        graph.link(
            RegistryNode::tool("shared"),
            "broken",
            RegistryNode::mark("absent"),
        );
        assert_eq!(
            graph.validate_active_references("run", roots).unwrap_err(),
            "active registry reference closure reached missing mark/absent; path: run.first -> tool/root -> tool/root.left -> tool/left -> tool/left.shared -> tool/shared -> tool/shared.broken -> mark/absent"
        );
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().expect("scratch path has a parent"))
            .expect("scratch dir writes");
        std::fs::write(path, body).expect("scratch file writes");
    }

    fn worker_pin(root: &Path, id: &str, ceiling_json: &str) {
        let dir = root.join("interface").join(id);
        write(
            &dir.join("interface.json"),
            &format!(
                r#"{{"id":"{id}","interface_type":"system","role":"worker","direction":"bidirectional","source_names":["{id}"]}}"#
            ),
        );
        write(
            &dir.join("policy.json"),
            &format!(r#"{{"id":"{id}","ceiling":{ceiling_json},"role_artifacts":[]}}"#),
        );
    }

    /// The live `RegistryPackages::load` path proves the ceiling reference closure:
    /// an absent ceiling mark / tool refuses the pin (fail-closed, WORKER_TYPES.md +
    /// notes2 2026-07-05), an absent ceiling interface stays a typed dangling
    /// warning (owner, notes2 2026-07-06), and every present ceiling member is
    /// edged so the active closure walks the pin's whole granted surface.
    #[test]
    fn worker_ceiling_closure_fails_closed_on_marks_and_tools_and_warns_on_interfaces() {
        let root = std::env::temp_dir().join(format!(
            "octopus-ceiling-closure-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for kind in [
            "interface",
            "tool",
            "worker_environment_kind",
            "ee_template",
        ] {
            std::fs::create_dir_all(root.join(kind)).expect("registry kind root writes");
        }
        write(
            &root
                .join("interface")
                .join("world_present")
                .join("interface.json"),
            r#"{"id":"world_present","interface_type":"world","role":"probe","direction":"outbound","source_names":["p"],"interface_driver_ref":"probe_reader"}"#,
        );
        worker_pin(
            &root,
            "system_worker_missing_mark",
            r#"{"interfaces":[],"tools":[],"wek":[],"mark_outcomes":["ghost_mark"],"hard_blocked_tools":[]}"#,
        );
        worker_pin(
            &root,
            "system_worker_missing_tool",
            r#"{"interfaces":[],"tools":["ghost_tool"],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]}"#,
        );
        worker_pin(
            &root,
            "system_worker_missing_iface",
            r#"{"interfaces":["ghost_iface"],"tools":[],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]}"#,
        );
        worker_pin(
            &root,
            "system_worker_reaches",
            r#"{"interfaces":["world_present"],"tools":[],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]}"#,
        );

        let packages = RegistryPackages::load(&root).expect("the crafted registry loads");

        let missing_mark = InterfaceId::new("system_worker_missing_mark");
        let missing_tool = InterfaceId::new("system_worker_missing_tool");
        let missing_iface = InterfaceId::new("system_worker_missing_iface");
        let reaches = InterfaceId::new("system_worker_reaches");

        // Fail-closed: an absent ceiling mark / tool refuses the pin, dropping it
        // from the loaded worker policies — it forms no grant.
        for refused in [&missing_mark, &missing_tool] {
            assert!(
                !packages
                    .graph
                    .is_available(&RegistryNode::interface(refused.as_str())),
                "{} must be refused for its unsatisfiable ceiling member",
                refused.as_str()
            );
            assert!(!packages.worker_policies.contains_key(refused));
        }
        // The fail-closed kinds are never demoted to boot warnings.
        assert!(
            !packages
                .dangling_ceiling_refs
                .iter()
                .any(|d| d.field == "ceiling.mark_outcomes" || d.field == "ceiling.tools"),
            "absent marks/tools fail closed, they do not masquerade as warnings: {:?}",
            packages.dangling_ceiling_refs
        );

        // Warning: an absent ceiling interface keeps the pin available and surfaces
        // a typed dangling fact instead of poisoning the boot.
        assert!(
            packages
                .graph
                .is_available(&RegistryNode::interface(missing_iface.as_str()))
        );
        assert!(packages.worker_policies.contains_key(&missing_iface));
        assert!(
            packages
                .dangling_ceiling_refs
                .iter()
                .any(|d| d.worker == "system_worker_missing_iface"
                    && d.field == "ceiling.interfaces"
                    && d.target == RegistryNode::interface("ghost_iface")),
            "the absent ceiling interface is a dangling warning: {:?}",
            packages.dangling_ceiling_refs
        );

        // Closure completeness: the reaching pin edges to its present ceiling
        // interface and the active reference closure walks that edge — the reach the
        // pre-fix graph (no ceiling.interfaces edges) could never validate.
        let closure = packages
            .graph
            .validate_active_references(
                "test",
                [(
                    "root".to_string(),
                    RegistryNode::interface(reaches.as_str()),
                )],
            )
            .expect("the reaching pin's closure is satisfiable");
        assert!(
            closure.contains(&RegistryNode::interface("world_present")),
            "the active closure reaches the granted ceiling interface: {closure:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
