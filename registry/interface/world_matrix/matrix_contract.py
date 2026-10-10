import hashlib
import ipaddress
import json
import os
import re
import urllib.parse


MXID_RE = re.compile(r"^@[0-9A-Za-z._=/-]{1,255}:[0-9A-Za-z.-]+(?::[0-9]{1,5})?$")
ROOM_ID_RE = re.compile(r"^![^:\s]{1,255}:[^\s]+$")
ROOM_ALIAS_RE = re.compile(r"^#[^:\s]{1,255}:[^\s]+$")
EVENT_ID_RE = re.compile(r"^\$[^\s]{1,1024}$")
SERVER_RE = re.compile(r"^[0-9A-Za-z.-]+(?::[0-9]{1,5})?$")
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")


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
    if not isinstance(value, str) or value.strip() != value:
        raise ValueError(f"{name} must be a trimmed string")
    if not allow_empty and not value:
        raise ValueError(f"{name} must not be empty")
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


def require_number(value, name, minimum=None, maximum=None):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{name} must be a number")
    value = float(value)
    if minimum is not None and value < minimum:
        raise ValueError(f"{name} must be at least {minimum}")
    if maximum is not None and value > maximum:
        raise ValueError(f"{name} must be at most {maximum}")
    return value


def require_string_list(value, name, allow_empty=False, maximum=None):
    if not isinstance(value, list) or (not allow_empty and not value):
        raise ValueError(f"{name} must be an array")
    if maximum is not None and len(value) > maximum:
        raise ValueError(f"{name} exceeds {maximum} entries")
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
        "interface": require_string(interface, "interface binding identity", maximum=512),
        "config": {"artifact_policy": {"mode": "all"}},
    }


def require_interface_binding(value, name):
    binding = require_keys(value, {"interface", "config"}, set(), name)
    interface = require_string(binding["interface"], f"{name}.interface", maximum=512)
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


def require_nullable_string(value, name, maximum=None):
    if value is None:
        return None
    return require_string(value, name, maximum=maximum)


def require_mxid(value, name):
    value = require_string(value, name, maximum=512)
    if not MXID_RE.fullmatch(value):
        raise ValueError(f"{name} is not a Matrix user id")
    return value


def require_room(value, name):
    value = require_string(value, name, maximum=1024)
    if not ROOM_ID_RE.fullmatch(value) and not ROOM_ALIAS_RE.fullmatch(value):
        raise ValueError(f"{name} is not a Matrix room id or alias")
    return value


def require_event(value, name, nullable=False):
    if nullable and value is None:
        return None
    value = require_string(value, name, maximum=2048)
    if not EVENT_ID_RE.fullmatch(value):
        raise ValueError(f"{name} is not a Matrix event id")
    return value


def require_server(value, name):
    value = require_string(value, name, maximum=512).lower()
    if not SERVER_RE.fullmatch(value):
        raise ValueError(f"{name} is not a Matrix server name")
    return value


def require_digest(value, name):
    value = require_string(value, name)
    if not DIGEST_RE.fullmatch(value):
        raise ValueError(f"{name} must be a lowercase sha256 digest")
    return value


def validate_url(value, name, schemes=("http", "https")):
    value = require_string(value, name, maximum=4096)
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme.lower() not in schemes or not parsed.hostname or parsed.username or parsed.password:
        raise ValueError(f"{name} must be an absolute {schemes} URL without credentials")
    return value.rstrip("/")


def is_loopback_hostname(value):
    if not isinstance(value, str) or not value:
        return False
    lowered = value.lower().rstrip(".")
    if lowered == "localhost" or lowered.endswith(".localhost"):
        return True
    try:
        return ipaddress.ip_address(lowered).is_loopback
    except ValueError:
        return False


def validate_proxy_url(value):
    if value is None or value == "":
        return ""
    value = require_string(value, "Matrix proxy URL", maximum=4096)
    if any(ord(character) < 33 or ord(character) == 127 for character in value) or "\\" in value:
        raise ValueError("Matrix proxy URL contains invalid characters")
    if "?" in value or "#" in value:
        raise ValueError("Matrix proxy URL must not contain query or fragment data")
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise ValueError("Matrix proxy URL is malformed") from error
    if parsed.scheme not in {"http", "https", "socks", "socks4", "socks5", "socks5h"} or not parsed.netloc or parsed.hostname is None:
        raise ValueError("Matrix proxy URL uses an unsupported scheme")
    if parsed.path not in {"", "/"}:
        raise ValueError("Matrix proxy URL must not contain a path")
    if port is not None and not 1 <= port <= 65535:
        raise ValueError("Matrix proxy URL port is invalid")
    if re.search(r"%(?![0-9A-Fa-f]{2})", parsed.netloc):
        raise ValueError("Matrix proxy URL contains malformed escaping")
    try:
        username = urllib.parse.unquote(parsed.username, errors="strict") if parsed.username is not None else None
        password = urllib.parse.unquote(parsed.password, errors="strict") if parsed.password is not None else None
    except UnicodeDecodeError as error:
        raise ValueError("Matrix proxy credentials are malformed") from error
    if (username is None) != (password is None) or username == "" or password == "":
        raise ValueError("Matrix proxy credentials require a non-empty username and password")
    if username is not None and ":" in username:
        raise ValueError("Matrix proxy username must not contain a colon")
    if any(item is not None and any(ord(character) < 32 or ord(character) == 127 for character in item) for item in (username, password)):
        raise ValueError("Matrix proxy credentials contain invalid characters")
    if parsed.scheme != "https" and not is_loopback_hostname(parsed.hostname):
        raise ValueError("remote Matrix proxies must use HTTPS")
    return value


def _validate_pattern_list(values, name):
    for index, pattern in enumerate(require_string_list(values, name, True, 256)):
        try:
            re.compile(pattern)
        except re.error as error:
            raise ValueError(f"{name}[{index}] is invalid: {error}") from error


def _validate_identifiers(values, name, validator):
    for index, value in enumerate(require_string_list(values, name, True, 10000)):
        validator(value, f"{name}[{index}]")


def validate_config(config, interface_id):
    config = require_keys(
        config,
        {
            "schema_version",
            "mode",
            "ledger",
            "transport",
            "authz",
            "gating",
            "sync",
            "routing",
            "crypto",
            "media",
            "outbound",
            "private_run",
            "binding",
            "gateway_run_template",
        },
        set(),
        "Matrix config",
    )
    if config["schema_version"] != 1:
        raise ValueError("Matrix config.schema_version must be 1")
    if config["mode"] not in {"gateway", "binding"}:
        raise ValueError("Matrix config.mode must be gateway or binding")
    ledger = require_keys(config["ledger"], {"max_row_bytes"}, set(), "Matrix ledger")
    require_int(ledger["max_row_bytes"], "Matrix ledger.max_row_bytes", 256, 16777216)
    transport = require_keys(
        config["transport"],
        {
            "homeserver_env",
            "access_token_env",
            "user_id_env",
            "password_env",
            "device_id_env",
            "proxy_env",
            "connect_timeout_ms",
            "request_timeout_ms",
            "sync_timeout_ms",
            "sync_watchdog_ms",
            "max_response_bytes",
            "reconnect_backoff_ms",
            "reconnect_backoff_cap_ms",
            "rate_limit_max_wait_ms",
            "safe_retry_limit",
            "user_agent",
        },
        set(),
        "Matrix transport",
    )
    for key in (
        "homeserver_env",
        "access_token_env",
        "user_id_env",
        "password_env",
        "device_id_env",
        "proxy_env",
        "user_agent",
    ):
        require_string(transport[key], f"Matrix transport.{key}", maximum=256)
    for key, low, high in (
        ("connect_timeout_ms", 100, 120000),
        ("request_timeout_ms", 100, 600000),
        ("sync_timeout_ms", 1000, 120000),
        ("sync_watchdog_ms", 1000, 180000),
        ("max_response_bytes", 1024, 268435456),
        ("reconnect_backoff_ms", 10, 60000),
        ("reconnect_backoff_cap_ms", 10, 600000),
        ("rate_limit_max_wait_ms", 0, 600000),
        ("safe_retry_limit", 0, 20),
    ):
        require_int(transport[key], f"Matrix transport.{key}", low, high)
    if transport["sync_watchdog_ms"] <= transport["sync_timeout_ms"]:
        raise ValueError("Matrix sync watchdog must exceed sync timeout")
    if transport["reconnect_backoff_cap_ms"] < transport["reconnect_backoff_ms"]:
        raise ValueError("Matrix reconnect cap must not be below its initial backoff")
    authz = require_keys(
        config["authz"],
        {
            "allow_all_users",
            "allow_all_users_env",
            "allowed_user_ids",
            "allowed_user_ids_env",
            "allowed_room_ids",
            "allowed_room_ids_env",
            "allowed_server_names",
            "allowed_server_names_env",
            "allowed_inviter_ids",
            "allowed_inviter_ids_env",
            "ignored_user_patterns",
            "ignored_user_patterns_env",
            "deny_unmatched",
            "auto_join_invites",
            "allow_public_room_creation",
        },
        set(),
        "Matrix authz",
    )
    for key in ("allow_all_users", "deny_unmatched", "auto_join_invites", "allow_public_room_creation"):
        require_bool(authz[key], f"Matrix authz.{key}")
    for key in (
        "allow_all_users_env",
        "allowed_user_ids_env",
        "allowed_room_ids_env",
        "allowed_server_names_env",
        "allowed_inviter_ids_env",
        "ignored_user_patterns_env",
    ):
        require_string(authz[key], f"Matrix authz.{key}", maximum=256)
    _validate_identifiers(authz["allowed_user_ids"], "Matrix authz.allowed_user_ids", require_mxid)
    _validate_identifiers(authz["allowed_inviter_ids"], "Matrix authz.allowed_inviter_ids", require_mxid)
    _validate_identifiers(authz["allowed_room_ids"], "Matrix authz.allowed_room_ids", require_room)
    _validate_identifiers(authz["allowed_server_names"], "Matrix authz.allowed_server_names", require_server)
    _validate_pattern_list(authz["ignored_user_patterns"], "Matrix authz.ignored_user_patterns")
    gating = require_keys(
        config["gating"],
        {
            "respond_in_dms",
            "require_mention_in_rooms",
            "free_response_room_ids",
            "free_response_room_ids_env",
            "thread_require_mention",
            "thread_sticky_after_mention",
            "thread_sticky_after_bot_reply",
            "auto_thread_rooms",
            "auto_thread_dms",
            "accept_notices",
            "accept_edits",
            "accept_redactions",
            "accept_reactions",
            "accept_membership",
            "accept_receipts",
            "accept_typing",
            "accept_presence",
            "allow_room_mentions",
        },
        set(),
        "Matrix gating",
    )
    for key, value in gating.items():
        if key == "free_response_room_ids":
            _validate_identifiers(value, "Matrix gating.free_response_room_ids", require_room)
        elif key == "free_response_room_ids_env":
            require_string(value, "Matrix gating.free_response_room_ids_env", maximum=256)
        else:
            require_bool(value, f"Matrix gating.{key}")
    sync = require_keys(
        config["sync"],
        {
            "initial_backfill",
            "max_pending_events",
            "pending_retry_ms",
            "dedup_retention",
            "raw_sync_retention",
            "text_batch_delay_ms",
            "text_batch_split_delay_ms",
            "text_batch_split_threshold",
        },
        set(),
        "Matrix sync",
    )
    if sync["initial_backfill"] not in {"admit", "retain_raw", "drop"}:
        raise ValueError("Matrix sync.initial_backfill is invalid")
    for key, low, high in (
        ("max_pending_events", 1, 1000000),
        ("pending_retry_ms", 10, 60000),
        ("dedup_retention", 1, 1000000),
        ("raw_sync_retention", 1, 100000),
        ("text_batch_delay_ms", 0, 60000),
        ("text_batch_split_delay_ms", 0, 60000),
        ("text_batch_split_threshold", 1, 1000000),
    ):
        require_int(sync[key], f"Matrix sync.{key}", low, high)
    routing = require_keys(
        config["routing"],
        {
            "binding_registry_path_env",
            "private_scope",
            "unbound_policy",
            "binding_authoring_mode",
            "ipc_listen",
            "ipc_port",
            "ipc_max_frame_bytes",
        },
        set(),
        "Matrix routing",
    )
    require_string(routing["binding_registry_path_env"], "Matrix routing.binding_registry_path_env", maximum=256)
    if routing["private_scope"] not in {"registered", "all"}:
        raise ValueError("Matrix routing.private_scope must be registered or all")
    if routing["unbound_policy"] not in {"admit_gateway", "retain", "refuse"}:
        raise ValueError("Matrix routing.unbound_policy is invalid")
    if routing["binding_authoring_mode"] not in {"automatic", "operator"}:
        raise ValueError("Matrix routing.binding_authoring_mode must be automatic or operator")
    listen = require_string(routing["ipc_listen"], "Matrix routing.ipc_listen", maximum=512)
    try:
        listen_address = ipaddress.ip_address(listen)
    except ValueError as error:
        raise ValueError("Matrix routing.ipc_listen must be an IP address") from error
    if listen_address.version != 4 or not listen_address.is_loopback:
        raise ValueError("Matrix routing.ipc_listen must be an IPv4 loopback address")
    require_int(routing["ipc_port"], "Matrix routing.ipc_port", 1, 65535)
    require_int(routing["ipc_max_frame_bytes"], "Matrix routing.ipc_max_frame_bytes", 1024, 268435456)
    crypto = require_keys(
        config["crypto"],
        {
            "mode",
            "mode_env",
            "store_subdir",
            "minimum_device_trust",
            "recovery_key_env",
            "generated_recovery_key_file",
            "encrypt_media",
            "require_cross_signing",
            "max_undecrypted_events",
        },
        set(),
        "Matrix crypto",
    )
    if crypto["mode"] not in {"off", "optional", "required"}:
        raise ValueError("Matrix crypto.mode is invalid")
    if crypto["minimum_device_trust"] not in {"unverified", "verified"}:
        raise ValueError("Matrix crypto.minimum_device_trust is invalid")
    for key in ("mode_env", "store_subdir", "recovery_key_env", "generated_recovery_key_file"):
        value = require_string(crypto[key], f"Matrix crypto.{key}", maximum=256)
        if key in {"store_subdir", "generated_recovery_key_file"} and (value != os.path.basename(value) or value in {".", ".."}):
            raise ValueError(f"Matrix crypto.{key} must be one path component")
    require_bool(crypto["encrypt_media"], "Matrix crypto.encrypt_media")
    require_bool(crypto["require_cross_signing"], "Matrix crypto.require_cross_signing")
    require_int(crypto["max_undecrypted_events"], "Matrix crypto.max_undecrypted_events", 1, 100000)
    media = require_keys(
        config["media"],
        {
            "max_download_bytes",
            "max_upload_bytes",
            "text_preview_bytes",
            "inline_text_extensions",
        },
        set(),
        "Matrix media",
    )
    require_int(media["max_download_bytes"], "Matrix media.max_download_bytes", 1, 1073741824)
    require_int(media["max_upload_bytes"], "Matrix media.max_upload_bytes", 1, 1073741824)
    require_int(media["text_preview_bytes"], "Matrix media.text_preview_bytes", 0, 10485760)
    for extension in require_string_list(media["inline_text_extensions"], "Matrix media.inline_text_extensions", True, 256):
        if not re.fullmatch(r"\.[0-9A-Za-z_-]{1,16}", extension) or extension.lower() != extension:
            raise ValueError("Matrix media.inline_text_extensions contains an invalid extension")
    outbound = require_keys(
        config["outbound"],
        {
            "reply_labels",
            "stream_labels",
            "error_labels",
            "telemetry_labels",
            "max_message_chars",
            "max_manifest_bytes",
            "max_payload_bytes",
            "stream_edit_interval_ms",
            "home_room_env",
            "reactions",
            "allowed_operations",
            "retry_safe_operations",
            "binding_allowed_operations",
        },
        set(),
        "Matrix outbound",
    )
    for key in ("reply_labels", "stream_labels", "error_labels", "telemetry_labels", "allowed_operations", "retry_safe_operations", "binding_allowed_operations"):
        require_string_list(outbound[key], f"Matrix outbound.{key}", key == "telemetry_labels", 512)
    require_int(outbound["max_message_chars"], "Matrix outbound.max_message_chars", 1, 65535)
    require_int(outbound["max_manifest_bytes"], "Matrix outbound.max_manifest_bytes", 1024, 16777216)
    require_int(outbound["max_payload_bytes"], "Matrix outbound.max_payload_bytes", 1024, 67108864)
    require_int(outbound["stream_edit_interval_ms"], "Matrix outbound.stream_edit_interval_ms", 0, 60000)
    require_string(outbound["home_room_env"], "Matrix outbound.home_room_env", maximum=256)
    reactions = require_keys(outbound["reactions"], {"enabled", "processing", "success", "failure"}, set(), "Matrix outbound.reactions")
    require_bool(reactions["enabled"], "Matrix outbound.reactions.enabled")
    for key in ("processing", "success", "failure"):
        require_string(reactions[key], f"Matrix outbound.reactions.{key}", maximum=64)
    if not set(outbound["retry_safe_operations"]).issubset(outbound["allowed_operations"]):
        raise ValueError("Matrix retry-safe operations must be a subset of allowed operations")
    if not set(outbound["binding_allowed_operations"]).issubset(outbound["allowed_operations"]):
        raise ValueError("Matrix binding operations must be a subset of allowed operations")
    validate_private_run(config["private_run"])
    if config["mode"] == "gateway":
        if config["binding"] is not None:
            raise ValueError("Matrix gateway binding must be null")
        validate_gateway_template(config["gateway_run_template"], config)
    else:
        if config["gateway_run_template"] is not None:
            raise ValueError("Matrix binding gateway_run_template must be null")
        validate_binding(config["binding"], interface_id)
    return config


def validate_private_run(value):
    value = require_keys(
        value,
        {"ee_template_owner", "parent_ee", "model", "reasoning_effort", "role_artifacts", "interface_owners", "id_prefixes", "file_write_tool", "file_write_label", "file_write_result_label", "worker_environment_kind", "file_write_mark_outcome", "provider_context_output_labels", "result_timeout_ms", "authoring_retry_attempts"},
        set(),
        "Matrix private_run",
    )
    for key in ("ee_template_owner", "parent_ee", "model", "reasoning_effort", "file_write_tool", "file_write_label", "file_write_result_label", "worker_environment_kind", "file_write_mark_outcome"):
        require_string(value[key], f"Matrix private_run.{key}", maximum=512)
    require_string_list(value["role_artifacts"], "Matrix private_run.role_artifacts", maximum=64)
    require_string_list(value["provider_context_output_labels"], "Matrix private_run.provider_context_output_labels", maximum=64)
    require_int(value["result_timeout_ms"], "Matrix private_run.result_timeout_ms", 100, 600000)
    require_int(value["authoring_retry_attempts"], "Matrix private_run.authoring_retry_attempts", 1, 16)
    owners = require_keys(value["interface_owners"], {"matrix", "provider", "interface_authoring", "worker", "worker_environment", "operator"}, set(), "Matrix private_run.interface_owners")
    prefixes = require_keys(value["id_prefixes"], {"run", "ee", "ee_template", "matrix", "provider", "interface_authoring", "worker", "worker_environment", "tool"}, set(), "Matrix private_run.id_prefixes")
    for name, item in owners.items():
        require_string(item, f"Matrix private_run.interface_owners.{name}", maximum=512)
    for name, item in prefixes.items():
        item = require_string(item, f"Matrix private_run.id_prefixes.{name}", maximum=256)
        if not re.fullmatch(r"[a-z0-9_.]+", item):
            raise ValueError(f"Matrix private_run.id_prefixes.{name} is malformed")
    return value


def validate_gateway_template(template, config):
    template = require_keys(template, {"id", "ee", "worker", "trace", "status", "interface_state", "checkpoints", "active_checkpoints"}, set(), "Matrix gateway_run_template")
    for key in ("id", "ee", "worker", "trace"):
        require_string(template[key], f"Matrix gateway_run_template.{key}")
    if template["status"] != "running" or not isinstance(template["interface_state"], dict) or not isinstance(template["checkpoints"], list):
        raise ValueError("Matrix gateway_run_template is not runnable")
    active = require_string_list(template["active_checkpoints"], "Matrix gateway_run_template.active_checkpoints")
    checkpoints = {}
    for index, row in enumerate(template["checkpoints"]):
        row = require_keys(
            row,
            {"id", "run", "trace", "epoch", "interface_binding", "outbound_interface_bindings", "artifact_bindings", "required_verified_artifacts"},
            set(),
            f"Matrix gateway_run_template.checkpoints[{index}]",
        )
        checkpoint_id = require_string(row["id"], f"Matrix gateway_run_template.checkpoints[{index}].id")
        if checkpoint_id in checkpoints:
            raise ValueError(f"Matrix gateway_run_template repeats checkpoint {checkpoint_id!r}")
        if row["run"] != template["id"]:
            raise ValueError(f"Matrix gateway_run_template checkpoint {checkpoint_id!r} belongs to another run")
        require_string(row["trace"], f"Matrix gateway_run_template checkpoint {checkpoint_id}.trace")
        require_int(row["epoch"], f"Matrix gateway_run_template checkpoint {checkpoint_id}.epoch", 0)
        require_interface_binding(row["interface_binding"], f"Matrix gateway_run_template checkpoint {checkpoint_id}.interface_binding")
        outbound = row["outbound_interface_bindings"]
        if not isinstance(outbound, list):
            raise ValueError(f"Matrix gateway_run_template checkpoint {checkpoint_id!r} outbound bindings must be an array")
        for outbound_index, binding in enumerate(outbound):
            require_interface_binding(binding, f"Matrix gateway_run_template checkpoint {checkpoint_id}.outbound_interface_bindings[{outbound_index}]")
        if not isinstance(row["artifact_bindings"], list) or not isinstance(row["required_verified_artifacts"], list):
            raise ValueError(f"Matrix gateway_run_template checkpoint {checkpoint_id!r} artifact fields must be arrays")
        checkpoints[checkpoint_id] = row
    if any(checkpoint_id not in checkpoints for checkpoint_id in active):
        raise ValueError("Matrix gateway_run_template active checkpoint is absent from checkpoints")
    routes = []
    for checkpoint_id in active:
        row = checkpoints[checkpoint_id]
        outbound_interfaces = [
            require_interface_binding(binding, f"Matrix gateway_run_template checkpoint {checkpoint_id}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(row["outbound_interface_bindings"])
        ]
        if require_interface_binding(row["interface_binding"], f"Matrix gateway_run_template checkpoint {checkpoint_id}.interface_binding") == "world_matrix":
            routes.extend(outbound_interfaces)
    targets = list(dict.fromkeys(routes))
    if config["routing"]["binding_authoring_mode"] == "operator":
        targets = [target for target in targets if target == config["private_run"]["interface_owners"]["operator"]]
    inbound = len(targets) == 1
    outbound = inbound and any(
        require_interface_binding(checkpoints[checkpoint_id]["interface_binding"], f"Matrix gateway_run_template checkpoint {checkpoint_id}.interface_binding") == targets[0]
        and "world_matrix" in [
            require_interface_binding(binding, f"Matrix gateway_run_template checkpoint {checkpoint_id}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(checkpoints[checkpoint_id]["outbound_interface_bindings"])
        ]
        for checkpoint_id in active
    )
    if not inbound or not outbound:
        raise ValueError("Matrix gateway_run_template does not route the configured binding authoring mode")
    return template


def validate_binding(binding, interface_id):
    binding = require_keys(
        binding,
        {"interface", "room_id", "thread_event_id", "run", "gateway_host", "gateway_port", "secret_file"},
        set(),
        "Matrix binding",
    )
    if require_string(binding["interface"], "Matrix binding.interface") != interface_id:
        raise ValueError("Matrix binding.interface must match folder identity")
    room_id = require_room(binding["room_id"], "Matrix binding.room_id")
    if not ROOM_ID_RE.fullmatch(room_id):
        raise ValueError("Matrix binding.room_id must be a concrete room id")
    require_event(binding["thread_event_id"], "Matrix binding.thread_event_id", nullable=True)
    require_string(binding["run"], "Matrix binding.run", maximum=512)
    gateway_host = require_string(binding["gateway_host"], "Matrix binding.gateway_host", maximum=512)
    try:
        gateway_address = ipaddress.ip_address(gateway_host)
    except ValueError as error:
        raise ValueError("Matrix binding.gateway_host must be an IP address") from error
    if gateway_address.version != 4 or not gateway_address.is_loopback:
        raise ValueError("Matrix binding.gateway_host must be an IPv4 loopback address")
    require_int(binding["gateway_port"], "Matrix binding.gateway_port", 1, 65535)
    secret_file = require_string(binding["secret_file"], "Matrix binding.secret_file", maximum=4096)
    if not os.path.isabs(secret_file):
        raise ValueError("Matrix binding.secret_file must be absolute")
    return binding


def env_flag(name):
    value = os.environ.get(name)
    if value is None:
        return None
    value = value.strip().lower()
    if value in {"1", "true", "yes", "on"}:
        return True
    if value in {"0", "false", "no", "off"}:
        return False
    raise ValueError(f"{name} must be a boolean")


def env_values(name, validator):
    raw = os.environ.get(name, "")
    values = set()
    for part in raw.split(","):
        part = part.strip()
        if part:
            values.add(validator(part, name))
    return values


def event_content(event):
    content = event.get("content")
    return content if isinstance(content, dict) else {}


def relation(event):
    value = event_content(event).get("m.relates_to")
    return value if isinstance(value, dict) else {}


def event_thread(event):
    rel = relation(event)
    if rel.get("rel_type") == "m.thread":
        value = rel.get("event_id")
        if isinstance(value, str) and EVENT_ID_RE.fullmatch(value):
            return value
    return None


def event_reply(event):
    value = relation(event).get("m.in_reply_to")
    if isinstance(value, dict):
        event_id = value.get("event_id")
        if isinstance(event_id, str) and EVENT_ID_RE.fullmatch(event_id):
            return event_id
    return None


def event_key(event):
    event_id = event.get("event_id")
    if isinstance(event_id, str) and event_id:
        return "event-" + hashlib.sha256(event_id.encode("utf-8")).hexdigest()
    raw = json.dumps(event, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return "digest-" + hashlib.sha256(raw).hexdigest()


def event_coordinate(event, auto_thread_rooms=True, auto_thread_dms=False):
    room_id = event.get("room_id")
    if not isinstance(room_id, str):
        return None, None
    thread = event_thread(event)
    if thread:
        return room_id, thread
    direct = event.get("room", {}).get("is_direct") if isinstance(event.get("room"), dict) else False
    if event.get("type") == "m.room.message" and ((direct and auto_thread_dms) or (not direct and auto_thread_rooms)):
        event_id = event.get("event_id")
        if isinstance(event_id, str) and EVENT_ID_RE.fullmatch(event_id):
            return room_id, event_id
    return room_id, None


def server_from_mxid(value):
    if not isinstance(value, str) or ":" not in value:
        return None
    return value.rsplit(":", 1)[-1].lower()


def configured_sets(config):
    authz = config["authz"]
    gating = config["gating"]
    users = set(authz["allowed_user_ids"]) | env_values(authz["allowed_user_ids_env"], require_mxid)
    rooms = set(authz["allowed_room_ids"]) | env_values(authz["allowed_room_ids_env"], require_room)
    servers = set(authz["allowed_server_names"]) | env_values(authz["allowed_server_names_env"], require_server)
    inviters = set(authz["allowed_inviter_ids"]) | env_values(authz["allowed_inviter_ids_env"], require_mxid)
    free_rooms = set(gating["free_response_room_ids"]) | env_values(gating["free_response_room_ids_env"], require_room)
    patterns = list(authz["ignored_user_patterns"])
    patterns.extend(part.strip() for part in os.environ.get(authz["ignored_user_patterns_env"], "").split(",") if part.strip())
    compiled = []
    for pattern in patterns:
        try:
            compiled.append(re.compile(pattern))
        except re.error as error:
            raise ValueError(f"Matrix ignored user pattern is invalid: {error}") from error
    override = env_flag(authz["allow_all_users_env"])
    allow_all = authz["allow_all_users"] if override is None else override
    return {"users": users, "rooms": rooms, "servers": servers, "inviters": inviters, "free_rooms": free_rooms, "ignored": compiled, "allow_all": allow_all}


def authorized(config, event, own_user_id):
    sets = configured_sets(config)
    sender = event.get("sender")
    room_id = event.get("room_id")
    event_type = event.get("type")
    if not isinstance(sender, str) or not MXID_RE.fullmatch(sender):
        return False, "sender_missing_or_malformed"
    if own_user_id and sender.casefold() == own_user_id.casefold():
        return False, "self_sender"
    if any(pattern.search(sender) for pattern in sets["ignored"]):
        return False, "ignored_sender"
    if sets["servers"] and server_from_mxid(sender) not in sets["servers"]:
        return False, "server_not_allowed"
    if sets["rooms"] and room_id not in sets["rooms"]:
        return False, "room_not_allowed"
    if event_type == "m.room.member" and event.get("membership") == "invite":
        if sets["inviters"] and sender not in sets["inviters"]:
            return False, "inviter_not_allowed"
    if sets["allow_all"] or sender in sets["users"]:
        return True, "allowed"
    if not sets["users"] and not config["authz"]["deny_unmatched"]:
        return True, "allowed_without_user_rule"
    return False, "user_not_allowed"


def mentioned(event, own_user_id):
    content = event_content(event)
    mentions = content.get("m.mentions")
    if isinstance(mentions, dict):
        users = mentions.get("user_ids")
        if isinstance(users, list) and own_user_id in users:
            return True
    body = content.get("body")
    formatted = content.get("formatted_body")
    if isinstance(body, str) and own_user_id and own_user_id.casefold() in body.casefold():
        return True
    if isinstance(formatted, str) and own_user_id and f"matrix.to/#/{own_user_id}" in formatted:
        return True
    if own_user_id and ":" in own_user_id and isinstance(body, str):
        localpart = own_user_id.split(":", 1)[0].lstrip("@")
        return bool(localpart and re.search(r"(?<![\w])@" + re.escape(localpart) + r"\b", body, re.IGNORECASE))
    return False


def strip_mention(text, own_user_id):
    if not isinstance(text, str):
        return ""
    if own_user_id:
        text = re.sub(re.escape(own_user_id), "", text, flags=re.IGNORECASE)
        localpart = own_user_id.split(":", 1)[0].lstrip("@") if ":" in own_user_id else ""
        if localpart:
            text = re.sub(r"(?<![\w])@" + re.escape(localpart) + r"\b", "", text, flags=re.IGNORECASE)
    text = re.sub(r"[ \t]{2,}", " ", text)
    text = re.sub(r"\s+([,.;:!?])", r"\1", text)
    return text.strip()


def strip_reply_fallback(text):
    if not isinstance(text, str) or not text.startswith(">"):
        return text if isinstance(text, str) else ""
    lines = text.splitlines()
    index = 0
    while index < len(lines) and (lines[index].startswith("> ") or lines[index] == ">"):
        index += 1
    if index < len(lines) and lines[index] == "":
        index += 1
    return "\n".join(lines[index:]) if index < len(lines) else text


def should_admit(config, event, own_user_id, participated_threads):
    admitted, reason = authorized(config, event, own_user_id)
    if not admitted:
        return False, reason
    event_type = event.get("type")
    content = event_content(event)
    if event_type == "m.room.message":
        msgtype = content.get("msgtype")
        if msgtype == "m.notice" and not config["gating"]["accept_notices"]:
            return False, "notice_disabled"
        rel = relation(event)
        if rel.get("rel_type") == "m.replace" and not config["gating"]["accept_edits"]:
            return False, "edits_disabled"
        room = event.get("room") if isinstance(event.get("room"), dict) else {}
        direct = room.get("is_direct") is True
        if direct and not config["gating"]["respond_in_dms"]:
            return False, "dm_disabled"
        thread = event_thread(event)
        is_mentioned = mentioned(event, own_user_id)
        free_rooms = configured_sets(config)["free_rooms"]
        command = isinstance(content.get("body"), str) and content["body"].lstrip().startswith(("/", "!"))
        if not direct and config["gating"]["require_mention_in_rooms"] and event.get("room_id") not in free_rooms:
            if not is_mentioned and not command and not (thread and thread in participated_threads):
                return False, "mention_required"
        if thread and thread in participated_threads and config["gating"]["thread_require_mention"] and not is_mentioned and event.get("room_id") not in free_rooms:
            return False, "thread_mention_required"
    gate = {
        "m.room.redaction": "accept_redactions",
        "m.reaction": "accept_reactions",
        "m.room.member": "accept_membership",
        "m.receipt": "accept_receipts",
        "m.typing": "accept_typing",
        "m.presence": "accept_presence",
    }.get(event_type)
    if gate and not config["gating"][gate]:
        return False, f"{event_type}_disabled"
    return True, "admitted"


def normalize_operation(body, config, default_target=None):
    body = require_object(body, "Matrix outbound payload")
    target = dict(default_target or {})
    if "operation" not in body:
        body = require_keys(body, {"text"}, {"room_id", "thread_event_id", "reply_to_event_id", "formatted_html", "notice"}, "Matrix reply")
        args = dict(body)
        args.setdefault("room_id", target.get("room_id"))
        args.setdefault("thread_event_id", target.get("thread_event_id"))
        args.setdefault("reply_to_event_id", target.get("reply_to_event_id"))
        return "message.send", args
    body = require_keys(body, {"operation", "args"}, set(), "Matrix operation payload")
    operation = require_string(body["operation"], "Matrix operation", maximum=128)
    if operation not in config["outbound"]["allowed_operations"]:
        raise ValueError(f"Matrix operation {operation!r} is not declared")
    args = require_object(body["args"], "Matrix operation.args")
    if "room_id" not in args and target.get("room_id") and operation not in {"presence.set", "device.keys.query", "device.verify", "account.data.get"}:
        args = dict(args)
        args["room_id"] = target["room_id"]
    if "thread_event_id" not in args and target.get("thread_event_id") and operation in {"message.send", "message.edit", "media.send", "location.send"}:
        args = dict(args)
        args["thread_event_id"] = target["thread_event_id"]
    if "reply_to_event_id" not in args and target.get("reply_to_event_id") and operation in {"message.send", "media.send", "location.send"}:
        args = dict(args)
        args["reply_to_event_id"] = target["reply_to_event_id"]
    return operation, args


def normalized_event(event, evidence):
    event = require_object(event, "Matrix event")
    content = event_content(event)
    rel = relation(event)
    room = event.get("room") if isinstance(event.get("room"), dict) else {}
    result = {
        "schema_version": 1,
        "event_key": event_key(event),
        "event_id": event.get("event_id"),
        "event_type": event.get("type"),
        "sender": event.get("sender"),
        "origin_server_ts": event.get("origin_server_ts"),
        "room_id": event.get("room_id"),
        "room": room,
        "thread_event_id": event_thread(event),
        "reply_to_event_id": event_reply(event),
        "relation_type": rel.get("rel_type"),
        "relation_key": rel.get("key"),
        "redacts_event_id": event.get("redacts") or content.get("redacts"),
        "membership": event.get("membership") or content.get("membership"),
        "content": content,
        "unsigned": event.get("unsigned") if isinstance(event.get("unsigned"), dict) else {},
        "decryption": event.get("decryption") if isinstance(event.get("decryption"), dict) else {},
        "evidence": evidence,
    }
    return result


def binding_registry(value):
    value = require_keys(value, {"schema_version", "bindings"}, set(), "Matrix binding registry")
    if value["schema_version"] != 1 or not isinstance(value["bindings"], list):
        raise ValueError("Matrix binding registry is malformed")
    result = {}
    for index, row in enumerate(value["bindings"]):
        row = require_keys(row, {"interface", "room_id", "thread_event_id", "secret_sha256"}, set(), f"Matrix binding registry.bindings[{index}]")
        interface = require_string(row["interface"], f"Matrix binding registry.bindings[{index}].interface", maximum=512)
        room_id = require_room(row["room_id"], f"Matrix binding registry.bindings[{index}].room_id")
        if not ROOM_ID_RE.fullmatch(room_id):
            raise ValueError("Matrix binding registry room_id must be a concrete room id")
        thread_id = require_event(row["thread_event_id"], f"Matrix binding registry.bindings[{index}].thread_event_id", nullable=True)
        digest = require_digest(row["secret_sha256"], f"Matrix binding registry.bindings[{index}].secret_sha256")
        coordinate = (room_id, thread_id)
        if coordinate in result:
            raise ValueError("Matrix binding registry repeats coordinates")
        result[coordinate] = {"interface": interface, "secret_sha256": digest}
    return result


def canonical_json(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def transaction_id(seed):
    digest = hashlib.sha256(seed.encode("utf-8")).hexdigest()
    return "octopus_" + digest[:48]
