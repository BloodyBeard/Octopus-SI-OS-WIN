import hashlib
import ipaddress
import json
import os
import re


CHANNEL_RE = re.compile(r"^[CDG][A-Z0-9]{8,32}$")
TEAM_RE = re.compile(r"^T[A-Z0-9]{8,32}$")
USER_RE = re.compile(r"^[UW][A-Z0-9]{8,32}$")
TS_RE = re.compile(r"^[0-9]{1,20}\.[0-9]{1,12}$")


def require_object(value, name):
    if not isinstance(value, dict):
        raise ValueError(f"{name} must be a JSON object")
    return value


def require_keys(value, required, optional, name):
    value = require_object(value, name)
    missing = sorted(set(required) - set(value))
    unknown = sorted(set(value) - set(required) - set(optional))
    if missing:
        raise ValueError(f"{name} is missing keys: {', '.join(missing)}")
    if unknown:
        raise ValueError(f"{name} has unknown keys: {', '.join(unknown)}")
    return value


def require_string(value, name, allow_empty=False, maximum=None):
    if not isinstance(value, str) or value.strip() != value or (not allow_empty and not value):
        raise ValueError(f"{name} must be a{' possibly empty' if allow_empty else ' non-empty'} trimmed string")
    if maximum is not None and len(value) > maximum:
        raise ValueError(f"{name} exceeds {maximum} characters")
    return value


def require_bool(value, name):
    if not isinstance(value, bool):
        raise ValueError(f"{name} must be a boolean")
    return value


def require_int(value, name, minimum=None, maximum=None):
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{name} must be an integer")
    if minimum is not None and value < minimum:
        raise ValueError(f"{name} must be at least {minimum}")
    if maximum is not None and value > maximum:
        raise ValueError(f"{name} must be at most {maximum}")
    return value


def require_string_list(value, name, allow_empty=False):
    if not isinstance(value, list) or (not allow_empty and not value):
        raise ValueError(f"{name} must be an array")
    result = []
    seen = set()
    for index, item in enumerate(value):
        item = require_string(item, f"{name}[{index}]")
        if item in seen:
            raise ValueError(f"{name} repeats {item!r}")
        seen.add(item)
        result.append(item)
    return result


def authored_interface_binding(interface):
    return {
        "interface": require_string(interface, "interface binding identity"),
        "config": {"artifact_policy": {"mode": "all"}},
    }


def require_interface_binding(value, name):
    binding = require_keys(value, {"interface", "config"}, set(), name)
    interface = require_string(binding["interface"], f"{name}.interface")
    config = require_keys(binding["config"], {"artifact_policy"}, set(), f"{name}.config")
    policy = require_object(config["artifact_policy"], f"{name}.config.artifact_policy")
    mode = require_string(policy.get("mode"), f"{name}.config.artifact_policy.mode")
    if mode == "all":
        require_keys(policy, {"mode"}, set(), f"{name}.config.artifact_policy")
    elif mode == "allow":
        require_keys(policy, {"mode", "names", "namespaces"}, set(), f"{name}.config.artifact_policy")
        names = require_string_list(policy["names"], f"{name}.config.artifact_policy.names", True)
        namespaces = require_string_list(policy["namespaces"], f"{name}.config.artifact_policy.namespaces", True)
        if not names and not namespaces:
            raise ValueError(f"{name}.config.artifact_policy allow mode grants nothing")
        if any(namespace.endswith(".") for namespace in namespaces):
            raise ValueError(f"{name}.config.artifact_policy namespace must not end with '.'")
    else:
        raise ValueError(f"{name}.config.artifact_policy.mode must be all or allow")
    return interface


def require_nullable_string(value, name):
    if value is None:
        return None
    return require_string(value, name)


def _validate_id_list(values, name, pattern):
    for value in require_string_list(values, name, True):
        if not pattern.fullmatch(value):
            raise ValueError(f"{name} contains malformed Slack id {value!r}")


def validate_config(config, interface_id):
    require_keys(
        config,
        {"schema_version", "mode", "ledger", "transport", "authz", "gating", "intake", "routing", "media", "outbound", "private_run", "binding", "gateway_run_template"},
        set(),
        "Slack config",
    )
    if config["schema_version"] != 1:
        raise ValueError("Slack config schema_version must be 1")
    if config["mode"] not in {"gateway", "binding"}:
        raise ValueError("Slack config mode must be gateway or binding")
    ledger = require_keys(config["ledger"], {"max_row_bytes"}, set(), "Slack ledger")
    require_int(ledger["max_row_bytes"], "Slack ledger.max_row_bytes", 256, 16777216)
    transport = require_keys(
        config["transport"],
        {"api_base_env", "api_base_default", "socket_url_env", "bot_token_env", "app_token_env", "proxy_env", "connect_timeout_ms", "request_timeout_ms", "max_response_bytes", "max_frame_bytes", "reconnect_backoff_ms", "reconnect_backoff_cap_ms", "rate_limit_max_wait_ms", "read_retry_limit"},
        set(),
        "Slack transport",
    )
    for key in ("api_base_env", "api_base_default", "socket_url_env", "bot_token_env", "app_token_env", "proxy_env"):
        require_string(transport[key], f"Slack transport.{key}")
    for key, low, high in (
        ("connect_timeout_ms", 100, 120000),
        ("request_timeout_ms", 100, 600000),
        ("max_response_bytes", 1024, 67108864),
        ("max_frame_bytes", 1024, 134217728),
        ("reconnect_backoff_ms", 10, 60000),
        ("reconnect_backoff_cap_ms", 10, 600000),
        ("rate_limit_max_wait_ms", 0, 600000),
        ("read_retry_limit", 0, 10),
    ):
        require_int(transport[key], f"Slack transport.{key}", low, high)
    if transport["reconnect_backoff_cap_ms"] < transport["reconnect_backoff_ms"]:
        raise ValueError("Slack reconnect cap must not be below its initial backoff")
    authz = require_keys(
        config["authz"],
        {"allow_all_users", "allow_all_users_env", "allowed_user_ids", "allowed_user_ids_env", "allowed_channel_ids", "allowed_channel_ids_env", "allowed_team_ids", "allowed_team_ids_env", "deny_unmatched"},
        set(),
        "Slack authz",
    )
    require_bool(authz["allow_all_users"], "Slack authz.allow_all_users")
    require_bool(authz["deny_unmatched"], "Slack authz.deny_unmatched")
    for key in ("allow_all_users_env", "allowed_user_ids_env", "allowed_channel_ids_env", "allowed_team_ids_env"):
        require_string(authz[key], f"Slack authz.{key}")
    _validate_id_list(authz["allowed_user_ids"], "Slack authz.allowed_user_ids", USER_RE)
    _validate_id_list(authz["allowed_channel_ids"], "Slack authz.allowed_channel_ids", CHANNEL_RE)
    _validate_id_list(authz["allowed_team_ids"], "Slack authz.allowed_team_ids", TEAM_RE)
    gating = require_keys(
        config["gating"],
        {"respond_in_dms", "respond_in_channels_on_mention_only", "free_response_channel_ids", "free_response_channel_ids_env", "mention_patterns", "thread_sticky_after_mention", "thread_sticky_after_bot_reply", "accept_message_edits", "accept_message_deletes", "accept_bot_events"},
        set(),
        "Slack gating",
    )
    for key in ("respond_in_dms", "respond_in_channels_on_mention_only", "thread_sticky_after_mention", "thread_sticky_after_bot_reply", "accept_message_edits", "accept_message_deletes", "accept_bot_events"):
        require_bool(gating[key], f"Slack gating.{key}")
    _validate_id_list(gating["free_response_channel_ids"], "Slack gating.free_response_channel_ids", CHANNEL_RE)
    require_string(gating["free_response_channel_ids_env"], "Slack gating.free_response_channel_ids_env")
    for index, pattern in enumerate(require_string_list(gating["mention_patterns"], "Slack gating.mention_patterns", True)):
        try:
            re.compile(pattern, re.IGNORECASE)
        except re.error as error:
            raise ValueError(f"Slack gating.mention_patterns[{index}] is invalid: {error}") from error
    intake = require_keys(config["intake"], {"event_types", "interactive_types", "max_pending_events", "pending_retry_ms", "dedup_retention", "thread_context_max_messages"}, set(), "Slack intake")
    require_string_list(intake["event_types"], "Slack intake.event_types")
    require_string_list(intake["interactive_types"], "Slack intake.interactive_types")
    require_int(intake["max_pending_events"], "Slack intake.max_pending_events", 1, 1000000)
    require_int(intake["pending_retry_ms"], "Slack intake.pending_retry_ms", 10, 60000)
    require_int(intake["dedup_retention"], "Slack intake.dedup_retention", 1, 1000000)
    require_int(intake["thread_context_max_messages"], "Slack intake.thread_context_max_messages", 0, 1000)
    routing = require_keys(config["routing"], {"binding_registry_path_env", "private_scope", "unbound_policy", "binding_authoring_mode", "ipc_listen", "ipc_port", "ipc_max_frame_bytes"}, set(), "Slack routing")
    require_string(routing["binding_registry_path_env"], "Slack routing.binding_registry_path_env")
    if routing["private_scope"] not in {"registered", "all"}:
        raise ValueError("Slack routing.private_scope must be registered or all")
    if routing["unbound_policy"] not in {"admit_gateway", "retain", "refuse"}:
        raise ValueError("Slack routing.unbound_policy is invalid")
    if routing["binding_authoring_mode"] not in {"automatic", "operator"}:
        raise ValueError("Slack routing.binding_authoring_mode must be automatic or operator")
    listen = require_string(routing["ipc_listen"], "Slack routing.ipc_listen")
    try:
        listen_address = ipaddress.ip_address(listen)
    except ValueError as error:
        raise ValueError("Slack routing.ipc_listen must be an IP address") from error
    if listen_address.version != 4 or not listen_address.is_loopback:
        raise ValueError("Slack routing.ipc_listen must be an IPv4 loopback address")
    require_int(routing["ipc_port"], "Slack routing.ipc_port", 1, 65535)
    require_int(routing["ipc_max_frame_bytes"], "Slack routing.ipc_max_frame_bytes", 1024, 134217728)
    media = require_keys(config["media"], {"download_enabled", "max_download_bytes", "max_upload_bytes", "max_files_per_event", "text_preview_bytes", "allowed_download_hosts", "allowed_upload_hosts"}, set(), "Slack media")
    require_bool(media["download_enabled"], "Slack media.download_enabled")
    require_int(media["max_download_bytes"], "Slack media.max_download_bytes", 1, 1073741824)
    require_int(media["max_upload_bytes"], "Slack media.max_upload_bytes", 1, 5368709120)
    require_int(media["max_files_per_event"], "Slack media.max_files_per_event", 0, 100)
    require_int(media["text_preview_bytes"], "Slack media.text_preview_bytes", 0, 10485760)
    for key in ("allowed_download_hosts", "allowed_upload_hosts"):
        for host in require_string_list(media[key], f"Slack media.{key}"):
            if host.lower() != host or "." not in host or "/" in host:
                raise ValueError(f"Slack media.{key} contains invalid host {host!r}")
    outbound = require_keys(config["outbound"], {"reply_labels", "stream_labels", "error_labels", "telemetry_labels", "max_message_chars", "max_manifest_bytes", "max_payload_bytes", "stream_edit_interval_ms", "max_blocks", "max_block_json_bytes", "home_channel_env", "reactions", "allowed_web_methods", "retry_safe_web_methods"}, set(), "Slack outbound")
    for key in ("reply_labels", "stream_labels", "error_labels", "telemetry_labels", "allowed_web_methods", "retry_safe_web_methods"):
        require_string_list(outbound[key], f"Slack outbound.{key}", key == "telemetry_labels")
    require_int(outbound["max_message_chars"], "Slack outbound.max_message_chars", 1, 40000)
    require_int(outbound["max_manifest_bytes"], "Slack outbound.max_manifest_bytes", 1, 16777216)
    require_int(outbound["max_payload_bytes"], "Slack outbound.max_payload_bytes", 1, 1073741824)
    require_int(outbound["stream_edit_interval_ms"], "Slack outbound.stream_edit_interval_ms", 0, 60000)
    require_int(outbound["max_blocks"], "Slack outbound.max_blocks", 0, 50)
    require_int(outbound["max_block_json_bytes"], "Slack outbound.max_block_json_bytes", 2, 1048576)
    require_string(outbound["home_channel_env"], "Slack outbound.home_channel_env")
    reactions = require_keys(outbound["reactions"], {"enabled", "processing", "success", "failure"}, set(), "Slack outbound.reactions")
    require_bool(reactions["enabled"], "Slack outbound.reactions.enabled")
    for key in ("processing", "success", "failure"):
        require_string(reactions[key], f"Slack outbound.reactions.{key}")
    if not set(outbound["retry_safe_web_methods"]).issubset(outbound["allowed_web_methods"]):
        raise ValueError("Slack retry_safe_web_methods must be a subset of allowed_web_methods")
    validate_private_run(config["private_run"])
    if config["mode"] == "gateway":
        if config["binding"] is not None:
            raise ValueError("Slack gateway config must have null binding")
        validate_gateway_template(config["gateway_run_template"], config)
    else:
        if config["gateway_run_template"] is not None:
            raise ValueError("Slack binding config must have null gateway_run_template")
        validate_binding(config["binding"], interface_id)


def validate_private_run(value):
    value = require_keys(
        value,
        {"ee_template_owner", "parent_ee", "model", "reasoning_effort", "role_artifacts", "interface_owners", "id_prefixes", "file_write_tool", "file_write_label", "file_write_result_label", "worker_environment_kind", "file_write_mark_outcome", "provider_context_output_labels", "result_timeout_ms", "authoring_retry_attempts"},
        set(),
        "Slack private_run",
    )
    for key in ("ee_template_owner", "parent_ee", "model", "reasoning_effort", "file_write_tool", "file_write_label", "file_write_result_label", "worker_environment_kind", "file_write_mark_outcome"):
        require_string(value[key], f"Slack private_run.{key}")
    require_string_list(value["role_artifacts"], "Slack private_run.role_artifacts")
    require_string_list(value["provider_context_output_labels"], "Slack private_run.provider_context_output_labels")
    require_int(value["result_timeout_ms"], "Slack private_run.result_timeout_ms", 100, 600000)
    require_int(value["authoring_retry_attempts"], "Slack private_run.authoring_retry_attempts", 1, 16)
    owners = require_keys(value["interface_owners"], {"slack", "provider", "interface_authoring", "worker", "worker_environment", "operator"}, set(), "Slack private_run.interface_owners")
    prefixes = require_keys(value["id_prefixes"], {"run", "ee", "ee_template", "slack", "provider", "interface_authoring", "worker", "worker_environment", "tool"}, set(), "Slack private_run.id_prefixes")
    for name, item in owners.items():
        require_string(item, f"Slack private_run.interface_owners.{name}")
    for name, item in prefixes.items():
        item = require_string(item, f"Slack private_run.id_prefixes.{name}")
        if not re.fullmatch(r"[a-z0-9_.]+", item):
            raise ValueError(f"Slack private_run.id_prefixes.{name} is malformed")
    return value


def validate_gateway_template(template, config):
    template = require_keys(template, {"id", "ee", "worker", "trace", "status", "interface_state", "checkpoints", "active_checkpoints"}, set(), "Slack gateway_run_template")
    for key in ("id", "ee", "worker", "trace"):
        require_string(template[key], f"Slack gateway_run_template.{key}")
    if template["status"] != "running" or not isinstance(template["interface_state"], dict) or not isinstance(template["checkpoints"], list):
        raise ValueError("Slack gateway_run_template is not runnable")
    active = require_string_list(template["active_checkpoints"], "Slack gateway_run_template.active_checkpoints")
    checkpoints = {}
    for index, row in enumerate(template["checkpoints"]):
        row = require_keys(
            row,
            {"id", "run", "trace", "epoch", "interface_binding", "outbound_interface_bindings", "artifact_bindings", "required_verified_artifacts"},
            set(),
            f"Slack gateway_run_template.checkpoints[{index}]",
        )
        checkpoint_id = require_string(row["id"], f"Slack gateway_run_template.checkpoints[{index}].id")
        if checkpoint_id in checkpoints:
            raise ValueError(f"Slack gateway_run_template repeats checkpoint {checkpoint_id!r}")
        if row["run"] != template["id"]:
            raise ValueError(f"Slack gateway_run_template checkpoint {checkpoint_id!r} belongs to another run")
        require_string(row["trace"], f"Slack gateway_run_template checkpoint {checkpoint_id}.trace")
        require_int(row["epoch"], f"Slack gateway_run_template checkpoint {checkpoint_id}.epoch", 0)
        require_interface_binding(row["interface_binding"], f"Slack gateway_run_template checkpoint {checkpoint_id}.interface_binding")
        outbound = row["outbound_interface_bindings"]
        if not isinstance(outbound, list):
            raise ValueError(f"Slack gateway_run_template checkpoint {checkpoint_id!r} outbound bindings must be an array")
        for outbound_index, binding in enumerate(outbound):
            require_interface_binding(binding, f"Slack gateway_run_template checkpoint {checkpoint_id}.outbound_interface_bindings[{outbound_index}]")
        if not isinstance(row["artifact_bindings"], list) or not isinstance(row["required_verified_artifacts"], list):
            raise ValueError(f"Slack gateway_run_template checkpoint {checkpoint_id!r} artifact fields must be arrays")
        checkpoints[checkpoint_id] = row
    if any(checkpoint_id not in checkpoints for checkpoint_id in active):
        raise ValueError("Slack gateway_run_template active checkpoint is absent from checkpoints")
    routes = []
    for checkpoint_id in active:
        row = checkpoints[checkpoint_id]
        outbound_interfaces = [
            require_interface_binding(binding, f"Slack gateway_run_template checkpoint {checkpoint_id}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(row["outbound_interface_bindings"])
        ]
        if require_interface_binding(row["interface_binding"], f"Slack gateway_run_template checkpoint {checkpoint_id}.interface_binding") == "world_slack":
            routes.extend(outbound_interfaces)
    targets = list(dict.fromkeys(routes))
    if config["routing"]["binding_authoring_mode"] == "operator":
        targets = [target for target in targets if target == config["private_run"]["interface_owners"]["operator"]]
    inbound = len(targets) == 1
    outbound = inbound and any(
        require_interface_binding(checkpoints[checkpoint_id]["interface_binding"], f"Slack gateway_run_template checkpoint {checkpoint_id}.interface_binding") == targets[0]
        and "world_slack" in [
            require_interface_binding(binding, f"Slack gateway_run_template checkpoint {checkpoint_id}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(checkpoints[checkpoint_id]["outbound_interface_bindings"])
        ]
        for checkpoint_id in active
    )
    if not inbound or not outbound:
        raise ValueError("Slack gateway_run_template does not route the configured binding authoring mode")
    return template


def validate_binding(binding, interface_id):
    binding = require_keys(binding, {"interface", "team_id", "channel_id", "thread_ts", "run", "gateway_host", "gateway_port", "secret_file", "bot_token_file"}, set(), "Slack binding")
    if require_string(binding["interface"], "Slack binding.interface") != interface_id:
        raise ValueError("Slack binding.interface must match folder identity")
    if not TEAM_RE.fullmatch(require_string(binding["team_id"], "Slack binding.team_id")):
        raise ValueError("Slack binding.team_id is malformed")
    if not CHANNEL_RE.fullmatch(require_string(binding["channel_id"], "Slack binding.channel_id")):
        raise ValueError("Slack binding.channel_id is malformed")
    thread = require_nullable_string(binding["thread_ts"], "Slack binding.thread_ts")
    if thread is not None and not TS_RE.fullmatch(thread):
        raise ValueError("Slack binding.thread_ts is malformed")
    require_string(binding["run"], "Slack binding.run")
    gateway_host = require_string(binding["gateway_host"], "Slack binding.gateway_host")
    try:
        gateway_address = ipaddress.ip_address(gateway_host)
    except ValueError as error:
        raise ValueError("Slack binding.gateway_host must be an IP address") from error
    if gateway_address.version != 4 or not gateway_address.is_loopback:
        raise ValueError("Slack binding.gateway_host must be an IPv4 loopback address")
    require_int(binding["gateway_port"], "Slack binding.gateway_port", 1, 65535)
    secret_file = require_string(binding["secret_file"], "Slack binding.secret_file")
    if not os.path.isabs(secret_file):
        raise ValueError("Slack binding.secret_file must be absolute")
    bot_token_file = require_string(binding["bot_token_file"], "Slack binding.bot_token_file")
    if not os.path.isabs(bot_token_file):
        raise ValueError("Slack binding.bot_token_file must be absolute")
    return binding


def env_flag(name):
    value = os.environ.get(name)
    if value is None:
        return None
    lowered = value.strip().lower()
    if lowered in {"1", "true", "yes", "on"}:
        return True
    if lowered in {"0", "false", "no", "off"}:
        return False
    raise ValueError(f"{name} must be a boolean")


def env_ids(name, pattern):
    raw = os.environ.get(name, "")
    values = {part.strip() for part in raw.split(",") if part.strip()}
    for value in values:
        if not pattern.fullmatch(value):
            raise ValueError(f"{name} contains malformed Slack id {value!r}")
    return values


def rich_text(blocks):
    if not isinstance(blocks, list):
        return ""
    lines = []

    def inline(elements):
        pieces = []
        for element in elements if isinstance(elements, list) else []:
            if not isinstance(element, dict):
                continue
            kind = element.get("type")
            if kind == "text":
                pieces.append(str(element.get("text") or ""))
            elif kind == "link":
                url = str(element.get("url") or "")
                pieces.append(str(element.get("text") or url))
                if element.get("text") and url:
                    pieces.append(f" ({url})")
            elif kind == "channel":
                pieces.append(f"<#{element.get('channel_id', '')}>")
            elif kind == "user":
                pieces.append(f"<@{element.get('user_id', '')}>")
            elif kind == "usergroup":
                pieces.append(f"<!subteam^{element.get('usergroup_id', '')}>")
            elif kind == "emoji":
                pieces.append(f":{element.get('name', '')}:")
            elif kind == "broadcast":
                pieces.append(f"<!{element.get('range', 'here')}>")
            elif kind == "date":
                pieces.append(str(element.get("fallback") or ""))
        return "".join(pieces)

    def walk(elements, quote=0, bullet=""):
        for element in elements if isinstance(elements, list) else []:
            if not isinstance(element, dict):
                continue
            kind = element.get("type")
            if kind == "rich_text_section":
                text = inline(element.get("elements"))
                if text.strip():
                    lines.append((">" * quote + " " if quote else "") + bullet + text)
            elif kind == "rich_text_quote":
                walk(element.get("elements"), quote + 1, bullet)
            elif kind == "rich_text_list":
                for index, item in enumerate(element.get("elements") or []):
                    marker = "• " if element.get("style") == "bullet" else f"{index + 1}. "
                    walk([item], quote, marker)
            elif kind == "rich_text_preformatted":
                text = inline(element.get("elements"))
                if text:
                    lines.append((">" * quote + " " if quote else "") + bullet + f"```\n{text}\n```")
    for block in blocks:
        if isinstance(block, dict) and block.get("type") == "rich_text":
            walk(block.get("elements"))
    return "\n".join(lines)


def safe_blocks(blocks, maximum_bytes):
    if blocks is None:
        return []
    if not isinstance(blocks, list):
        raise ValueError("Slack blocks must be an array")
    encoded = json.dumps(blocks, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if len(encoded) > maximum_bytes:
        raise ValueError("Slack blocks exceed configured byte ceiling")
    return blocks


def slack_mrkdwn(text):
    text = require_string(text, "Slack message text", allow_empty=True)
    parts = re.split(r"(```[\s\S]*?```|`[^`\n]*`)", text)
    for index in range(0, len(parts), 2):
        value = parts[index]
        value = re.sub(r"\[([^\]\n]+)\]\((https?://[^\s)]+)\)", r"<\2|\1>", value)
        value = re.sub(r"(?m)^#{1,6}\s+(.+)$", r"*\1*", value)
        value = re.sub(r"\*\*([^*\n]+)\*\*", r"*\1*", value)
        value = re.sub(r"__([^_\n]+)__", r"*\1*", value)
        value = re.sub(r"~~([^~\n]+)~~", r"~\1~", value)
        parts[index] = value
    return "".join(parts)


def event_coordinates(envelope):
    payload = envelope.get("payload") if isinstance(envelope.get("payload"), dict) else {}
    outer_type = envelope.get("type")
    if outer_type == "events_api":
        event = payload.get("event") if isinstance(payload.get("event"), dict) else {}
        team = payload.get("team_id") or event.get("team") or event.get("team_id")
        item = event.get("item") if isinstance(event.get("item"), dict) else {}
        assistant_thread = event.get("assistant_thread") if isinstance(event.get("assistant_thread"), dict) else {}
        context = assistant_thread.get("context") if isinstance(assistant_thread.get("context"), dict) else {}
        channel = event.get("channel") or event.get("channel_id") or item.get("channel") or assistant_thread.get("channel_id") or context.get("channel_id")
        thread = event.get("thread_ts")
        thread = thread or assistant_thread.get("thread_ts")
        ts = event.get("ts") or event.get("event_ts") or item.get("ts")
        if event.get("subtype") in {"message_changed", "message_deleted"}:
            message = event.get("message") if isinstance(event.get("message"), dict) else {}
            previous = event.get("previous_message") if isinstance(event.get("previous_message"), dict) else {}
            thread = message.get("thread_ts") or previous.get("thread_ts") or thread
            ts = message.get("ts") or previous.get("ts") or ts
        return team, channel, thread, ts
    if outer_type == "slash_commands":
        return payload.get("team_id"), payload.get("channel_id"), None, None
    if outer_type == "interactive":
        team = (payload.get("team") or {}).get("id") if isinstance(payload.get("team"), dict) else payload.get("team_id")
        container = payload.get("container") if isinstance(payload.get("container"), dict) else {}
        channel = container.get("channel_id") or ((payload.get("channel") or {}).get("id") if isinstance(payload.get("channel"), dict) else None)
        thread = container.get("thread_ts")
        ts = container.get("message_ts")
        return team, channel, thread, ts
    return None, None, None, None


def envelope_key(envelope):
    payload = envelope.get("payload")
    if envelope.get("type") == "events_api" and isinstance(payload, dict):
        event_id = payload.get("event_id")
        if isinstance(event_id, str) and event_id:
            return "event-" + hashlib.sha256(event_id.encode("utf-8")).hexdigest()
    key = envelope.get("envelope_id")
    if isinstance(key, str) and key:
        return "envelope-" + hashlib.sha256(key.encode("utf-8")).hexdigest()
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return "digest-" + hashlib.sha256(encoded).hexdigest()


def normalize_operation(body, config, default_target=None):
    body = require_object(body, "Slack outbound payload")
    operation = body.get("operation")
    if operation is None:
        require_keys(body, {"text"}, {"team_id", "channel", "channel_id", "thread_ts"}, "Slack reply")
        text = require_string(body.get("text"), "Slack reply.text", maximum=config["outbound"]["max_message_chars"])
        target = dict(default_target or {})
        return "message.post", {"team_id": body.get("team_id") or target.get("team_id"), "channel": body.get("channel") or body.get("channel_id") or target.get("channel_id"), "thread_ts": body.get("thread_ts") if "thread_ts" in body else target.get("thread_ts"), "text": text}
    operation = require_string(operation, "Slack operation")
    require_keys(body, {"operation", "args"}, set(), "Slack operation payload")
    args = require_object(body.get("args"), "Slack operation.args")
    allowed = {
        "assistant.status.clear",
        "assistant.status.set",
        "callback.respond",
        "file.upload",
        "message.delete",
        "message.ephemeral",
        "message.post",
        "message.update",
        "profile.status.set",
        "reaction.add",
        "reaction.remove",
        "typing.clear",
        "typing.set",
        "view.open",
        "view.publish",
        "view.push",
        "view.update",
        "web.call",
    }
    if operation not in allowed:
        raise ValueError(f"Slack operation {operation!r} is not supported")
    shapes = {
        "assistant.status.clear": (set(), {"team_id", "channel", "channel_id", "thread_ts"}),
        "assistant.status.set": ({"status"}, {"team_id", "channel", "channel_id", "thread_ts"}),
        "callback.respond": ({"handle", "body"}, set()),
        "file.upload": (set(), {"team_id", "channel", "channel_id", "thread_ts", "filename", "blob_id", "title", "files", "initial_comment", "caption"}),
        "message.delete": ({"ts"}, {"team_id", "channel", "channel_id"}),
        "message.ephemeral": ({"user", "text"}, {"team_id", "channel", "channel_id", "thread_ts", "blocks"}),
        "message.post": (set(), {"team_id", "channel", "channel_id", "thread_ts", "text", "blocks", "fallback_text", "mrkdwn", "reply_broadcast", "metadata"}),
        "message.update": ({"ts"}, {"team_id", "channel", "channel_id", "text", "blocks"}),
        "profile.status.set": (set(), {"team_id", "status_text", "status_emoji", "status_expiration"}),
        "reaction.add": ({"name"}, {"team_id", "channel", "channel_id", "timestamp", "ts"}),
        "reaction.remove": ({"name"}, {"team_id", "channel", "channel_id", "timestamp", "ts"}),
        "typing.clear": (set(), {"team_id", "channel", "channel_id", "thread_ts"}),
        "typing.set": (set(), {"team_id", "channel", "channel_id", "thread_ts", "status"}),
        "view.open": ({"view"}, {"team_id", "trigger_handle", "trigger_id"}),
        "view.publish": ({"user_id", "view"}, {"team_id"}),
        "view.push": ({"view"}, {"team_id", "trigger_handle", "trigger_id"}),
        "view.update": ({"view"}, {"team_id", "view_id", "external_id", "hash"}),
        "web.call": ({"method"}, {"team_id", "params"}),
    }
    required, optional = shapes[operation]
    require_keys(args, required, optional, f"Slack {operation} args")
    return operation, dict(args)
