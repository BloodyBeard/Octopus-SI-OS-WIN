use std::collections::HashMap;
use std::path::PathBuf;

#[cfg(test)]
use crate::app_command::AppCommand as Op;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::approval_events::ExecPolicyAmendment;
use crate::approval_events::NetworkPolicyAmendment;
use crate::approval_events::PermissionGrantScope;
use crate::approval_events::RequestPermissionProfile;
use crate::approval_events::RequestPermissionsResponse;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::list_selection_view::ListSelectionView;
use crate::bottom_pane::list_selection_view::SelectionItem;
use crate::bottom_pane::list_selection_view::SelectionViewParams;
use crate::bottom_pane::popup_consts::accept_cancel_hint_line;
use crate::diff_model::FileChange;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell;
use crate::history_cell::ReviewDecision;
use crate::key_hint::KeyBinding;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::ApprovalKeymap;
use crate::keymap::ListKeymap;
use crate::keymap::primary_binding;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use octopus_substrate_contract::approvals_substrate::NetworkPolicyRuleAction;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_world_terminal_client::ApprovalDetail;
use octopus_world_terminal_client::ApprovalOptionItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

#[derive(Clone, Debug)]
pub(crate) enum ApprovalRequest {
    Exec {
        thread_id: String,
        thread_label: Option<String>,
        id: String,
        environment_id: Option<String>,
        command: Vec<String>,
        reason: Option<String>,
        options: Vec<ApprovalOptionItem>,
        detail: ApprovalDetail,
    },
    Permissions {
        thread_id: String,
        thread_label: Option<String>,
        call_id: String,
        environment_id: Option<String>,
        reason: Option<String>,
        permissions: RequestPermissionProfile,
    },
    ApplyPatch {
        thread_id: String,
        thread_label: Option<String>,
        id: String,
        reason: Option<String>,
        cwd: AbsolutePathBuf,
        changes: HashMap<PathBuf, FileChange>,
        options: Vec<ApprovalOptionItem>,
    },
}

impl ApprovalRequest {
    fn thread_id(&self) -> String {
        match self {
            ApprovalRequest::Exec { thread_id, .. }
            | ApprovalRequest::Permissions { thread_id, .. }
            | ApprovalRequest::ApplyPatch { thread_id, .. } => thread_id.clone(),
        }
    }

    fn thread_label(&self) -> Option<&str> {
        match self {
            ApprovalRequest::Exec { thread_label, .. }
            | ApprovalRequest::Permissions { thread_label, .. }
            | ApprovalRequest::ApplyPatch { thread_label, .. } => thread_label.as_deref(),
        }
    }

    pub(super) fn request_id(&self) -> &str {
        match self {
            ApprovalRequest::Exec { id, .. } | ApprovalRequest::ApplyPatch { id, .. } => id,
            ApprovalRequest::Permissions { call_id, .. } => call_id,
        }
    }
}

pub(crate) struct ApprovalOverlay {
    current_request: Option<ApprovalRequest>,
    queue: Vec<ApprovalRequest>,
    app_event_tx: AppEventSender,
    list: ListSelectionView,
    options: Vec<ApprovalOption>,
    current_complete: bool,
    done: bool,
    approval_keymap: ApprovalKeymap,
    list_keymap: ListKeymap,
}

impl ApprovalOverlay {
    pub fn new(
        request: ApprovalRequest,
        app_event_tx: AppEventSender,
        approval_keymap: ApprovalKeymap,
        list_keymap: ListKeymap,
    ) -> Self {
        let mut view = Self {
            current_request: None,
            queue: Vec::new(),
            app_event_tx: app_event_tx.clone(),
            list: ListSelectionView::new(Default::default(), app_event_tx, list_keymap.clone()),
            options: Vec::new(),
            current_complete: false,
            done: false,
            approval_keymap,
            list_keymap,
        };
        view.set_current(request);
        view
    }

    pub fn enqueue_request(&mut self, req: ApprovalRequest) {
        self.queue.push(req);
    }

    fn set_current(&mut self, request: ApprovalRequest) {
        self.current_complete = false;
        let header = build_header(&request);
        let (options, params) =
            Self::build_options(&request, header, &self.approval_keymap, &self.list_keymap);
        self.current_request = Some(request);
        self.options = options;
        self.list =
            ListSelectionView::new(params, self.app_event_tx.clone(), self.list_keymap.clone());
    }

    fn build_options(
        request: &ApprovalRequest,
        header: Box<dyn Renderable>,
        approval_keymap: &ApprovalKeymap,
        list_keymap: &ListKeymap,
    ) -> (Vec<ApprovalOption>, SelectionViewParams) {
        let (options, title) = match request {
            ApprovalRequest::Exec {
                options, detail, ..
            } => (
                exec_options(options, detail, approval_keymap),
                detail.network.as_ref().map_or_else(
                    || "Would you like to run the following command?".to_string(),
                    |network| {
                        format!(
                            "Do you want to approve network access to \"{}\"?",
                            network.host
                        )
                    },
                ),
            ),
            ApprovalRequest::Permissions { .. } => (
                permissions_options(approval_keymap),
                "Would you like to grant these permissions?".to_string(),
            ),
            ApprovalRequest::ApplyPatch { options, .. } => (
                patch_options(options, approval_keymap),
                "Would you like to make the following edits?".to_string(),
            ),
        };

        let header = Box::new(ColumnRenderable::with([
            Line::from(title.bold()).into(),
            Line::from("").into(),
            header,
        ]));

        let items = options
            .iter()
            .map(|opt| SelectionItem {
                name: opt.label.clone(),
                display_shortcut: opt.shortcuts.first().copied(),
                dismiss_on_select: false,
                ..Default::default()
            })
            .collect();

        let params = SelectionViewParams {
            footer_hint: Some(approval_footer_hint(request, approval_keymap, list_keymap)),
            items,
            header,
            ..Default::default()
        };

        (options, params)
    }

    fn apply_selection(&mut self, actual_idx: usize) {
        if self.current_complete {
            return;
        }
        let Some(option) = self.options.get(actual_idx) else {
            return;
        };
        if let Some(request) = self.current_request.as_ref() {
            match (request, &option.decision) {
                (ApprovalRequest::Exec { id, command, .. }, ApprovalDecision::Opaque(decision)) => {
                    self.handle_exec_decision(id, command, decision.clone());
                }
                (
                    ApprovalRequest::Permissions {
                        call_id,
                        permissions,
                        ..
                    },
                    ApprovalDecision::Permissions(decision),
                ) => self.handle_permissions_decision(call_id, permissions, *decision),
                (ApprovalRequest::ApplyPatch { id, .. }, ApprovalDecision::Opaque(decision)) => {
                    self.handle_patch_decision(id, decision.clone());
                }
                _ => {}
            }
        }

        self.current_complete = true;
        self.advance_queue();
    }

    fn handle_exec_decision(&self, id: &str, command: &[String], option: ApprovalOptionItem) {
        let Some(request) = self.current_request.as_ref() else {
            return;
        };
        if request.thread_label().is_none() {
            let detail = match request {
                ApprovalRequest::Exec { detail, .. } => Some(detail),
                _ => None,
            };
            let subject = match detail.and_then(|detail| detail.network.as_ref()) {
                Some(network) => history_cell::ApprovalDecisionSubject::NetworkAccess {
                    target: network_approval_target(network, command),
                },
                None => {
                    if let Some(target) = network_approval_command_target(command) {
                        history_cell::ApprovalDecisionSubject::NetworkAccess {
                            target: target.to_string(),
                        }
                    } else {
                        history_cell::ApprovalDecisionSubject::Command(command.to_vec())
                    }
                }
            };
            let cell = history_cell::new_approval_decision_cell(
                subject,
                option_review_decision(&option, detail),
                history_cell::ApprovalDecisionActor::User,
            );
            self.app_event_tx.send(AppEvent::InsertHistoryCell(cell));
        }
        self.app_event_tx
            .approval_decision(id.to_string(), option.decision);
    }

    fn handle_permissions_decision(
        &self,
        call_id: &str,
        permissions: &RequestPermissionProfile,
        decision: PermissionsDecision,
    ) {
        let Some(request) = self.current_request.as_ref() else {
            return;
        };
        let granted_permissions = match decision {
            PermissionsDecision::GrantForTurn
            | PermissionsDecision::GrantForTurnWithStrictAutoReview
            | PermissionsDecision::GrantForSession => permissions.clone(),
            PermissionsDecision::Deny => Default::default(),
        };
        let scope = if matches!(decision, PermissionsDecision::GrantForSession) {
            PermissionGrantScope::Session
        } else {
            PermissionGrantScope::Turn
        };
        let strict_auto_review = matches!(
            decision,
            PermissionsDecision::GrantForTurnWithStrictAutoReview
        );
        if request.thread_label().is_none() {
            let message = if granted_permissions.is_empty() {
                "You did not grant additional permissions"
            } else if strict_auto_review {
                "You granted additional permissions with strict auto review"
            } else if matches!(scope, PermissionGrantScope::Session) {
                "You granted additional permissions for this session"
            } else {
                "You granted additional permissions"
            };
            self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                crate::history_cell::PlainHistoryCell::new(vec![message.into()]),
            )));
        }
        let response = RequestPermissionsResponse {
            permissions: granted_permissions,
            scope,
            strict_auto_review,
        };
        let decision_value = serde_json::to_value(&response)
            .unwrap_or_else(|_| serde_json::json!({ "permissions": {}, "scope": "turn" }));
        self.app_event_tx
            .approval_decision(call_id.to_string(), decision_value);
    }

    fn handle_patch_decision(&self, id: &str, option: ApprovalOptionItem) {
        self.app_event_tx
            .approval_decision(id.to_string(), option.decision);
    }

    fn advance_queue(&mut self) {
        if let Some(next) = self.queue.pop() {
            self.set_current(next);
        } else {
            self.done = true;
        }
    }

    fn dismiss_operator_request(&mut self, request_id: &str) -> bool {
        let queue_len = self.queue.len();
        self.queue
            .retain(|request| request.request_id() != request_id);
        if self
            .current_request
            .as_ref()
            .is_some_and(|request| request.request_id() == request_id)
        {
            self.current_complete = true;
            self.advance_queue();
            return true;
        }
        self.queue.len() != queue_len
    }

    fn cancel_current_request(&mut self) {
        if self.done {
            return;
        }
        if !self.current_complete
            && let Some(request) = self.current_request.as_ref()
        {
            match request {
                ApprovalRequest::Exec {
                    id,
                    command,
                    options,
                    ..
                } => {
                    let cancel = cancel_option(options);
                    self.handle_exec_decision(&id.clone(), &command.clone(), cancel);
                }
                ApprovalRequest::Permissions {
                    call_id,
                    permissions,
                    ..
                } => {
                    self.handle_permissions_decision(
                        &call_id.clone(),
                        &permissions.clone(),
                        PermissionsDecision::Deny,
                    );
                }
                ApprovalRequest::ApplyPatch { id, options, .. } => {
                    self.handle_patch_decision(&id.clone(), cancel_option(options));
                }
            }
        }
        self.queue.clear();
        self.done = true;
    }

    fn try_handle_shortcut(&mut self, key_event: &KeyEvent) -> bool {
        if key_event.kind == KeyEventKind::Press
            && self.approval_keymap.open_fullscreen.is_pressed(*key_event)
            && let Some(request) = self.current_request.as_ref()
        {
            self.app_event_tx
                .send(AppEvent::FullScreenApprovalRequest(request.clone()));
            return true;
        }

        if key_event.kind == KeyEventKind::Press
            && self.approval_keymap.open_thread.is_pressed(*key_event)
            && let Some(request) = self.current_request.as_ref()
            && request.thread_label().is_some()
        {
            self.app_event_tx
                .send(AppEvent::SelectAgentThread(request.thread_id()));
            return true;
        }

        if self.list_keymap.cancel.is_pressed(*key_event) {
            self.cancel_current_request();
            return true;
        }

        if let Some(idx) = self
            .options
            .iter()
            .position(|opt| opt.shortcuts.iter().any(|s| s.is_press(*key_event)))
        {
            self.apply_selection(idx);
            true
        } else {
            false
        }
    }
}

impl BottomPaneView for ApprovalOverlay {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.try_handle_shortcut(&key_event) {
            return;
        }
        self.list.handle_key_event(key_event);
        if let Some(idx) = self.list.take_last_selected_index() {
            self.apply_selection(idx);
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.cancel_current_request();
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.done
    }

    fn try_consume_approval_request(
        &mut self,
        request: ApprovalRequest,
    ) -> Option<ApprovalRequest> {
        self.enqueue_request(request);
        None
    }

    fn dismiss_operator_request(&mut self, request_id: &str) -> bool {
        ApprovalOverlay::dismiss_operator_request(self, request_id)
    }

    fn terminal_title_requires_action(&self) -> bool {
        true
    }
}

impl Renderable for ApprovalOverlay {
    fn desired_height(&self, width: u16) -> u16 {
        self.list.desired_height(width)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.list.render(area, buf);
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.list.cursor_pos(area)
    }
}

fn approval_footer_hint(
    request: &ApprovalRequest,
    approval_keymap: &ApprovalKeymap,
    list_keymap: &ListKeymap,
) -> Line<'static> {
    let mut spans = accept_cancel_hint_line(
        primary_binding(&list_keymap.accept),
        "to confirm",
        primary_binding(&list_keymap.cancel),
        "to cancel",
    )
    .spans;
    if request.thread_label().is_some()
        && let Some(open_thread) = primary_binding(&approval_keymap.open_thread)
    {
        if !spans.is_empty() {
            spans.push(" or ".into());
        } else {
            spans.push("Press ".into());
        }
        spans.extend([open_thread.into(), " to open thread".into()]);
    }
    Line::from(spans)
}

fn network_approval_target(
    network: &octopus_world_terminal_client::NetworkApprovalScope,
    command: &[String],
) -> String {
    if let Some(target) = network_approval_command_target(command) {
        return target.to_string();
    }
    network.target()
}

fn network_approval_command_target(command: &[String]) -> Option<&str> {
    match command {
        [program, target] if program == "network-access" && !target.is_empty() => {
            Some(target.as_str())
        }
        [command] => command
            .strip_prefix("network-access ")
            .filter(|target| !target.is_empty()),
        _ => None,
    }
}

fn build_header(request: &ApprovalRequest) -> Box<dyn Renderable> {
    match request {
        ApprovalRequest::Exec {
            thread_label,
            environment_id,
            reason,
            command,
            detail,
            ..
        } => {
            let mut header: Vec<Line<'static>> = Vec::new();
            if let Some(thread_label) = thread_label {
                header.push(Line::from(vec![
                    "Thread: ".into(),
                    thread_label.clone().bold(),
                ]));
                header.push(Line::from(""));
            }
            if let Some(environment_id) = environment_id {
                header.push(Line::from(vec![
                    "Environment: ".into(),
                    environment_id.clone().bold(),
                ]));
                header.push(Line::from(""));
            }
            if let Some(reason) = reason {
                header.push(Line::from(vec!["Reason: ".into(), reason.clone().italic()]));
                header.push(Line::from(""));
            }
            if let Some(rule_line) = detail
                .permissions
                .as_ref()
                .and_then(|permissions| permissions.rule_line())
            {
                header.push(Line::from(vec![
                    "Permission rule: ".into(),
                    rule_line.cyan(),
                ]));
                header.push(Line::from(""));
            }
            let full_cmd = strip_bash_lc_and_escape(command);
            let mut full_cmd_lines = highlight_bash_to_lines(&full_cmd);
            if let Some(first) = full_cmd_lines.first_mut() {
                first.spans.insert(0, Span::from("$ "));
            }
            if detail.network.is_none() {
                header.extend(full_cmd_lines);
            }
            Box::new(Paragraph::new(header).wrap(Wrap { trim: false }))
        }
        ApprovalRequest::Permissions {
            thread_label,
            environment_id,
            reason,
            permissions,
            ..
        } => {
            let mut header: Vec<Line<'static>> = Vec::new();
            if let Some(thread_label) = thread_label {
                header.push(Line::from(vec![
                    "Thread: ".into(),
                    thread_label.clone().bold(),
                ]));
                header.push(Line::from(""));
            }
            if let Some(environment_id) = environment_id {
                header.push(Line::from(vec![
                    "Environment: ".into(),
                    environment_id.clone().bold(),
                ]));
                header.push(Line::from(""));
            }
            if let Some(reason) = reason {
                header.push(Line::from(vec!["Reason: ".into(), reason.clone().italic()]));
                header.push(Line::from(""));
            }
            if let Some(rule_line) = format_requested_permissions_rule(permissions) {
                header.push(Line::from(vec![
                    "Permission rule: ".into(),
                    rule_line.cyan(),
                ]));
            }
            Box::new(Paragraph::new(header).wrap(Wrap { trim: false }))
        }
        ApprovalRequest::ApplyPatch {
            thread_label,
            reason,
            ..
        } => {
            let mut header: Vec<Box<dyn Renderable>> = Vec::new();
            if let Some(thread_label) = thread_label {
                header.push(Box::new(Line::from(vec![
                    "Thread: ".into(),
                    thread_label.clone().bold(),
                ])));
            }
            if let Some(reason) = reason
                && !reason.is_empty()
            {
                if !header.is_empty() {
                    header.push(Box::new(Line::from("")));
                }
                header.push(Box::new(
                    Paragraph::new(Line::from_iter([
                        "Reason: ".into(),
                        reason.clone().italic(),
                    ]))
                    .wrap(Wrap { trim: false }),
                ));
            }
            Box::new(ColumnRenderable::with(header))
        }
    }
}

#[derive(Clone)]
enum ApprovalDecision {
    Opaque(ApprovalOptionItem),
    Permissions(PermissionsDecision),
}

#[derive(Clone, Copy)]
enum PermissionsDecision {
    GrantForTurn,
    GrantForTurnWithStrictAutoReview,
    GrantForSession,
    Deny,
}

#[derive(Clone)]
struct ApprovalOption {
    label: String,
    decision: ApprovalDecision,
    shortcuts: Vec<KeyBinding>,
}

fn default_cancel_option() -> ApprovalOptionItem {
    ApprovalOptionItem {
        kind: "cancel".to_string(),
        label: None,
        command_prefix: None,
        decision: serde_json::json!("cancel"),
    }
}

fn cancel_option(options: &[ApprovalOptionItem]) -> ApprovalOptionItem {
    options
        .iter()
        .find(|option| option.kind == "cancel")
        .cloned()
        .unwrap_or_else(default_cancel_option)
}

fn option_review_decision(
    option: &ApprovalOptionItem,
    detail: Option<&ApprovalDetail>,
) -> ReviewDecision {
    match option.kind.as_str() {
        "accept_for_session" => ReviewDecision::ApprovedForSession,
        "accept_for_prefix" => match option.command_prefix.as_deref() {
            Some(prefix) => ReviewDecision::ApprovedExecpolicyAmendment {
                proposed_execpolicy_amendment: ExecPolicyAmendment::new(
                    shlex::split(prefix).unwrap_or_else(|| vec![prefix.to_string()]),
                ),
            },
            None => ReviewDecision::Approved,
        },
        "allow_host" | "block_host" => {
            let host = detail
                .and_then(|detail| detail.network.as_ref())
                .map(|network| network.host.clone())
                .unwrap_or_default();
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment: NetworkPolicyAmendment {
                    host,
                    action: if option.kind == "allow_host" {
                        NetworkPolicyRuleAction::Allow
                    } else {
                        NetworkPolicyRuleAction::Deny
                    },
                },
            }
        }
        "decline" => ReviewDecision::Denied,
        "cancel" => ReviewDecision::Abort,
        _ => ReviewDecision::Approved,
    }
}

fn exec_options(
    options: &[ApprovalOptionItem],
    detail: &ApprovalDetail,
    keymap: &ApprovalKeymap,
) -> Vec<ApprovalOption> {
    let has_network = detail.network.is_some();
    let has_permissions = detail.permissions.is_some();
    options
        .iter()
        .filter_map(|option| {
            let (label, shortcuts) = match option.kind.as_str() {
                "accept" => (
                    if has_network {
                        "Yes, just this once".to_string()
                    } else {
                        "Yes, proceed".to_string()
                    },
                    keymap.approve.clone(),
                ),
                "accept_for_prefix" => {
                    let rendered_prefix = option.command_prefix.clone().unwrap_or_default();
                    if rendered_prefix.is_empty()
                        || rendered_prefix.contains('\n')
                        || rendered_prefix.contains('\r')
                    {
                        return None;
                    }
                    (
                        format!(
                            "Yes, and don't ask again for commands that start with `{rendered_prefix}`"
                        ),
                        keymap.approve_for_prefix.clone(),
                    )
                }
                "accept_for_session" => (
                    if has_network {
                        "Yes, and allow this host for this conversation".to_string()
                    } else if has_permissions {
                        "Yes, and allow these permissions for this session".to_string()
                    } else {
                        "Yes, and don't ask again for this command in this session".to_string()
                    },
                    keymap.approve_for_session.clone(),
                ),
                "allow_host" => (
                    "Yes, and allow this host in the future".to_string(),
                    keymap.approve_for_prefix.clone(),
                ),
                "block_host" => (
                    "No, and block this host in the future".to_string(),
                    keymap.deny.clone(),
                ),
                "decline" => (
                    "No, continue without running it".to_string(),
                    keymap.deny.clone(),
                ),
                "cancel" => (
                    "No, and tell Codex what to do differently".to_string(),
                    keymap.decline.clone(),
                ),
                _ => (
                    option
                        .label
                        .clone()
                        .unwrap_or_else(|| option.kind.clone()),
                    Vec::new(),
                ),
            };
            let label = option.label.clone().unwrap_or(label);
            Some(ApprovalOption {
                label,
                decision: ApprovalDecision::Opaque(option.clone()),
                shortcuts,
            })
        })
        .collect()
}

pub(crate) fn format_requested_permissions_rule(
    permissions: &RequestPermissionProfile,
) -> Option<String> {
    use octopus_substrate_contract::permissions::FileSystemAccessMode;
    use octopus_substrate_contract::permissions::FileSystemPath;
    use octopus_substrate_contract::permissions::FileSystemSpecialPath;

    fn path_label(base: &str, subpath: &Option<PathBuf>) -> String {
        match subpath {
            Some(subpath) => format!("{base}/{}", subpath.display()),
            None => base.to_string(),
        }
    }

    fn special_path_label(value: &FileSystemSpecialPath) -> String {
        match value {
            FileSystemSpecialPath::Root => ":root".to_string(),
            FileSystemSpecialPath::Minimal => ":minimal".to_string(),
            FileSystemSpecialPath::ProjectRoots { subpath } => {
                path_label(":workspace_roots", subpath)
            }
            FileSystemSpecialPath::Tmpdir => ":tmpdir".to_string(),
            FileSystemSpecialPath::SlashTmp => "/tmp".to_string(),
            FileSystemSpecialPath::Unknown { path, subpath } => path_label(path, subpath),
        }
    }

    let mut parts = Vec::new();
    if permissions
        .network
        .as_ref()
        .and_then(|network| network.enabled)
        .unwrap_or(false)
    {
        parts.push("network".to_string());
    }
    if let Some(file_system) = permissions.file_system.as_ref() {
        let format_paths = |mode: FileSystemAccessMode| {
            file_system
                .entries
                .iter()
                .filter(|entry| entry.access == mode)
                .map(|entry| match &entry.path {
                    FileSystemPath::Path { path } => format!("`{}`", path.as_path().display()),
                    FileSystemPath::GlobPattern { pattern } => format!("glob `{pattern}`"),
                    FileSystemPath::Special { value } => {
                        format!("`{}`", special_path_label(value))
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let reads = format_paths(FileSystemAccessMode::Read);
        if !reads.is_empty() {
            parts.push(format!("read {reads}"));
        }
        let writes = format_paths(FileSystemAccessMode::Write);
        if !writes.is_empty() {
            parts.push(format!("write {writes}"));
        }
        let denied_reads = format_paths(FileSystemAccessMode::Deny);
        if !denied_reads.is_empty() {
            parts.push(format!("deny read {denied_reads}"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

fn patch_options(options: &[ApprovalOptionItem], keymap: &ApprovalKeymap) -> Vec<ApprovalOption> {
    let defaults = [
        ApprovalOptionItem {
            kind: "accept".to_string(),
            label: None,
            command_prefix: None,
            decision: serde_json::json!("accept"),
        },
        ApprovalOptionItem {
            kind: "accept_for_session".to_string(),
            label: None,
            command_prefix: None,
            decision: serde_json::json!("acceptForSession"),
        },
        default_cancel_option(),
    ];
    let options = if options.is_empty() {
        defaults.as_slice()
    } else {
        options
    };
    options
        .iter()
        .map(|option| {
            let (label, shortcuts) = match option.kind.as_str() {
                "accept" => ("Yes, proceed".to_string(), keymap.approve.clone()),
                "accept_for_session" => (
                    "Yes, and don't ask again for these files".to_string(),
                    keymap.approve_for_session.clone(),
                ),
                "decline" => (
                    "No, continue without making these edits".to_string(),
                    keymap.deny.clone(),
                ),
                "cancel" => (
                    "No, and tell Codex what to do differently".to_string(),
                    keymap.decline.clone(),
                ),
                other => (other.to_string(), Vec::new()),
            };
            ApprovalOption {
                label: option.label.clone().unwrap_or(label),
                decision: ApprovalDecision::Opaque(option.clone()),
                shortcuts,
            }
        })
        .collect()
}

fn permissions_options(keymap: &ApprovalKeymap) -> Vec<ApprovalOption> {
    let deny_shortcuts = keymap
        .deny
        .iter()
        .copied()
        .filter(|shortcut| shortcut.parts() != (KeyCode::Esc, KeyModifiers::NONE))
        .collect();

    vec![
        ApprovalOption {
            label: "Yes, grant these permissions for this turn".to_string(),
            decision: ApprovalDecision::Permissions(PermissionsDecision::GrantForTurn),
            shortcuts: keymap.approve.clone(),
        },
        ApprovalOption {
            label: "Yes, grant for this turn with strict auto review".to_string(),
            decision: ApprovalDecision::Permissions(
                PermissionsDecision::GrantForTurnWithStrictAutoReview,
            ),
            shortcuts: vec![crate::key_hint::plain(KeyCode::Char('r'))],
        },
        ApprovalOption {
            label: "Yes, grant these permissions for this session".to_string(),
            decision: ApprovalDecision::Permissions(PermissionsDecision::GrantForSession),
            shortcuts: keymap.approve_for_session.clone(),
        },
        ApprovalOption {
            label: "No, continue without permissions".to_string(),
            decision: ApprovalDecision::Permissions(PermissionsDecision::Deny),
            shortcuts: deny_shortcuts,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval_events::default_approval_options;
    use octopus_world_terminal_client::NetworkApprovalScope;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::mpsc::unbounded_channel;

    fn test_sender() -> (AppEventSender, UnboundedReceiver<AppEvent>) {
        let (tx_raw, rx) = unbounded_channel::<AppEvent>();
        (AppEventSender::new(tx_raw), rx)
    }

    fn make_overlay(request: ApprovalRequest) -> (ApprovalOverlay, UnboundedReceiver<AppEvent>) {
        let (tx, rx) = test_sender();
        let keymaps = crate::keymap::RuntimeKeymap::defaults();
        (
            ApprovalOverlay::new(request, tx, keymaps.approval, keymaps.list),
            rx,
        )
    }

    fn exec_request(detail: ApprovalDetail) -> ApprovalRequest {
        let options = default_approval_options(&detail);
        ApprovalRequest::Exec {
            thread_id: String::default(),
            thread_label: None,
            id: "request-1".to_string(),
            environment_id: None,
            command: vec!["echo".into(), "ok".into()],
            reason: None,
            options,
            detail,
        }
    }

    fn next_approval_decision(rx: &mut UnboundedReceiver<AppEvent>) -> (String, serde_json::Value) {
        loop {
            match rx.try_recv().expect("expected an approval decision op") {
                AppEvent::CodexOp(Op::ApprovalDecision {
                    request_id,
                    decision,
                }) => return (request_id, decision),
                _ => continue,
            }
        }
    }

    #[test]
    fn selection_emits_opaque_decision() {
        let (mut overlay, mut rx) = make_overlay(exec_request(ApprovalDetail::default()));

        overlay.apply_selection(0);

        let (request_id, decision) = next_approval_decision(&mut rx);
        assert_eq!(request_id, "request-1");
        assert_eq!(decision, serde_json::json!("accept"));
        assert!(overlay.is_complete());
    }

    #[test]
    fn ctrl_c_aborts_and_emits_cancel_decision() {
        let (mut overlay, mut rx) = make_overlay(exec_request(ApprovalDetail::default()));
        overlay.enqueue_request(exec_request(ApprovalDetail::default()));

        assert_eq!(overlay.on_ctrl_c(), CancellationEvent::Handled);

        let (request_id, decision) = next_approval_decision(&mut rx);
        assert_eq!(request_id, "request-1");
        assert_eq!(decision, serde_json::json!("cancel"));
        assert!(overlay.is_complete());
    }

    #[test]
    fn network_detail_shapes_options_and_title() {
        let detail = ApprovalDetail {
            network: Some(NetworkApprovalScope {
                host: "example.com".to_string(),
                protocol: "https".to_string(),
                port: None,
            }),
            ..Default::default()
        };
        let options = default_approval_options(&detail);
        let keymaps = crate::keymap::RuntimeKeymap::defaults();
        let rendered = exec_options(&options, &detail, &keymaps.approval);
        assert_eq!(
            rendered
                .iter()
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Yes, just this once",
                "Yes, and allow this host for this conversation",
                "No, and tell Codex what to do differently",
            ]
        );
    }

    #[test]
    fn permissions_grant_emits_serialized_response() {
        let request = ApprovalRequest::Permissions {
            thread_id: String::default(),
            thread_label: None,
            call_id: "call-7".to_string(),
            environment_id: None,
            reason: None,
            permissions: RequestPermissionProfile::default(),
        };
        let (mut overlay, mut rx) = make_overlay(request);

        overlay.apply_selection(0);

        let (request_id, decision) = next_approval_decision(&mut rx);
        assert_eq!(request_id, "call-7");
        assert_eq!(
            decision,
            serde_json::to_value(&RequestPermissionsResponse {
                permissions: RequestPermissionProfile::default(),
                scope: PermissionGrantScope::Turn,
                strict_auto_review: false,
            })
            .unwrap()
        );
    }

    #[test]
    fn option_kinds_map_to_review_decisions() {
        let prefix_option = ApprovalOptionItem {
            kind: "accept_for_prefix".to_string(),
            label: None,
            command_prefix: Some("git status".to_string()),
            decision: serde_json::json!("accept_for_prefix"),
        };
        assert!(matches!(
            option_review_decision(&prefix_option, None),
            ReviewDecision::ApprovedExecpolicyAmendment {
                proposed_execpolicy_amendment,
            } if proposed_execpolicy_amendment.command == vec!["git", "status"]
        ));

        let detail = ApprovalDetail {
            network: Some(NetworkApprovalScope {
                host: "example.com".to_string(),
                protocol: "https".to_string(),
                port: None,
            }),
            ..Default::default()
        };
        let block_option = ApprovalOptionItem {
            kind: "block_host".to_string(),
            label: None,
            command_prefix: None,
            decision: serde_json::json!("block_host"),
        };
        assert!(matches!(
            option_review_decision(&block_option, Some(&detail)),
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } if network_policy_amendment.host == "example.com"
                && network_policy_amendment.action == NetworkPolicyRuleAction::Deny
        ));
    }
}
