use octopus_app_server_interface::InterfaceInputArtifactBody;
use octopus_descriptors::InterfaceId;
use octopus_ledger::{BlobStore, Movement, MovementLog, Seq};
use octopus_ledger_store::{DurableBlobs, DurableLog};
use octopus_world_terminal_client::{
    ApprovalRequest, DaemonArrivalError, ElicitationRequest, ExecCommandItem, HookCatalog,
    HookRunItem, InterfaceHostIoLimits, OutboundRenderKind, OutboundResolved, PlanUpdateItem,
    ReasoningItem, TerminalDaemonEmitter, TerminalOutboundError, TerminalOutboundReader,
    TokenUsageItem, ToolCallItem, UserInputRequest, UserMessageItem, outbound_resolved,
};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct ActiveArrivalInterface {
    pub(crate) interface: InterfaceId,
    pub(crate) interface_input_artifact_label: String,
    pub(crate) turn_control_artifact_label: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalSubmitAck {
    pub(crate) addressed_run: octopus_runtime::RunId,
    pub(crate) admitted_seq: u64,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TerminalSubmitError {
    #[error("terminal input artifact could not be materialized: {0}")]
    Materialize(String),
    #[error(
        "terminal has no client interface id; refusing to submit without a descriptor-selected interface"
    )]
    NoInterface,
    #[error("handing the input artifact to the octopus-app daemon failed: {0}")]
    Daemon(#[from] DaemonArrivalError),
}

fn default_durable_root() -> PathBuf {
    if let Some(root) = std::env::var_os("OCTOPUS_HOME") {
        return PathBuf::from(root).join("ledger");
    }
    octopus_utils_home_dir::find_octopus_home()
        .map(|home| home.join("ledger").to_path_buf())
        .unwrap_or_else(|_| PathBuf::from(".octopus").join("ledger"))
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReplyRenderLane {
    TurnStarted,
    MessageDelta {
        artifact_label: Option<String>,
        index: usize,
        content: octopus_ledger::ContentHash,
    },
    ExecOutputDelta {
        artifact_label: Option<String>,
        index: usize,
        content: octopus_ledger::ContentHash,
    },
    MarkOutcomeItem {
        mark: String,
    },
    Approval {
        artifact_label: String,
        request: ApprovalRequest,
    },
    Elicitation {
        artifact_label: String,
        request: ElicitationRequest,
    },
    UserInputRequest {
        artifact_label: String,
        request: UserInputRequest,
    },
    OperatorRequestResolved {
        artifact_label: String,
        request_id: String,
    },
    HookResult {
        artifact_label: String,
        item: HookRunItem,
    },
    ToolCall {
        artifact_label: String,
        item: ToolCallItem,
    },
    HookCatalog {
        artifact_label: String,
        catalog: HookCatalog,
    },
    TurnCompleted {
        content: octopus_ledger::ContentHash,
    },

    Reasoning {
        item: ReasoningItem,
    },
    PlanUpdate {
        item: PlanUpdateItem,
    },
    RichExec {
        item: ExecCommandItem,
        summary: String,
    },
    TokenUsage {
        item: TokenUsageItem,
    },
    UserMessage {
        item: UserMessageItem,
    },
    ProviderError {
        message: String,
    },
    RecordedArtifact {
        label: String,
        text: String,
    },

    WorkerStarted {
        role: String,
        run: String,
    },
    WorkerToolArtifact {
        role: String,
        run: String,
        count: u32,
    },
    WorkerMark {
        role: String,
        run: String,
        mark: String,
    },
    WorkerCompleted {
        role: String,
        run: String,
        reply: String,
    },
}

impl ReplyRenderLane {
    pub fn server_notification_method(&self) -> &'static str {
        match self {
            Self::TurnStarted => "turn/started",
            Self::MessageDelta { .. } => "item/agentMessage/delta",
            Self::ExecOutputDelta { .. } => "item/commandExecution/outputDelta",
            Self::MarkOutcomeItem { .. } => "item/completed",
            Self::Approval { .. } => "operator/approval/request",
            Self::Elicitation { .. } => "operator/elicitation/request",
            Self::UserInputRequest { .. } => "operator/userInput/request",
            Self::OperatorRequestResolved { .. } => "operator/request/resolved",
            Self::HookResult { .. } => "hook/result",
            Self::ToolCall { .. } => "tool/activity",
            Self::HookCatalog { .. } => "hook/catalog",
            Self::TurnCompleted { .. } => "turn/completed",
            Self::Reasoning { .. }
            | Self::PlanUpdate { .. }
            | Self::RichExec { .. }
            | Self::TokenUsage { .. }
            | Self::UserMessage { .. }
            | Self::RecordedArtifact { .. } => "item/completed",
            Self::ProviderError { .. } => "error",
            Self::WorkerStarted { .. } => "subagent/started",
            Self::WorkerToolArtifact { .. } => "subagent/toolArtifact",
            Self::WorkerMark { .. } => "subagent/markAdmitted",
            Self::WorkerCompleted { .. } => "subagent/completed",
        }
    }
}

pub struct TerminalClientSpine {
    daemon: TerminalDaemonEmitter,
    artifact_root: PathBuf,
    durable_root: PathBuf,
    run: octopus_runtime::RunId,

    active_arrival_interface: Option<ActiveArrivalInterface>,
    client_interface: Option<(InterfaceId, InterfaceHostIoLimits)>,
}

impl TerminalClientSpine {
    pub(crate) fn new(run: octopus_runtime::RunId, durable_root: PathBuf) -> Self {
        let artifact_root = durable_root
            .parent()
            .map(octopus_world_terminal_client::terminal_hand_in_root_path)
            .unwrap_or_else(|| durable_root.join("interface-hand-in/world_terminal"));
        Self {
            daemon: TerminalDaemonEmitter::from_env(),
            artifact_root: artifact_root.join("terminal-input-artifacts"),
            durable_root,
            run,
            active_arrival_interface: None,
            client_interface: None,
        }
    }

    pub(crate) fn new_with_endpoint(
        run: octopus_runtime::RunId,
        endpoint: octopus_world_terminal_client::DaemonEndpoint,
    ) -> Self {
        let durable_root = endpoint.durable_ledger_root;
        Self {
            daemon: TerminalDaemonEmitter::with_socket_path(endpoint.control_socket),
            artifact_root: endpoint
                .artifact_hand_in_root
                .join("terminal-input-artifacts"),
            durable_root,
            run,
            active_arrival_interface: None,
            client_interface: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn new_with_emitter(
        run: octopus_runtime::RunId,
        durable_root: PathBuf,
        daemon: TerminalDaemonEmitter,
    ) -> Self {
        let artifact_root = durable_root
            .parent()
            .map(octopus_world_terminal_client::terminal_hand_in_root_path)
            .unwrap_or_else(|| durable_root.join("interface-hand-in/world_terminal"));
        Self {
            daemon,
            artifact_root: artifact_root.join("terminal-input-artifacts"),
            durable_root,
            run,
            active_arrival_interface: None,
            client_interface: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn new_default_root(run: octopus_runtime::RunId) -> Self {
        Self::new(run, default_durable_root())
    }

    pub(crate) fn with_active_arrival_interface(mut self, active: ActiveArrivalInterface) -> Self {
        self.active_arrival_interface = Some(active);
        self
    }

    pub(crate) fn active_arrival_interface(&self) -> Option<&ActiveArrivalInterface> {
        self.active_arrival_interface.as_ref()
    }

    pub(crate) fn with_client_interface(
        mut self,
        interface: InterfaceId,
        host_io_limits: InterfaceHostIoLimits,
    ) -> Self {
        self.client_interface = Some((interface, host_io_limits));
        self
    }

    pub(crate) fn client_interface(&self) -> Result<&InterfaceId, TerminalSubmitError> {
        self.client_interface
            .as_ref()
            .map(|(interface, _)| interface)
            .ok_or(TerminalSubmitError::NoInterface)
    }

    pub fn has_daemon(&self) -> bool {
        self.daemon.has_daemon_socket()
    }

    pub(crate) fn operator_responder(&self) -> octopus_world_terminal_client::OperatorResponder {
        octopus_world_terminal_client::OperatorResponder::new(
            self.artifact_root.join("interface-input-artifacts"),
            self.daemon.clone(),
        )
    }

    pub(crate) fn write_interface_input_reference(
        &self,
        artifact_label: &str,
        input: &InterfaceInputArtifactBody,
    ) -> Result<Vec<u8>, String> {
        octopus_world_terminal_client::write_terminal_input_reference(
            self.artifact_root.join("interface-input-artifacts"),
            artifact_label,
            input,
        )
        .map_err(|e| e.to_string())
    }

    pub(crate) async fn hand_input_to_daemon(
        &self,
        manifest_bytes: &[u8],
    ) -> Result<TerminalSubmitAck, TerminalSubmitError> {
        self.client_interface()?;
        let verdict = self.daemon.emit_input_manifest(manifest_bytes).await?;
        let octopus_interfaces::DeliveryVerdict::Admitted { seq } = verdict else {
            unreachable!("the terminal daemon emitter converts refused verdicts into errors")
        };
        Ok(TerminalSubmitAck {
            addressed_run: self.run.clone(),
            admitted_seq: seq,
        })
    }

    fn open_daemon_blobs(&self) -> Option<DurableBlobs> {
        DurableBlobs::open(self.durable_root.join("blobs")).ok()
    }

    fn open_daemon_ledger(&self) -> Option<DurableLog> {
        DurableLog::open(self.durable_root.join("movements.jsonl")).ok()
    }

    #[allow(dead_code)]
    pub(crate) fn run(&self) -> &octopus_runtime::RunId {
        &self.run
    }

    pub(crate) fn durable_root(&self) -> &Path {
        &self.durable_root
    }

    pub(crate) fn movements_snapshot(&self) -> Vec<Movement> {
        self.open_daemon_ledger()
            .map(|log| log.read_from(Seq(0)))
            .unwrap_or_default()
    }

    pub(crate) fn run_source(
        &self,
    ) -> std::sync::Arc<dyn crate::embedded_registry_read::RunSource> {
        std::sync::Arc::new(std::sync::Mutex::new(SnapshotLog::new(
            self.movements_snapshot(),
        ))) as std::sync::Arc<dyn crate::embedded_registry_read::RunSource>
    }

    pub fn blob_bytes(&self, content: &octopus_ledger::ContentHash) -> Option<Vec<u8>> {
        self.open_daemon_blobs()
            .and_then(|blobs| blobs.get(content))
    }

    fn terminal_cursor_home(&self) -> PathBuf {
        self.durable_root.clone()
    }

    pub fn spawn_terminal_outbound_consumer(
        &self,
    ) -> (
        tokio::sync::mpsc::UnboundedReceiver<
            Result<(ReplyRenderLane, Option<String>), TerminalOutboundError>,
        >,
        tokio::task::JoinHandle<()>,
    ) {
        let Some((interface, host_io_limits)) = self.client_interface.clone() else {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let _ = tx.send(Err(TerminalOutboundError::Open(
                "terminal client has no interface id; cannot scope the outbound reader".into(),
            )));
            let handle = tokio::spawn(async {});
            return (rx, handle);
        };
        let octopus_home = self.terminal_cursor_home();
        let durable_root = self.durable_root.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            let mut reader = match TerminalOutboundReader::open(
                interface,
                octopus_home,
                durable_root,
                host_io_limits,
            ) {
                Ok(reader) => reader,
                Err(error) => {
                    let _ = tx.send(Err(error));
                    return;
                }
            };
            let mut delta_index = 0usize;
            loop {
                let batch = match reader.read_new() {
                    Ok(batch) => batch,
                    Err(error) => {
                        let _ = tx.send(Err(error));
                        return;
                    }
                };
                let mut closed = false;
                for own in &batch {
                    let envelope = match reader.envelope(own) {
                        Ok(envelope) => envelope,
                        Err(error) => {
                            let _ = tx.send(Err(error));
                            return;
                        }
                    };
                    let (kind, resolved) = match outbound_resolved(&envelope) {
                        Ok(pair) => pair,
                        Err(message) => {
                            let _ = tx.send(Err(TerminalOutboundError::Envelope(message)));
                            return;
                        }
                    };
                    let label = envelope
                        .artifact
                        .id
                        .as_str()
                        .split_once(':')
                        .map_or(envelope.artifact.id.as_str(), |(label, _)| label)
                        .to_string();
                    let Some((lane, lane_text)) = terminal_outbound_lane(
                        kind,
                        resolved,
                        label,
                        own.movement
                            .artifact
                            .as_ref()
                            .map(|artifact| artifact.content),
                        &mut delta_index,
                    ) else {
                        continue;
                    };
                    if tx.send(Ok((lane, lane_text))).is_err() {
                        closed = true;
                        break;
                    }
                }
                if closed {
                    break;
                }
                if let Err(error) = reader.commit() {
                    let _ = tx.send(Err(error));
                    return;
                }
                if batch.is_empty() {
                    tokio::select! {
                        _ = tx.closed() => break,
                        result = reader.wait_for_change() => {
                            if let Err(error) = result {
                                let _ = tx.send(Err(error));
                                return;
                            }
                        }
                    }
                }
            }
        });
        (rx, handle)
    }
}

pub(crate) struct SnapshotLog {
    movements: Vec<Movement>,
}

impl SnapshotLog {
    fn new(movements: Vec<Movement>) -> Self {
        Self { movements }
    }
}

impl MovementLog for SnapshotLog {
    fn append(&mut self, _movement: octopus_ledger::MovementDraft) -> Movement {
        unreachable!("SnapshotLog is a read-only projection of the daemon ledger; it never appends")
    }

    fn read_from(&self, from: Seq) -> Vec<Movement> {
        self.movements
            .iter()
            .filter(|m| m.seq.0 >= from.0)
            .cloned()
            .collect()
    }

    fn len(&self) -> usize {
        self.movements.len()
    }
}

pub(crate) fn terminal_outbound_lane(
    kind: OutboundRenderKind,
    resolved: OutboundResolved,
    label: String,
    content: Option<octopus_ledger::ContentHash>,
    delta_index: &mut usize,
) -> Option<(ReplyRenderLane, Option<String>)> {
    match resolved {
        OutboundResolved::Invalid(message) => {
            return Some((ReplyRenderLane::ProviderError { message }, None));
        }
        OutboundResolved::Approval(request) => {
            return Some((
                ReplyRenderLane::Approval {
                    artifact_label: label,
                    request,
                },
                None,
            ));
        }
        OutboundResolved::Elicitation(request) => {
            return Some((
                ReplyRenderLane::Elicitation {
                    artifact_label: label,
                    request,
                },
                None,
            ));
        }
        OutboundResolved::UserInput(request) => {
            return Some((
                ReplyRenderLane::UserInputRequest {
                    artifact_label: label,
                    request,
                },
                None,
            ));
        }
        OutboundResolved::OperatorRequestResolved(item) => {
            return Some((
                ReplyRenderLane::OperatorRequestResolved {
                    artifact_label: label,
                    request_id: item.request_id,
                },
                None,
            ));
        }
        OutboundResolved::Hook(item) => {
            return Some((
                ReplyRenderLane::HookResult {
                    artifact_label: label,
                    item,
                },
                None,
            ));
        }
        OutboundResolved::Tool(item) => {
            return Some((
                ReplyRenderLane::ToolCall {
                    artifact_label: label,
                    item,
                },
                None,
            ));
        }
        OutboundResolved::HookCatalog(catalog) => {
            return Some((
                ReplyRenderLane::HookCatalog {
                    artifact_label: label,
                    catalog,
                },
                None,
            ));
        }
        OutboundResolved::Reasoning(item) => {
            return Some((ReplyRenderLane::Reasoning { item }, None));
        }
        OutboundResolved::Plan(item) => {
            return Some((ReplyRenderLane::PlanUpdate { item }, None));
        }
        OutboundResolved::Tokens(item) => {
            return Some((ReplyRenderLane::TokenUsage { item }, None));
        }
        OutboundResolved::Exec { item, summary } => {
            return Some((ReplyRenderLane::RichExec { item, summary }, None));
        }
        OutboundResolved::UserMessage(_) => return None,
        OutboundResolved::Text(_) => {}
    }
    let text = match resolved {
        OutboundResolved::Text(text) => text,
        _ => String::new(),
    };
    let content = content.unwrap_or_else(|| octopus_ledger::ContentHash::of_bytes(text.as_bytes()));
    match kind {
        OutboundRenderKind::MessageDelta
        | OutboundRenderKind::Reasoning
        | OutboundRenderKind::TokenUsage
        | OutboundRenderKind::PlanUpdate => {
            let index = *delta_index;
            *delta_index += 1;
            Some((
                ReplyRenderLane::MessageDelta {
                    artifact_label: None,
                    index,
                    content,
                },
                Some(text),
            ))
        }
        OutboundRenderKind::CommandExecution | OutboundRenderKind::ExecCommand => {
            let index = *delta_index;
            *delta_index += 1;
            Some((
                ReplyRenderLane::ExecOutputDelta {
                    artifact_label: None,
                    index,
                    content,
                },
                Some(text),
            ))
        }
        OutboundRenderKind::TerminalReply => {
            Some((ReplyRenderLane::TurnCompleted { content }, Some(text)))
        }
        OutboundRenderKind::ProviderError => {
            Some((ReplyRenderLane::ProviderError { message: text }, None))
        }
        OutboundRenderKind::AuthRecoveryStarted | OutboundRenderKind::AuthRecoveryCompleted => {
            Some((ReplyRenderLane::RecordedArtifact { label, text }, None))
        }
        OutboundRenderKind::UserMessage => None,
        OutboundRenderKind::ApprovalRequest
        | OutboundRenderKind::ElicitationRequest
        | OutboundRenderKind::UserInputRequest
        | OutboundRenderKind::OperatorRequestResolved
        | OutboundRenderKind::HookResult
        | OutboundRenderKind::ToolCall
        | OutboundRenderKind::DynamicToolCall
        | OutboundRenderKind::McpToolCall
        | OutboundRenderKind::HookCatalog => {
            Some((ReplyRenderLane::RecordedArtifact { label, text }, None))
        }
        OutboundRenderKind::Other => {
            Some((ReplyRenderLane::RecordedArtifact { label, text }, None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_ledger::ContentHash;

    fn hash(text: &str) -> ContentHash {
        ContentHash::of_bytes(text.as_bytes())
    }

    #[test]
    fn terminal_outbound_lane_maps_kind_to_render_lane() {
        let mut index = 0usize;
        let content = Some(hash("m0"));

        let (lane, text) = terminal_outbound_lane(
            OutboundRenderKind::MessageDelta,
            OutboundResolved::Text("hi".into()),
            "agent.message.delta".into(),
            content,
            &mut index,
        )
        .unwrap();
        assert!(matches!(
            lane,
            ReplyRenderLane::MessageDelta { index: 0, .. }
        ));
        assert_eq!(lane.server_notification_method(), "item/agentMessage/delta");
        assert_eq!(text.as_deref(), Some("hi"));
        assert_eq!(index, 1);

        let (lane, text) = terminal_outbound_lane(
            OutboundRenderKind::CommandExecution,
            OutboundResolved::Text("stdout".into()),
            "command_execution.completed".into(),
            Some(hash("e1")),
            &mut index,
        )
        .unwrap();
        assert!(matches!(
            lane,
            ReplyRenderLane::ExecOutputDelta { index: 1, .. }
        ));
        assert_eq!(
            lane.server_notification_method(),
            "item/commandExecution/outputDelta"
        );
        assert_eq!(text.as_deref(), Some("stdout"));
        assert_eq!(index, 2);

        let (lane, text) = terminal_outbound_lane(
            OutboundRenderKind::Other,
            OutboundResolved::Text("unknown label body".into()),
            "future.event".into(),
            Some(hash("o2")),
            &mut index,
        )
        .unwrap();
        assert!(matches!(lane, ReplyRenderLane::RecordedArtifact { .. }));
        assert!(text.is_none());
        assert_eq!(index, 2);
    }

    #[test]
    fn rich_turn_items_route_to_their_own_lanes() {
        let mut index = 0usize;

        let (lane, text) = terminal_outbound_lane(
            OutboundRenderKind::Reasoning,
            OutboundResolved::Reasoning(ReasoningItem {
                summary: vec!["plan it".into()],
                ..Default::default()
            }),
            "turn.reasoning".into(),
            None,
            &mut index,
        )
        .unwrap();
        assert!(matches!(lane, ReplyRenderLane::Reasoning { .. }));
        assert!(text.is_none());
        assert_eq!(index, 0);

        let (lane, _) = terminal_outbound_lane(
            OutboundRenderKind::PlanUpdate,
            OutboundResolved::Plan(PlanUpdateItem {
                text: "1. read".into(),
                ..Default::default()
            }),
            "turn.plan.update".into(),
            None,
            &mut index,
        )
        .unwrap();
        assert!(matches!(lane, ReplyRenderLane::PlanUpdate { .. }));

        let (lane, _) = terminal_outbound_lane(
            OutboundRenderKind::TokenUsage,
            OutboundResolved::Tokens(TokenUsageItem::default()),
            "turn.token_usage".into(),
            None,
            &mut index,
        )
        .unwrap();
        assert!(matches!(lane, ReplyRenderLane::TokenUsage { .. }));

        let (lane, _) = terminal_outbound_lane(
            OutboundRenderKind::ExecCommand,
            OutboundResolved::Exec {
                item: ExecCommandItem {
                    command: "ls".into(),
                    ..Default::default()
                },
                summary: "$ ls [running]".into(),
            },
            "turn.exec.command".into(),
            None,
            &mut index,
        )
        .unwrap();
        match lane {
            ReplyRenderLane::RichExec { item, summary } => {
                assert_eq!(item.command, "ls");
                assert!(summary.contains("ls"));
            }
            other => panic!("expected a rich exec lane, got {other:?}"),
        }
    }

    #[test]
    fn undecoded_rich_label_falls_back_to_prose() {
        let mut index = 0usize;
        let (lane, text) = terminal_outbound_lane(
            OutboundRenderKind::Reasoning,
            OutboundResolved::Text("raw reasoning body".into()),
            "turn.reasoning".into(),
            None,
            &mut index,
        )
        .unwrap();
        assert!(matches!(lane, ReplyRenderLane::MessageDelta { .. }));
        assert_eq!(text.as_deref(), Some("raw reasoning body"));
        assert_eq!(index, 1);
    }

    #[test]
    fn terminal_reply_maps_to_turn_completed_without_advancing_index() {
        let mut index = 5usize;
        let (lane, text) = terminal_outbound_lane(
            OutboundRenderKind::TerminalReply,
            OutboundResolved::Text("final".into()),
            "interface.artifact.output".into(),
            Some(hash("r")),
            &mut index,
        )
        .unwrap();
        assert!(matches!(lane, ReplyRenderLane::TurnCompleted { .. }));
        assert_eq!(lane.server_notification_method(), "turn/completed");
        assert_eq!(text.as_deref(), Some("final"));
        assert_eq!(index, 5);
    }

    #[test]
    fn missing_content_hash_falls_back_to_text_hash() {
        let mut index = 0usize;
        let (lane, _text) = terminal_outbound_lane(
            OutboundRenderKind::MessageDelta,
            OutboundResolved::Text("body".into()),
            "agent.message.delta".into(),
            None,
            &mut index,
        )
        .unwrap();
        match lane {
            ReplyRenderLane::MessageDelta { content, .. } => {
                assert_eq!(content, ContentHash::of_bytes(b"body"));
            }
            other => panic!("expected a message delta lane, got {other:?}"),
        }
    }

    #[test]
    fn provider_context_artifacts_do_not_enter_the_chat_lane() {
        let mut index = 0usize;
        assert!(
            terminal_outbound_lane(
                OutboundRenderKind::UserMessage,
                OutboundResolved::Text("rendered context".into()),
                "turn.user_message".into(),
                None,
                &mut index,
            )
            .is_none()
        );
        let (lane, text) = terminal_outbound_lane(
            OutboundRenderKind::HookCatalog,
            OutboundResolved::HookCatalog(HookCatalog::default()),
            "hook.catalog".into(),
            None,
            &mut index,
        )
        .unwrap();
        assert!(matches!(lane, ReplyRenderLane::HookCatalog { .. }));
        assert!(text.is_none());
        assert_eq!(index, 0);
    }
}
