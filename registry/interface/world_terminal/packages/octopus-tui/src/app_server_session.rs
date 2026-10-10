mod fs;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::guardian_events::GuardianAssessmentEvent;
use crate::session_vocab::openai_models::ModelPreset;
use crate::status::StatusAccountDisplay;
use crate::tui_config::TuiConfig;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use octopus_substrate_contract::approvals_substrate::AskForApproval;
use octopus_substrate_contract::permission_profile::ActivePermissionProfile;
use octopus_substrate_contract::permission_profile::PermissionProfile;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_world_terminal_client::ExecCommandItem;
use octopus_world_terminal_client::ExecItemSource;
use octopus_world_terminal_client::ExecItemStatus;
use octopus_world_terminal_client::GoalState;
use octopus_world_terminal_client::GoalStatusItem;
use octopus_world_terminal_client::HostPath;
use octopus_world_terminal_client::TerminalUserInput;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

fn shell_aggregated_output(
    stdout: String,
    stderr: String,
    error: Option<String>,
) -> Option<String> {
    let mut parts = Vec::new();
    for value in [stdout, stderr] {
        let value = value.trim_end_matches(&['\r', '\n'][..]);
        if !value.is_empty() {
            parts.push(value.to_string());
        }
    }
    if let Some(error) = error {
        let error = error.trim();
        if !error.is_empty() {
            parts.push(error.to_string());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn goal_item_from_ee(ee: &octopus_ee::EeInstance) -> GoalStatusItem {
    GoalStatusItem {
        objective: ee.goal.clone(),
        status: GoalState::Active,
        token_budget: None,
        tokens_used: 0,
        time_used_seconds: 0,
    }
}

fn model_presets_from_provider_catalog(
    catalog: Vec<octopus_world_terminal_client::ProviderModelPreset>,
    active_model: &str,
) -> Result<Vec<ModelPreset>> {
    catalog
        .into_iter()
        .map(|preset| {
            let default_reasoning_effort = preset
                .default_reasoning_effort
                .parse()
                .map_err(|error: String| color_eyre::eyre::eyre!(error))?;
            let supported_reasoning_efforts = preset
                .supported_reasoning_efforts
                .into_iter()
                .map(|effort| {
                    Ok(crate::session_vocab::openai_models::ReasoningEffortPreset {
                        effort: effort
                            .effort
                            .parse()
                            .map_err(|error: String| color_eyre::eyre::eyre!(error))?,
                        description: effort.description,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(ModelPreset {
                id: preset.model.clone(),
                display_name: preset.display_name,
                description: preset.description,
                model: preset.model.clone(),
                default_reasoning_effort,
                supported_reasoning_efforts,
                supports_personality: false,
                additional_speed_tiers: Vec::new(),
                service_tiers: Vec::new(),
                default_service_tier: None,
                is_default: preset.model == active_model,
                upgrade: None,
                show_in_picker: preset.show_in_picker,
                availability_nux: None,
                supported_in_api: true,
                input_modalities: crate::session_vocab::openai_models::default_input_modalities(),
            })
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TerminalAccountRead {
    pub(crate) identity: Option<TerminalAccountIdentity>,
    pub(crate) requires_openai_auth: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalAccountIdentity {
    pub(crate) id: String,
    pub(crate) display: Option<String>,
    pub(crate) plan: Option<String>,
}

pub(crate) struct AppServerBootstrap {
    pub(crate) duration: Duration,
    pub(crate) account_email: Option<String>,
    pub(crate) status_account_display: Option<StatusAccountDisplay>,
    pub(crate) plan_type: Option<crate::plan_type::PlanType>,
    pub(crate) requires_openai_auth: bool,
    pub(crate) default_model: String,
    pub(crate) has_chatgpt_account: bool,
    pub(crate) available_models: Vec<ModelPreset>,
}

pub(crate) struct AppServerSession {
    remote_cwd_override: Option<PathBuf>,
    thread_params_mode: ThreadParamsMode,
    default_model: Option<String>,
    available_models: Vec<ModelPreset>,
    embedded_spine: Option<crate::embedded_spine::TerminalClientSpine>,
    embedded_reply_rx: Option<EmbeddedReplyLaneReceiver>,
    embedded_reply_consumer: Option<tokio::task::JoinHandle<()>>,
    pending_admission_lanes: std::collections::VecDeque<crate::embedded_spine::ReplyRenderLane>,
    private_topology: Option<PrivateTopologyContext>,
}

#[derive(Clone)]
struct PrivateTopologyContext {
    octopus_home: PathBuf,
    roots: octopus_world_terminal_client::TerminalHostRoots,
    durable_root: PathBuf,
    topology: octopus_world_terminal_client::PrivateRunTopology,
    topology_mutation: std::sync::Arc<tokio::sync::Mutex<()>>,
}

fn merge_recent_transcript_artifacts(
    context: &PrivateTopologyContext,
    request: &mut octopus_world_terminal_client::PrivateRunRequest,
    limit: usize,
) -> Result<()> {
    let mut pager = octopus_world_terminal_client::ReverseHistoryPager::open(
        &context.durable_root,
        context.topology.run.clone(),
        context.topology.terminal_interface.clone(),
    )
    .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    let page = pager
        .read_page(
            limit.min(octopus_world_terminal_client::MAX_HISTORY_PAGE_ENTRIES),
            octopus_world_terminal_client::MAX_HISTORY_SCAN_ROWS,
        )
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
    let mut seen = request
        .seed_artifacts
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for artifact in page
        .entries
        .into_iter()
        .map(|entry| entry.artifact.id.as_str().to_string())
    {
        if seen.insert(artifact.clone()) {
            request.seed_artifacts.push(artifact);
        }
    }
    Ok(())
}

type EmbeddedReplyLaneItem = Result<
    (crate::embedded_spine::ReplyRenderLane, Option<String>),
    octopus_world_terminal_client::TerminalOutboundError,
>;
type EmbeddedReplyLaneReceiver = tokio::sync::mpsc::UnboundedReceiver<EmbeddedReplyLaneItem>;

#[derive(Clone, Debug)]
pub(crate) struct RegistryTabReads {
    pub(crate) registry_root: std::result::Result<PathBuf, String>,
    pub(crate) config_kind_count: std::result::Result<usize, String>,
    pub(crate) run_count: std::result::Result<usize, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadParamsMode {
    Embedded,
    Remote,
}

#[derive(Debug, thiserror::Error)]
#[error("{method} failed in the terminal facade: {message}")]
pub(crate) struct TerminalFacadeRequestError {
    pub(crate) method: String,
    pub(crate) message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TurnPermissionsOverride {
    Preserve,
    ActiveProfile(ActivePermissionProfile),
    LegacySandbox(PermissionProfile),
}

impl AppServerSession {
    pub(crate) fn new(thread_params_mode: ThreadParamsMode) -> Self {
        Self {
            remote_cwd_override: None,
            thread_params_mode,
            default_model: None,
            available_models: Vec::new(),
            embedded_spine: None,
            embedded_reply_rx: None,
            embedded_reply_consumer: None,
            pending_admission_lanes: std::collections::VecDeque::new(),
            private_topology: None,
        }
    }

    pub(crate) fn with_private_topology(
        mut self,
        octopus_home: PathBuf,
        roots: octopus_world_terminal_client::TerminalHostRoots,
        durable_root: PathBuf,
        topology: octopus_world_terminal_client::PrivateRunTopology,
    ) -> Result<Self> {
        self.private_topology = Some(PrivateTopologyContext {
            octopus_home,
            roots,
            durable_root,
            topology: topology.clone(),
            topology_mutation: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        });
        self.replace_private_topology(topology)?;
        Ok(self)
    }

    pub(crate) fn replace_private_topology(
        &mut self,
        topology: octopus_world_terminal_client::PrivateRunTopology,
    ) -> Result<()> {
        let context = self.private_topology.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "private topology replacement requires an attached terminal host context"
            )
        })?;
        let active_arrival_interface = crate::embedded_spine::ActiveArrivalInterface {
            interface: topology.terminal_interface.clone(),
            interface_input_artifact_label: topology.operator_input_label.clone(),
            turn_control_artifact_label: topology.turn_control_label.clone(),
            model: topology.model.clone(),
            reasoning_effort: topology.reasoning_effort.clone(),
        };
        let available_models = model_presets_from_provider_catalog(
            octopus_world_terminal_client::load_private_provider_model_catalog(
                &context.roots.registry_root,
                &context.roots.ee_instances_root,
                &topology,
            )
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?,
            &topology.model,
        )?;
        let endpoint = octopus_world_terminal_client::DaemonEndpoint::with_control_socket(
            &context.octopus_home,
            topology.terminal_socket.clone(),
        )
        .with_durable_ledger_root(context.durable_root.clone());
        let host_io_limits = octopus_world_terminal_client::load_interface_host_io_limits(
            &context.roots.registry_root,
            &topology.terminal_interface,
        )
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        let spine = crate::embedded_spine::TerminalClientSpine::new_with_endpoint(
            topology.run.clone(),
            endpoint,
        )
        .with_active_arrival_interface(active_arrival_interface.clone())
        .with_client_interface(topology.terminal_interface.clone(), host_io_limits);
        if let Some(consumer) = self.embedded_reply_consumer.take() {
            consumer.abort();
        }
        self.embedded_reply_rx = None;
        self.pending_admission_lanes.clear();
        self.embedded_spine = Some(spine);
        self.default_model = Some(topology.model.clone());
        self.available_models = available_models;
        self.private_topology
            .as_mut()
            .expect("private topology context was checked above")
            .topology = topology.clone();
        tracing::info!(
            run = %topology.run.as_str(),
            interface = %active_arrival_interface.interface.as_str(),
            model = %active_arrival_interface.model,
            reasoning_effort = %active_arrival_interface.reasoning_effort,
            "terminal client attached to its private run interfaces"
        );
        Ok(())
    }

    pub(crate) fn drives_embedded_spine(&self) -> bool {
        self.embedded_spine.is_some()
    }

    pub(crate) fn active_run_id(&self) -> Option<octopus_descriptors::RunId> {
        self.private_topology
            .as_ref()
            .map(|context| context.topology.run.clone())
    }

    pub(crate) fn active_topology(
        &self,
    ) -> Option<&octopus_world_terminal_client::PrivateRunTopology> {
        self.private_topology
            .as_ref()
            .map(|context| &context.topology)
    }

    pub(crate) fn task_projection(&self) -> Result<Vec<octopus_world_terminal_client::TaskEntry>> {
        let context = self.private_topology.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!("task projection requires an attached terminal host")
        })?;
        octopus_world_terminal_client::TerminalTaskProjection::load(&context.octopus_home)
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))
    }

    pub(crate) fn history_pager(
        &self,
    ) -> Result<octopus_world_terminal_client::ReverseHistoryPager> {
        let context = self.private_topology.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!("history projection requires an attached terminal run")
        })?;
        octopus_world_terminal_client::ReverseHistoryPager::open(
            &context.durable_root,
            context.topology.run.clone(),
            context.topology.terminal_interface.clone(),
        )
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))
    }

    pub(crate) fn working_roots(&self) -> Result<Vec<(String, PathBuf)>> {
        let context = self.private_topology.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!("working-root projection requires an attached terminal run")
        })?;
        let mut roots = Vec::new();
        for (room, interface) in &context.topology.worker_environment_interfaces {
            let path = context
                .roots
                .registry_interface_root
                .join(interface.as_str())
                .join("interface.json");
            let bytes = std::fs::read(&path).wrap_err_with(|| {
                format!("reading exact coder door '{}' failed", path.display())
            })?;
            let descriptor = octopus_descriptors::parse_strict::<
                octopus_interfaces::InterfaceDescriptor,
            >(&bytes)
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
            if let Some(config) = descriptor.interface_driver_config {
                for (slot, root) in config.host_slots {
                    roots.push((format!("{room}.{slot}"), root.path));
                }
            }
        }
        if roots.is_empty() {
            roots.push(("repo_root".to_string(), context.roots.repo_root.clone()));
            roots.push((
                "worktree_root".to_string(),
                context.roots.worktree_root.clone(),
            ));
        }
        roots.sort();
        roots.dedup();
        Ok(roots)
    }

    pub(crate) async fn attach_existing_run(
        &mut self,
        run: &octopus_descriptors::RunId,
    ) -> Result<()> {
        let context = self.private_topology.as_ref().cloned().ok_or_else(|| {
            color_eyre::eyre::eyre!("task switch requires an attached terminal host")
        })?;
        let topology = octopus_world_terminal_client::load_private_run_topology(
            &context.octopus_home,
            &context.roots.registry_root,
            &context.roots.run_records_root,
            run,
        )
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        octopus_world_terminal_client::await_private_run_interfaces(
            &context.roots.registry_root,
            &topology,
        )
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        self.replace_private_topology(topology)
    }

    pub(crate) async fn author_new_task(&mut self) -> Result<octopus_descriptors::RunId> {
        let context = self.private_topology.as_ref().cloned().ok_or_else(|| {
            color_eyre::eyre::eyre!("new task requires an attached terminal host")
        })?;
        let mut request = octopus_world_terminal_client::default_private_run_request(
            &context.roots.registry_root,
        )
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?
        .with_existing_ee(context.topology.ee.clone());
        request.model = context.topology.model.clone();
        request.reasoning_effort = context.topology.reasoning_effort.clone();
        let authored = octopus_world_terminal_client::author_private_run(
            &context.octopus_home,
            &context.roots.registry_root,
            request,
        )
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        let run = authored.topology.run.clone();
        self.replace_private_topology(authored.topology)?;
        Ok(run)
    }

    pub(crate) async fn author_side_run(&mut self) -> Result<octopus_descriptors::RunId> {
        let context = self.private_topology.as_ref().cloned().ok_or_else(|| {
            color_eyre::eyre::eyre!("side question requires an attached terminal run")
        })?;
        let parent = context.topology.run.clone();
        let mut request = octopus_world_terminal_client::fork_private_run_request(
            &context.octopus_home,
            &context.roots.registry_root,
            &context.roots.run_records_root,
            &parent,
        )
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?
        .with_existing_ee(context.topology.ee.clone());
        merge_recent_transcript_artifacts(&context, &mut request, 24)?;
        let authored = octopus_world_terminal_client::author_private_run(
            &context.octopus_home,
            &context.roots.registry_root,
            request,
        )
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        let run = authored.topology.run.clone();
        let mut state = octopus_world_terminal_client::TaskStateStore::open(&context.octopus_home)
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        state
            .mark_side(&run, &parent)
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        self.replace_private_topology(authored.topology)?;
        Ok(run)
    }

    pub(crate) async fn author_root_continuation(
        &mut self,
        root: PathBuf,
    ) -> Result<octopus_descriptors::RunId> {
        let context = self.private_topology.as_ref().cloned().ok_or_else(|| {
            color_eyre::eyre::eyre!("working-root change requires an attached terminal run")
        })?;
        let source = context.topology.run.clone();
        let mut request = octopus_world_terminal_client::fork_private_run_request(
            &context.octopus_home,
            &context.roots.registry_root,
            &context.roots.run_records_root,
            &source,
        )
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?
        .with_existing_ee(context.topology.ee.clone());
        merge_recent_transcript_artifacts(&context, &mut request, 40)?;
        request.door_host_slots.insert(
            "system_coder_env".to_string(),
            std::collections::BTreeMap::from([
                ("repo_root".to_string(), root.clone()),
                ("worktree_root".to_string(), root),
            ]),
        );
        let authored = octopus_world_terminal_client::author_private_run(
            &context.octopus_home,
            &context.roots.registry_root,
            request,
        )
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        let run = authored.topology.run.clone();
        let mut state = octopus_world_terminal_client::TaskStateStore::open(&context.octopus_home)
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        state
            .mark_continuation(&run, &source)
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        self.replace_private_topology(authored.topology)?;
        Ok(run)
    }

    pub(crate) fn rename_task(&self, run: &octopus_descriptors::RunId, title: &str) -> Result<()> {
        let context = self.private_topology.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!("task rename requires an attached terminal host")
        })?;
        let mut state = octopus_world_terminal_client::TaskStateStore::open(&context.octopus_home)
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        state
            .rename(run, title)
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))
    }

    pub(crate) fn active_run_record_path(&self) -> Option<PathBuf> {
        self.private_topology.as_ref().map(|context| {
            context
                .roots
                .run_records_root
                .join(format!("{}.json", context.topology.run.as_str()))
        })
    }

    pub(crate) fn run_record_path(&self, run: &octopus_descriptors::RunId) -> Option<PathBuf> {
        self.private_topology.as_ref().map(|context| {
            context
                .roots
                .run_records_root
                .join(format!("{}.json", run.as_str()))
        })
    }

    pub(crate) fn prompt_history_durable_root(&self) -> Option<std::path::PathBuf> {
        self.embedded_spine
            .as_ref()
            .map(|spine| spine.durable_root().to_path_buf())
    }

    pub(crate) fn client_interface(&self) -> Result<octopus_descriptors::InterfaceId> {
        self.embedded_spine
            .as_ref()
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "no embedded spine is attached, so the interface composer has no active interface \
                     id"
                )
            })?
            .client_interface()
            .cloned()
            .map_err(|err| color_eyre::eyre::eyre!(err.to_string()))
    }

    fn unavailable_interface_action<T>(&self, action: &str) -> Result<T> {
        crate::unavailable_interface_action::fail(action)
    }

    pub(crate) fn drives_artifact_admission(&self) -> bool {
        self.embedded_spine
            .as_ref()
            .map(|spine| spine.has_daemon())
            .unwrap_or(false)
    }

    pub(crate) async fn submit_artifact_admission_via_embedded_spine(
        &mut self,
        submit_bytes: &[u8],
    ) -> Option<
        Result<
            crate::embedded_spine::TerminalSubmitAck,
            crate::embedded_spine::TerminalSubmitError,
        >,
    > {
        let spine = self.embedded_spine.as_ref()?;
        Some(spine.hand_input_to_daemon(submit_bytes).await)
    }

    pub(crate) fn dashboard_movements_snapshot(&self) -> Vec<octopus_ledger::Movement> {
        self.embedded_spine
            .as_ref()
            .map(|spine| spine.movements_snapshot())
            .unwrap_or_default()
    }

    pub(crate) fn file_manager_registry_read(
        &self,
    ) -> color_eyre::Result<crate::embedded_registry_read::RegistryRead> {
        let root = &self
            .private_topology
            .as_ref()
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "registry file-manager read requires an attached terminal host context"
                )
            })?
            .roots
            .registry_root;
        let config = crate::registry_explorer::RegistryListing::scan(root)
            .wrap_err_with(|| format!("scanning registry root '{}' failed", root.display()))?
            .config_summary();

        let mut reader = crate::embedded_registry_read::RegistryRead::new().with_config(config);
        if let Some(spine) = self.embedded_spine.as_ref() {
            reader = reader.with_runs(spine.run_source());
        }
        Ok(reader)
    }

    pub(crate) fn registry_root(&self) -> Result<&std::path::Path> {
        self.private_topology
            .as_ref()
            .map(|context| context.roots.registry_root.as_path())
            .ok_or_else(|| color_eyre::eyre::eyre!("terminal host registry root is not attached"))
    }

    pub(crate) fn registry_tab_reads(&self) -> RegistryTabReads {
        let registry_root = self
            .private_topology
            .as_ref()
            .map(|context| context.roots.registry_root.clone())
            .ok_or_else(|| "terminal host registry root is not attached".to_string());
        let reader = self.file_manager_registry_read();
        let config_kind_count =
            reader
                .as_ref()
                .map_err(|error| error.to_string())
                .and_then(|reader| {
                    reader
                        .config_summary()
                        .map(|config| config.len())
                        .map_err(|error| error.to_string())
                });
        let run_count = reader
            .as_ref()
            .map_err(|error| error.to_string())
            .and_then(|reader| {
                reader
                    .run_summaries()
                    .map(|runs| runs.len())
                    .map_err(|error| error.to_string())
            });
        RegistryTabReads {
            registry_root,
            config_kind_count,
            run_count,
        }
    }

    pub(crate) async fn next_embedded_reply_lane(
        &mut self,
    ) -> Option<Option<EmbeddedReplyLaneItem>> {
        if self.embedded_reply_rx.is_none() {
            let spine = self.embedded_spine.as_ref()?;
            let (rx, handle) = spine.spawn_terminal_outbound_consumer();
            self.embedded_reply_rx = Some(rx);
            self.embedded_reply_consumer = Some(handle);
        }
        let rx = self.embedded_reply_rx.as_mut()?;
        match rx.recv().await {
            Some(item) => Some(Some(item)),
            None => {
                if let Some(admission_lane) = self.pending_admission_lanes.pop_front() {
                    return Some(Some(Ok((admission_lane, None))));
                }
                if let Some(handle) = self.embedded_reply_consumer.take() {
                    match handle.await {
                        Ok(()) => Some(None),
                        Err(join_err) => Some(Some(Err(
                            octopus_world_terminal_client::TerminalOutboundError::Read(format!(
                                "terminal outbound reader consumer task failed: {join_err}"
                            )),
                        ))),
                    }
                } else {
                    Some(None)
                }
            }
        }
    }

    pub(crate) fn embedded_reply_blob(
        &self,
        content: &octopus_ledger::ContentHash,
    ) -> Option<Vec<u8>> {
        self.embedded_spine
            .as_ref()
            .and_then(|spine| spine.blob_bytes(content))
    }

    pub(crate) fn with_remote_cwd_override(mut self, remote_cwd_override: Option<PathBuf>) -> Self {
        self.remote_cwd_override = remote_cwd_override;
        self
    }

    pub(crate) fn remote_cwd_override(&self) -> Option<&std::path::Path> {
        self.remote_cwd_override.as_deref()
    }

    pub(crate) fn uses_remote_workspace(&self) -> bool {
        matches!(self.thread_params_mode, ThreadParamsMode::Remote)
    }

    pub(crate) fn uses_embedded_app_server(&self) -> bool {
        false
    }

    pub(crate) fn codex_home_path(&self, local_codex_home: &AbsolutePathBuf) -> Option<HostPath> {
        HostPath::from_absolute_str(local_codex_home.as_path().to_str()?)
    }

    pub(crate) async fn bootstrap(&mut self, config: &TuiConfig) -> Result<AppServerBootstrap> {
        let started_at = Instant::now();
        let account = self.read_account().await?;

        let active_interface = self
            .embedded_spine
            .as_ref()
            .and_then(|spine| spine.active_arrival_interface())
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "TUI bootstrap has no active arrival interface artifact projection; refusing to derive \
                     a model from interface/list or config.model"
                )
            })?;
        let default_model = active_interface.model.clone();
        if let Some(config_model) = config
            .model
            .as_deref()
            .filter(|m| *m != default_model.as_str())
        {
            tracing::warn!(
                config_model,
                active_arrival_interface_model = %default_model,
                interface = %active_interface.interface.as_str(),
                "ignoring config.model during embedded TUI bootstrap; active arrival interface \
                 projection is the authority"
            );
        }
        let available_models = self.available_models.clone();
        if available_models.is_empty() {
            color_eyre::eyre::bail!("TUI bootstrap provider interface model catalog is empty");
        }

        self.default_model = Some(default_model.clone());
        self.available_models = available_models.clone();

        let (account_email, status_account_display, plan_type, has_chatgpt_account) =
            match account.identity {
                Some(identity) => (
                    identity.display.clone(),
                    Some(StatusAccountDisplay::ChatGpt {
                        email: identity.display,
                        plan: identity.plan,
                    }),
                    None,
                    false,
                ),
                None => (None, None, None, false),
            };
        Ok(AppServerBootstrap {
            duration: started_at.elapsed(),
            account_email,
            status_account_display,
            plan_type,
            requires_openai_auth: account.requires_openai_auth,
            default_model,
            has_chatgpt_account,
            available_models,
        })
    }

    pub(crate) async fn read_account(&mut self) -> Result<TerminalAccountRead> {
        let reader = self
            .file_manager_registry_read()
            .wrap_err("reading terminal registry files for account projection failed")?;
        let identity = reader.account().map(|account| TerminalAccountIdentity {
            id: account.id.clone(),
            display: account.display.clone(),
            plan: account.plan.clone(),
        });
        Ok(TerminalAccountRead {
            identity,
            requires_openai_auth: false,
        })
    }

    pub(crate) async fn thread_archive(&mut self) -> Result<()> {
        self.unavailable_interface_action("archive current chat")
    }

    pub(crate) async fn thread_delete(&mut self) -> Result<()> {
        self.unavailable_interface_action("delete current chat")
    }

    pub(crate) fn project_interface_payload_as_arrival_artifact_input(
        &self,
        payload_name: &str,
        payload_body: serde_json::Value,
    ) -> Result<Vec<u8>> {
        let spine = self.embedded_spine.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "{payload_name}: no embedded spine is attached, so the interface payload cannot be \
                 materialized as an artifact reference"
            )
        })?;
        let active_interface = spine.active_arrival_interface().cloned().ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "{payload_name}: embedded spine has no active interface artifact binding; refusing \
                 to submit without a descriptor-selected interface"
            )
        })?;
        let input_artifact_body =
            octopus_app_server_interface::InterfaceInputArtifactBody::from_json(
                serde_json::json!({
                    "interface_payload": {
                        "name": payload_name,
                        "body": payload_body,
                    }
                }),
            )
            .map_err(|e| color_eyre::eyre::eyre!(e))
            .wrap_err("failed to build interface-local payload artifact")?;
        spine
            .write_interface_input_reference(
                &active_interface.interface_input_artifact_label,
                &input_artifact_body,
            )
            .map_err(|e| color_eyre::eyre::eyre!(e))
            .wrap_err("failed to persist interface-local payload artifact reference")
    }

    async fn submit_interface_payload_admission(
        &mut self,
        payload_name: &str,
        payload_body: serde_json::Value,
    ) -> Result<crate::embedded_spine::TerminalSubmitAck> {
        let bytes =
            self.project_interface_payload_as_arrival_artifact_input(payload_name, payload_body)?;
        let Some(result) = self
            .submit_artifact_admission_via_embedded_spine(&bytes)
            .await
        else {
            color_eyre::eyre::bail!(
                "{payload_name}: no terminal client spine is wired; refusing to submit without a daemon"
            );
        };
        result.map_err(|source| {
            color_eyre::eyre::eyre!(
                "{payload_name} failed handing the payload artifact to the octopus-app daemon: {source}"
            )
        })
    }

    fn project_user_submit_as_interface_arrival_artifact_input(
        &self,
        items: &[TerminalUserInput],
        turn_context: serde_json::Value,
    ) -> Result<Vec<u8>> {
        let spine = self.embedded_spine.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "user submit: no embedded spine is attached, so the user input cannot be persisted as \
                 a descriptor-selected input artifact or submitted through the interface artifact binding"
            )
        })?;
        let active_interface = spine.active_arrival_interface().cloned().ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "user submit: embedded spine has no active interface artifact binding; refusing to \
                     submit without a descriptor-selected interface"
            )
        })?;
        let input_items = serde_json::to_value(items).wrap_err(
            "failed to encode user-submit input items into descriptor-selected input artifact",
        )?;
        let input_artifact_body =
            octopus_app_server_interface::InterfaceInputArtifactBody::from_input_items(input_items)
                .and_then(|body| body.with_turn_context(turn_context))
                .map_err(|e| color_eyre::eyre::eyre!(e))
                .wrap_err("failed to build user-submit descriptor-selected input artifact")?;
        let interface_artifact_input_bytes = spine
            .write_interface_input_reference(
                &active_interface.interface_input_artifact_label,
                &input_artifact_body,
            )
            .map_err(|e| color_eyre::eyre::eyre!(e))
            .wrap_err(
                "failed to persist user-submit descriptor-selected input artifact reference",
            )?;
        Ok(interface_artifact_input_bytes)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn turn_start(
        &mut self,
        items: Vec<TerminalUserInput>,
        cwd: PathBuf,
        approval_policy: AskForApproval,
        approvals_reviewer: crate::session_vocab::config_types::ApprovalsReviewer,
        permissions_override: TurnPermissionsOverride,
        workspace_roots: &[AbsolutePathBuf],
        summary: Option<crate::session_vocab::config_types::ReasoningSummary>,
        service_tier: Option<Option<String>>,
        collaboration_mode: Option<crate::session_vocab::config_types::CollaborationMode>,
        personality: Option<crate::session_vocab::config_types::Personality>,
        output_schema: Option<serde_json::Value>,
    ) -> Result<()> {
        let items = if let Some(context) = self.private_topology.as_ref() {
            octopus_world_terminal_client::bind_task_references(
                &context.octopus_home,
                &context.roots.registry_root,
                &context.roots.run_records_root,
                &context.durable_root,
                &context.topology.run,
                &context.topology.terminal_interface,
                items,
            )
            .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?
        } else {
            items
        };
        let permissions_override_debug = format!("{permissions_override:?}");
        let turn_context = serde_json::json!({
            "cwd": cwd.display().to_string(),
            "approval_policy": format!("{approval_policy:?}"),
            "approvals_reviewer": format!("{approvals_reviewer:?}"),
            "permissions_override": permissions_override_debug,
            "workspace_roots": workspace_roots
                .iter()
                .map(|root| format!("{root:?}"))
                .collect::<Vec<_>>(),
            "summary": format!("{summary:?}"),
            "service_tier": service_tier.clone(),
            "collaboration_mode": format!("{collaboration_mode:?}"),
            "personality": format!("{personality:?}"),
            "output_schema": output_schema.clone(),
        });
        let interface_artifact_input_bytes =
            self.project_user_submit_as_interface_arrival_artifact_input(&items, turn_context)?;

        if self.drives_artifact_admission() {
            if let Some(result) = self
                .submit_artifact_admission_via_embedded_spine(&interface_artifact_input_bytes)
                .await
            {
                let ack = result.map_err(|source| {
                    color_eyre::eyre::eyre!(
                        "user submit failed handing the input artifact to the octopus-app daemon: {source}"
                    )
                })?;
                tracing::info!(
                    run = %ack.addressed_run.as_str(),
                    "interactive submit handed to the octopus-app daemon (async admission)"
                );
                return Ok(());
            }
        }

        color_eyre::eyre::bail!(
            "user submit: no terminal client spine is wired; refusing to submit without a daemon"
        )
    }

    pub(crate) async fn turn_interrupt(
        &mut self,
    ) -> std::result::Result<(), TerminalFacadeRequestError> {
        let spine = self
            .embedded_spine
            .as_ref()
            .ok_or_else(|| TerminalFacadeRequestError {
                method: "turn/interrupt".to_string(),
                message: "no embedded spine is attached".to_string(),
            })?;
        let active_interface = spine.active_arrival_interface().cloned().ok_or_else(|| {
            TerminalFacadeRequestError {
                method: "turn/interrupt".to_string(),
                message: "embedded spine has no active interface artifact binding".to_string(),
            }
        })?;
        let control_id = format!("terminal-stop-{}", uuid::Uuid::now_v7());
        let body = octopus_app_server_interface::InterfaceInputArtifactBody::from_json(
            serde_json::json!({
                "action": "stop",
                "control_id": control_id,
            }),
        )
        .map_err(|message| TerminalFacadeRequestError {
            method: "turn/interrupt".to_string(),
            message,
        })?;
        let bytes = spine
            .write_interface_input_reference(&active_interface.turn_control_artifact_label, &body)
            .map_err(|error| TerminalFacadeRequestError {
                method: "turn/interrupt".to_string(),
                message: error.to_string(),
            })?;
        let ack = self
            .submit_artifact_admission_via_embedded_spine(&bytes)
            .await
            .ok_or_else(|| TerminalFacadeRequestError {
                method: "turn/interrupt".to_string(),
                message: "no terminal client spine is wired".to_string(),
            })?
            .map_err(|err| TerminalFacadeRequestError {
                method: "turn/interrupt".to_string(),
                message: err.to_string(),
            })?;
        tracing::debug!(run = %ack.addressed_run.as_str(), "turn/interrupt handed to the octopus-app daemon (async admission)");
        Ok(())
    }

    pub(crate) async fn respond_approval(
        &self,
        request_id: &str,
        decision: serde_json::Value,
    ) -> Result<()> {
        self.operator_responder()?
            .respond_approval(request_id, decision)
            .await
            .map(|_| ())
            .map_err(|e| color_eyre::eyre::eyre!("approval response artifact failed: {e}"))
    }

    pub(crate) async fn respond_elicitation(
        &self,
        request_id: &str,
        answer: serde_json::Value,
    ) -> Result<()> {
        self.operator_responder()?
            .respond_elicitation(request_id, answer)
            .await
            .map(|_| ())
            .map_err(|e| color_eyre::eyre::eyre!("elicitation response artifact failed: {e}"))
    }

    pub(crate) async fn respond_user_input(
        &self,
        request_id: &str,
        answers: std::collections::BTreeMap<String, Vec<String>>,
    ) -> Result<()> {
        self.operator_responder()?
            .respond_user_input(request_id, answers)
            .await
            .map(|_| ())
            .map_err(|e| color_eyre::eyre::eyre!("user-input response artifact failed: {e}"))
    }

    fn operator_responder(&self) -> Result<octopus_world_terminal_client::OperatorResponder> {
        let spine = self.embedded_spine.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "no terminal client spine is wired; refusing to emit an operator response without \
                 a daemon"
            )
        })?;
        Ok(spine.operator_responder())
    }

    pub(crate) async fn thread_set_name(&mut self, name: String) -> Result<()> {
        let _ = name;
        self.unavailable_interface_action("rename current chat")
    }

    pub(crate) async fn thread_goal_get(
        &mut self,
        thread_id: &str,
    ) -> Result<Option<GoalStatusItem>> {
        let context = self.private_topology_for_thread(thread_id)?;
        let path = context
            .roots
            .ee_instances_root
            .join(format!("{}.json", context.topology.ee.as_str()));
        let bytes = std::fs::read(&path)
            .wrap_err_with(|| format!("reading active EE '{}' failed", path.display()))?;
        let ee: octopus_ee::EeInstance = serde_json::from_slice(&bytes)
            .wrap_err_with(|| format!("active EE '{}' is malformed", path.display()))?;
        if ee.ee_id != context.topology.ee {
            color_eyre::eyre::bail!(
                "active EE file '{}' declares '{}', expected '{}'",
                path.display(),
                ee.ee_id.as_str(),
                context.topology.ee.as_str()
            );
        }
        Ok(Some(goal_item_from_ee(&ee)))
    }

    fn private_topology_for_thread(&self, thread_id: &str) -> Result<&PrivateTopologyContext> {
        let context = self.private_topology.as_ref().ok_or_else(|| {
            color_eyre::eyre::eyre!("thread goal requires an attached private terminal run")
        })?;
        if context.topology.run.as_str() != thread_id {
            color_eyre::eyre::bail!(
                "thread goal addressed run '{}', but the terminal is attached to '{}'",
                thread_id,
                context.topology.run.as_str()
            );
        }
        Ok(context)
    }

    async fn author_goal_run(&mut self, thread_id: &str, objective: String) -> Result<()> {
        let context = self.private_topology_for_thread(thread_id)?.clone();
        let mut request = octopus_world_terminal_client::fork_private_run_request(
            &context.octopus_home,
            &context.roots.registry_root,
            &context.roots.run_records_root,
            &context.topology.run,
        )
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        request.goal = objective;
        let authored = octopus_world_terminal_client::author_private_run(
            &context.octopus_home,
            &context.roots.registry_root,
            request,
        )
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        self.replace_private_topology(authored.topology)
    }

    pub(crate) async fn thread_goal_set(
        &mut self,
        thread_id: &str,
        objective: Option<String>,
        status: Option<GoalState>,
        token_budget: Option<Option<i64>>,
    ) -> Result<GoalStatusItem> {
        if token_budget.is_some() {
            color_eyre::eyre::bail!(
                "goal token budgets belong to the selected model interface, not EE policy"
            );
        }
        if status.is_some_and(|status| status != GoalState::Active) {
            color_eyre::eyre::bail!(
                "changing goal lifecycle state is not an interface-side mutation; create a new goal or explicitly edit the authored run and EE relations through a granted file tool"
            );
        }
        let objective = objective.ok_or_else(|| {
            color_eyre::eyre::eyre!("creating a goal requires a non-empty objective")
        })?;
        let objective = objective.trim().to_string();
        if objective.is_empty() {
            color_eyre::eyre::bail!("creating a goal requires a non-empty objective");
        }
        self.author_goal_run(thread_id, objective).await?;
        let active_run = self
            .active_run_id()
            .ok_or_else(|| color_eyre::eyre::eyre!("new goal run was not attached"))?;
        self.thread_goal_get(active_run.as_str())
            .await?
            .ok_or_else(|| color_eyre::eyre::eyre!("new goal EE was not materialized"))
    }

    pub(crate) async fn thread_goal_clear(&mut self, thread_id: &str) -> Result<bool> {
        self.private_topology_for_thread(thread_id)?;
        color_eyre::eyre::bail!(
            "clearing a goal requires explicitly editing or deleting its authored run and EE relations through a granted file tool"
        )
    }

    pub(crate) async fn logout_account(&mut self) -> Result<()> {
        self.unavailable_interface_action("account logout")
    }

    pub(crate) async fn thread_compact_start(&mut self) -> Result<()> {
        self.private_topology
            .as_ref()
            .ok_or_else(|| color_eyre::eyre::eyre!("compact requires an active private run"))?;
        self.submit_interface_payload_admission(
            "provider.thread.compact.start",
            serde_json::json!({}),
        )
        .await
        .wrap_err("compact: handing the provider operation artifact to the daemon failed")?;
        Ok(())
    }

    pub(crate) fn thread_shell_command(
        &self,
        item_id: String,
        command: String,
        events: AppEventSender,
    ) -> Result<()> {
        if item_id.trim().is_empty() || command.trim().is_empty() {
            color_eyre::eyre::bail!("shell item id and command must both be non-empty");
        }
        let context = self.private_topology.as_ref().cloned().ok_or_else(|| {
            color_eyre::eyre::eyre!("shell execution requires an active private terminal run")
        })?;
        tokio::spawn(async move {
            let _guard = context.topology_mutation.lock().await;
            let started = Instant::now();
            let result = octopus_world_terminal_client::execute_private_shell(
                &context.octopus_home,
                &context.roots.registry_root,
                &context.durable_root,
                &context
                    .octopus_home
                    .join("interface-hand-in")
                    .join("world_terminal")
                    .join("terminal-shell-artifacts")
                    .join(context.topology.run.as_str()),
                &context.topology,
                command.clone(),
            )
            .await;
            let item = match result {
                Ok(execution) => {
                    tracing::debug!(
                        run = %context.topology.run.as_str(),
                        admitted_seq = execution.admitted_seq,
                        result_seq = execution.result_seq,
                        result_artifact = %execution.result_artifact.as_str(),
                        "terminal shell execution completed through its worker-environment door"
                    );
                    let failed = execution.exit_code != Some(0) || execution.error.is_some();
                    let exit_code = if execution.error.is_some() {
                        execution.exit_code.or(Some(1))
                    } else {
                        execution.exit_code
                    };
                    ExecCommandItem {
                        item_id,
                        command: execution.command,
                        cwd: execution.worktree.display().to_string(),
                        process_id: None,
                        command_actions: Vec::new(),
                        source: ExecItemSource::UserShell,
                        status: if failed {
                            ExecItemStatus::Failed
                        } else {
                            ExecItemStatus::Completed
                        },
                        exit_code,
                        duration_ms: Some(i64::try_from(execution.duration_ms).unwrap_or(i64::MAX)),
                        aggregated_output: shell_aggregated_output(
                            execution.stdout,
                            execution.stderr,
                            execution.error,
                        ),
                    }
                }
                Err(error) => ExecCommandItem {
                    item_id,
                    command,
                    cwd: String::new(),
                    process_id: None,
                    command_actions: Vec::new(),
                    source: ExecItemSource::UserShell,
                    status: ExecItemStatus::Failed,
                    exit_code: Some(1),
                    duration_ms: Some(
                        i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
                    ),
                    aggregated_output: Some(error.to_string()),
                },
            };
            events.send(AppEvent::ShellCommandCompleted { item });
        });
        Ok(())
    }

    pub(crate) async fn persist_model_interface_selection(
        &mut self,
        model: String,
        effort: Option<crate::session_vocab::openai_models::ReasoningEffort>,
    ) -> Result<()> {
        let context = self.private_topology.as_ref().cloned().ok_or_else(|| {
            color_eyre::eyre::eyre!("model selection requires an active private terminal run")
        })?;
        let reasoning_effort = effort
            .as_ref()
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| context.topology.reasoning_effort.clone());
        let _guard = context.topology_mutation.lock().await;
        let topology = octopus_world_terminal_client::configure_private_model_interface(
            &context.octopus_home,
            &context.roots.registry_root,
            &context.topology,
            model,
            reasoning_effort,
        )
        .await
        .map_err(|error| color_eyre::eyre::eyre!(error.to_string()))?;
        self.replace_private_topology(topology)
    }

    pub(crate) async fn thread_approve_guardian_denied_action(
        &mut self,
        event: &GuardianAssessmentEvent,
    ) -> Result<()> {
        let event_value =
            serde_json::to_value(event).wrap_err("failed to serialize Auto Review denial event")?;
        let _ = event_value;
        self.unavailable_interface_action("approve denied action")
    }

    pub(crate) async fn thread_background_terminals_clean(&mut self) -> Result<()> {
        self.unavailable_interface_action("clean background terminals")
    }

    pub(crate) async fn thread_rollback(&mut self, num_turns: u32) -> Result<()> {
        let _ = num_turns;
        self.unavailable_interface_action("rollback current chat")
    }

    pub(crate) async fn review_start(
        &mut self,
        target: crate::app_command::ReviewTargetSpec,
    ) -> Result<()> {
        let _ = target;
        self.unavailable_interface_action("start review")
    }

    pub(crate) async fn shutdown(mut self) -> std::io::Result<()> {
        if let Some(consumer) = self.embedded_reply_consumer.take() {
            consumer.abort();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn embedded_session_refuses_unbound_interface_projection() {
        let session = AppServerSession::new(ThreadParamsMode::Embedded);

        let projection_err = session
            .project_interface_payload_as_arrival_artifact_input(
                "turn/interrupt",
                serde_json::json!({}),
            )
            .expect_err("a typed interface artifact needs an attached interface artifact binding");
        assert!(
            projection_err
                .to_string()
                .contains("no embedded spine is attached"),
            "{projection_err}"
        );
    }

    #[test]
    fn ee_projection_folds_into_goal_status_item() {
        let ee = octopus_ee::EeInstance {
            ee_id: octopus_descriptors::EeId::new("ee_ship"),
            template: octopus_descriptors::EeTemplateId::new("operator_interaction"),
            parent_ee: Some(octopus_descriptors::EeId::new("ee_existence")),
            goal: "ship the vertebra".to_string(),
            config: Default::default(),
            policy_narrowing: Default::default(),
        };
        let item = goal_item_from_ee(&ee);
        assert_eq!(item.objective, "ship the vertebra");
        assert_eq!(item.status, GoalState::Active);
        assert_eq!(item.token_budget, None);
        assert_eq!(item.tokens_used, 0);
    }
}
