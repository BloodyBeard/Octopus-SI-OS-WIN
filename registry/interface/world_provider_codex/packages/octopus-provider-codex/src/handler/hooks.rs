use super::*;

pub(crate) fn hook_result_output(
    run: &octopus_appserver_proto::HookRunSummary,
    notification: &'static str,
    turn_id: Option<&str>,
) -> CodexTurnOutput {
    use octopus_appserver_proto::HookOutputEntryKind as K;
    use octopus_appserver_proto::HookRunStatus as S;
    let event = hook_event_wire(run.event_name);
    let status = match run.status {
        S::Running => "running",
        S::Completed => "completed",
        S::Failed => "failed",
        S::Blocked => "blocked",
        S::Stopped => "stopped",
    };
    let outputs: Vec<serde_json::Value> = run
        .entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "kind": match entry.kind {
                    K::Feedback => "feedback",
                    K::Warning => "warning",
                    K::Stop => "stop",
                    K::Context => "context",
                    K::Error => "error",
                },
                "text": entry.text,
            })
        })
        .collect();
    CodexTurnOutput::client(
        HOOK_RESULT_LABEL,
        serde_json::json!({
            "id": run.id,
            "event": event,
            "notification": notification,
            "turn_id": turn_id,
            "handler_type": hook_handler_type_wire(run.handler_type),
            "execution_mode": hook_execution_mode_wire(run.execution_mode),
            "scope": hook_scope_wire(run.scope),
            "source": hook_source_wire(run.source),
            "source_path": serde_json::json!(run.source_path),
            "display_order": run.display_order,
            "status": status,
            "status_message": run.status_message,
            "started_at": run.started_at,
            "completed_at": run.completed_at,
            "duration_ms": run.duration_ms,
            "outputs": outputs,
        }),
    )
}

pub(crate) fn hook_event_wire(event: octopus_appserver_proto::HookEventName) -> &'static str {
    use octopus_appserver_proto::HookEventName as E;
    match event {
        E::PreToolUse => "PreToolUse",
        E::PermissionRequest => "PermissionRequest",
        E::PostToolUse => "PostToolUse",
        E::PreCompact => "PreCompact",
        E::PostCompact => "PostCompact",
        E::SessionStart => "SessionStart",
        E::SessionEnd => "SessionEnd",
        E::UserPromptSubmit => "UserPromptSubmit",
        E::SubagentStart => "SubagentStart",
        E::SubagentStop => "SubagentStop",
        E::Stop => "Stop",
        E::Interrupt => "Interrupt",
    }
}

pub(crate) fn hook_handler_type_wire(handler: octopus_appserver_proto::HookHandlerType) -> &'static str {
    use octopus_appserver_proto::HookHandlerType as H;
    match handler {
        H::Command => "command",
        H::McpTool => "mcp_tool",
        H::Prompt => "prompt",
        H::Agent => "agent",
    }
}

pub(crate) fn hook_execution_mode_wire(mode: octopus_appserver_proto::HookExecutionMode) -> &'static str {
    use octopus_appserver_proto::HookExecutionMode as M;
    match mode {
        M::Sync => "sync",
        M::Async => "async",
    }
}

pub(crate) fn hook_scope_wire(scope: octopus_appserver_proto::HookScope) -> &'static str {
    use octopus_appserver_proto::HookScope as S;
    match scope {
        S::Thread => "thread",
        S::Turn => "turn",
    }
}

pub(crate) fn hook_source_wire(source: HookSource) -> &'static str {
    match source {
        HookSource::System => "system",
        HookSource::User => "user",
        HookSource::Project => "project",
        HookSource::Mdm => "mdm",
        HookSource::SessionFlags => "sessionFlags",
        HookSource::Plugin => "plugin",
        HookSource::CloudRequirements => "cloudRequirements",
        HookSource::CloudManagedConfig => "cloudManagedConfig",
        HookSource::LegacyManagedConfigFile => "legacyManagedConfigFile",
        HookSource::LegacyManagedConfigMdm => "legacyManagedConfigMdm",
        HookSource::Unknown => "unknown",
    }
}

pub(crate) fn hook_trust_wire(trust: HookTrustStatus) -> &'static str {
    match trust {
        HookTrustStatus::Managed => "managed",
        HookTrustStatus::Trusted => "trusted",
        HookTrustStatus::Untrusted => "untrusted",
        HookTrustStatus::Modified => "modified",
    }
}

pub(crate) fn hook_catalog_output(entries: &[HooksListEntry]) -> CodexTurnOutput {
    let mut hooks: Vec<serde_json::Value> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut warning_entries: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<serde_json::Value> = Vec::new();
    let mut cwds: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        let cwd = entry.cwd.display().to_string();
        cwds.insert(cwd.clone());
        for warning in &entry.warnings {
            warnings.push(warning.clone());
            warning_entries.push(serde_json::json!({
                "cwd": cwd,
                "message": warning,
            }));
        }
        for error in &entry.errors {
            errors.push(serde_json::json!({
                "cwd": cwd,
                "path": error.path.display().to_string(),
                "message": error.message,
            }));
        }
        for hook in &entry.hooks {
            let (handler_type, execution_mode, handler, command) = match &hook.handler {
                HookHandlerMetadata::Command { command, r#async } => (
                    "command",
                    if *r#async { "async" } else { "sync" },
                    serde_json::json!({
                        "command": command,
                        "async": r#async,
                    }),
                    Some(command.as_str()),
                ),
                HookHandlerMetadata::McpTool { server, tool } => (
                    "mcp_tool",
                    "sync",
                    serde_json::json!({
                        "server": server,
                        "tool": tool,
                    }),
                    None,
                ),
                HookHandlerMetadata::Prompt {} => ("prompt", "sync", serde_json::json!({}), None),
                HookHandlerMetadata::Agent {} => ("agent", "sync", serde_json::json!({}), None),
            };
            hooks.push(serde_json::json!({
                "cwd": cwd,
                "key": hook.key,
                "event": hook_event_wire(hook.event_name),
                "handler_type": handler_type,
                "execution_mode": execution_mode,
                "handler": handler,
                "matcher": hook.matcher,
                "command": command,
                "timeout_sec": hook.timeout_sec,
                "status_message": hook.status_message,
                "additional_context_limit": hook.additional_context_limit,
                "source": hook_source_wire(hook.source),
                "source_path": serde_json::json!(hook.source_path),
                "plugin_id": hook.plugin_id,
                "display_order": hook.display_order,
                "enabled": hook.enabled,
                "is_managed": hook.is_managed,
                "current_hash": hook.current_hash,
                "trust": hook_trust_wire(hook.trust_status),
            }));
        }
    }
    CodexTurnOutput::client(
        HOOK_CATALOG_LABEL,
        serde_json::json!({
            "cwds": cwds,
            "hooks": hooks,
            "warnings": warnings,
            "warning_entries": warning_entries,
            "errors": errors,
        }),
    )
}

pub(crate) fn hook_catalog_error_output(error: &str) -> CodexTurnOutput {
    CodexTurnOutput::client(
        HOOK_CATALOG_ERROR_LABEL,
        serde_json::json!({
            "operation": "hooks/list",
            "error": {
                "message": error,
            },
        }),
    )
}

pub(crate) fn goal_status_output(goal: &octopus_appserver_proto::ThreadGoal) -> CodexTurnOutput {
    use octopus_appserver_proto::ThreadGoalStatus as G;
    CodexTurnOutput::client(
        GOAL_STATUS_LABEL,
        serde_json::json!({
            "objective": goal.objective,
            "status": match goal.status {
                G::Active => "active",
                G::Paused => "paused",
                G::Blocked => "blocked",
                G::UsageLimited => "usage_limited",
                G::BudgetLimited => "budget_limited",
                G::Complete => "complete",
            },
            "token_budget": goal.token_budget,
            "tokens_used": goal.tokens_used,
            "time_used_seconds": goal.time_used_seconds,
        }),
    )
}
