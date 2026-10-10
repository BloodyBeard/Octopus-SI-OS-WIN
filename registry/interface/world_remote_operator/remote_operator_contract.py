import base64
import datetime
import hashlib
import ipaddress
import json
import os
import re
import urllib.parse


OPERATION_IDS = frozenset(
    {
        "remote_operator.capabilities.status",
        "remote_operator.connection.enable",
        "remote_operator.connection.disable",
        "remote_operator.connection.status",
        "remote_operator.pairing.start",
        "remote_operator.pairing.status",
        "remote_operator.clients.list",
        "remote_operator.clients.revoke",
        "remote_operator.deliver",
    }
)
OPERATION_OUTPUT_LABELS = {
    "remote_operator.capabilities.status": "remote_operator.status",
    "remote_operator.connection.enable": "remote_operator.status",
    "remote_operator.connection.disable": "remote_operator.status",
    "remote_operator.connection.status": "remote_operator.status",
    "remote_operator.pairing.start": "remote_operator.pairing",
    "remote_operator.pairing.status": "remote_operator.pairing",
    "remote_operator.clients.list": "remote_operator.client_catalog",
    "remote_operator.clients.revoke": "remote_operator.client_catalog",
    "remote_operator.deliver": "remote_operator.delivery",
}
CONNECTION_STATES = frozenset({"disabled", "connecting", "connected", "errored"})
DESIRED_MODES = frozenset({"disabled", "ephemeral", "durable"})
_DISALLOWED_MESSAGE_LABEL_PREFIXES = ("app_server.", "thread.", "session.", "conversation.", "provider.")
_DISALLOWED_MESSAGE_PAYLOAD_KEYS = frozenset({"jsonrpc", "method", "session_id", "conversation_id", "provider_call"})


class ContractError(ValueError):
    pass


def require_object(value, name):
    if not isinstance(value, dict) or any(not isinstance(key, str) for key in value):
        raise ContractError(f"{name} must be a JSON object with string keys")
    return value


def require_keys(value, required, optional, name):
    value = require_object(value, name)
    missing = sorted(set(required) - set(value))
    unknown = sorted(set(value) - set(required) - set(optional))
    if missing:
        raise ContractError(f"{name} is missing keys: {', '.join(missing)}")
    if unknown:
        raise ContractError(f"{name} has unknown keys: {', '.join(unknown)}")
    return value


def require_string(value, name, maximum=None, allow_empty=False):
    if not isinstance(value, str) or value.strip() != value or "\x00" in value:
        raise ContractError(f"{name} must be a trimmed string without NUL")
    if not allow_empty and not value:
        raise ContractError(f"{name} must not be empty")
    if maximum is not None and len(value) > maximum:
        raise ContractError(f"{name} exceeds {maximum} characters")
    return value


def require_int(value, name, minimum=None, maximum=None):
    if isinstance(value, bool) or not isinstance(value, int):
        raise ContractError(f"{name} must be an integer")
    if minimum is not None and value < minimum:
        raise ContractError(f"{name} must be at least {minimum}")
    if maximum is not None and value > maximum:
        raise ContractError(f"{name} must be at most {maximum}")
    return value


def require_bool(value, name):
    if not isinstance(value, bool):
        raise ContractError(f"{name} must be a boolean")
    return value


def read_json(path, name):
    with open(path, "r", encoding="utf-8") as handle:
        return require_object(json.load(handle), name)


def _env_name(value, name):
    value = require_string(value, name, 128)
    if not re.fullmatch(r"[A-Z][A-Z0-9_]*", value):
        raise ContractError(f"{name} is not an environment variable name")
    return value


def validate_config(value):
    value = require_keys(value, {"schema_version", "transport", "protocol", "contract", "driver_ipc"}, set(), "remote operator config")
    if value["schema_version"] != 1:
        raise ContractError("remote operator config schema_version must be 1")
    transport = require_keys(
        value["transport"],
        {
            "base_url_env",
            "auth_token_env",
            "auth_kind_env",
            "account_id_env",
            "server_name_env",
            "installation_id_env",
            "request_timeout_ms",
            "max_response_bytes",
            "user_agent",
        },
        set(),
        "remote operator transport",
    )
    for key in ("base_url_env", "auth_token_env", "auth_kind_env", "account_id_env", "server_name_env", "installation_id_env"):
        _env_name(transport[key], f"remote operator transport.{key}")
    require_int(transport["request_timeout_ms"], "remote operator transport.request_timeout_ms", 1000, 600000)
    require_int(transport["max_response_bytes"], "remote operator transport.max_response_bytes", 1024, 268435456)
    require_string(transport["user_agent"], "remote operator transport.user_agent", 256)
    protocol = require_keys(
        value["protocol"],
        {
            "version",
            "segment_target_bytes",
            "segment_max_bytes",
            "reassembled_max_bytes",
            "segment_count_max",
            "assembly_count_max",
            "outbound_buffer_max",
            "connect_timeout_ms",
            "enable_ready_timeout_ms",
            "ping_interval_ms",
            "pong_timeout_ms",
            "shutdown_timeout_ms",
            "reconnect_backoff_ms",
            "reconnect_backoff_cap_ms",
            "token_refresh_skew_ms",
            "body_preview_bytes",
        },
        set(),
        "remote operator protocol",
    )
    if protocol["version"] != "3":
        raise ContractError("remote operator protocol version must be 3")
    for key in protocol:
        if key == "version":
            continue
        require_int(protocol[key], f"remote operator protocol.{key}", 1, 536870912)
    if protocol["segment_target_bytes"] > protocol["segment_max_bytes"]:
        raise ContractError("remote operator segment target exceeds its envelope limit")
    if protocol["segment_max_bytes"] > protocol["reassembled_max_bytes"]:
        raise ContractError("remote operator segment envelope exceeds the reassembled limit")
    if protocol["reconnect_backoff_ms"] > protocol["reconnect_backoff_cap_ms"]:
        raise ContractError("remote operator reconnect backoff exceeds its cap")
    if protocol["ping_interval_ms"] >= protocol["pong_timeout_ms"]:
        raise ContractError("remote operator pong timeout must exceed ping interval")
    contract = require_keys(
        value["contract"],
        {
            "accepted_labels",
            "max_client_id_chars",
            "max_stream_id_chars",
            "max_pairing_code_chars",
            "max_clients_limit",
            "operation_catalog_dir",
        },
        set(),
        "remote operator contract",
    )
    if contract["accepted_labels"] != ["remote_operator.request"]:
        raise ContractError("remote operator accepts only remote_operator.request")
    require_int(contract["max_client_id_chars"], "remote operator max_client_id_chars", 1, 4096)
    require_int(contract["max_stream_id_chars"], "remote operator max_stream_id_chars", 1, 4096)
    require_int(contract["max_pairing_code_chars"], "remote operator max_pairing_code_chars", 1, 4096)
    require_int(contract["max_clients_limit"], "remote operator max_clients_limit", 1, 100)
    catalog = require_string(contract["operation_catalog_dir"], "remote operator operation_catalog_dir", 128)
    if os.path.basename(catalog) != catalog:
        raise ContractError("remote operator operation catalog must be interface-relative")
    ipc = require_keys(
        value["driver_ipc"],
        {
            "ledger_batch_size",
            "idle_sleep_ms",
            "verdict_timeout_ms",
            "max_cached_manifest_bytes",
            "max_outbound_payload_bytes",
            "ledger_fallback_max_line_bytes",
        },
        set(),
        "remote operator driver_ipc",
    )
    for key in ipc:
        require_int(ipc[key], f"remote operator driver_ipc.{key}", 1, 536870912)
    return value


def load_config(path):
    return validate_config(read_json(path, "remote operator config"))


def validate_activation(value, interface_id="world_remote_operator"):
    value = require_keys(value, {"schema_version", "interface", "activations"}, set(), "remote operator activation")
    if value["schema_version"] != 1 or value["interface"] != interface_id:
        raise ContractError("remote operator activation identity is invalid")
    activations = value["activations"]
    if not isinstance(activations, list) or len(activations) != 1:
        raise ContractError("remote operator activation must declare one activation")
    activation = require_keys(activations[0], {"enabled", "activation_driver_ref", "config"}, set(), "remote operator activation entry")
    require_bool(activation["enabled"], "remote operator activation enabled")
    if activation["activation_driver_ref"] != "app_server_transport":
        raise ContractError("remote operator activation must use app_server_transport")
    config = require_keys(activation["config"], {"bind_interface_socket_when_env_unset"}, set(), "remote operator activation config")
    if config["bind_interface_socket_when_env_unset"] is not True:
        raise ContractError("remote operator activation must bind its interface socket")
    return value


def load_operation_catalog(interface_dir):
    operations_dir = os.path.join(interface_dir, "operations")
    found = {}
    for name in sorted(os.listdir(operations_dir)):
        if not name.endswith(".json"):
            raise ContractError(f"remote operator operation catalog contains unexpected entry {name!r}")
        value = read_json(os.path.join(operations_dir, name), f"remote operator operation {name}")
        value = require_keys(
            value,
            {"id", "request_label", "output_label", "handler_config", "description", "input_schema"},
            set(),
            f"remote operator operation {name}",
        )
        identifier = require_string(value["id"], f"remote operator operation {name}.id", 256)
        if identifier in found:
            raise ContractError(f"remote operator operation {identifier!r} is duplicated")
        if value["request_label"] != "remote_operator.request":
            raise ContractError(f"remote operator operation {identifier} request label drifted")
        if value["output_label"] != OPERATION_OUTPUT_LABELS.get(identifier):
            raise ContractError(f"remote operator operation {identifier} output label drifted")
        handler = require_keys(value["handler_config"], {"operation"}, set(), f"remote operator operation {identifier}.handler_config")
        operation_name = require_string(handler["operation"], f"remote operator operation {identifier}.handler_config.operation", 128)
        if operation_name != identifier.removeprefix("remote_operator."):
            raise ContractError(f"remote operator operation {identifier} handler operation drifted")
        require_object(value["input_schema"], f"remote operator operation {identifier}.input_schema")
        found[identifier] = value
    if set(found) != set(OPERATION_IDS):
        raise ContractError(f"remote operator operation catalog drifted; found={sorted(found)}")
    return found


def normalize_target(base_url):
    base_url = require_string(base_url, "remote operator base URL", 4096)
    parsed = urllib.parse.urlsplit(base_url)
    host = (parsed.hostname or "").lower()
    try:
        localhost = ipaddress.ip_address(host).is_loopback
    except ValueError:
        localhost = host == "localhost"
    chat_host = host == "chatgpt.com" or host == "chatgpt-staging.com" or host.endswith(".chatgpt.com") or host.endswith(".chatgpt-staging.com")
    if parsed.scheme == "https" and (localhost or chat_host):
        pass
    elif parsed.scheme == "http" and localhost:
        pass
    else:
        raise ContractError("remote operator base URL requires HTTPS chatgpt.com/chatgpt-staging.com or HTTP/HTTPS loopback")
    if parsed.username or parsed.password or parsed.query or parsed.fragment or not parsed.hostname:
        raise ContractError("remote operator base URL contains unsupported authority data")
    path = parsed.path
    if not path.endswith("/"):
        path += "/"
    normalized = urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, path, "", ""))
    def join(suffix):
        return urllib.parse.urljoin(normalized, suffix)
    websocket = urllib.parse.urlsplit(join("wham/remote/control/server"))
    websocket_url = urllib.parse.urlunsplit(("wss" if websocket.scheme == "https" else "ws", websocket.netloc, websocket.path, "", ""))
    return {
        "base_url": normalized,
        "websocket_url": websocket_url,
        "enroll_url": join("wham/remote/control/server/enroll"),
        "refresh_url": join("wham/remote/control/server/refresh"),
        "pair_url": join("wham/remote/control/server/pair"),
        "pair_status_url": join("wham/remote/control/server/pair/status"),
        "environment_clients_url": join("wham/remote/control/environments"),
    }


def _optional_string(value, key, maximum, name="remote operator arguments"):
    if key not in value:
        return None
    return require_string(value[key], f"{name}.{key}", maximum)


def validate_artifact_message(value, name="remote operator message"):
    value = require_keys(value, {"schema_version", "label", "payload"}, set(), name)
    if value["schema_version"] != 1:
        raise ContractError(f"{name} schema_version must be 1")
    label = require_string(value["label"], f"{name}.label", 256)
    if label.startswith(_DISALLOWED_MESSAGE_LABEL_PREFIXES):
        raise ContractError(f"{name} label imports forbidden app-server/session/provider ontology")
    payload = require_object(value["payload"], f"{name}.payload")
    overlap = sorted(set(payload) & _DISALLOWED_MESSAGE_PAYLOAD_KEYS)
    if overlap:
        raise ContractError(f"{name} payload imports forbidden RPC/session keys: {', '.join(overlap)}")
    return {"schema_version": 1, "label": label, "payload": payload}


def validate_request(value, config):
    value = require_keys(value, {"schema_version", "request_id", "operation", "arguments"}, set(), "remote operator request")
    if value["schema_version"] != 1:
        raise ContractError("remote operator request schema_version must be 1")
    request_id = require_string(value["request_id"], "remote operator request.request_id", 256)
    operation = require_string(value["operation"], "remote operator request.operation", 256)
    if operation not in OPERATION_IDS:
        raise ContractError(f"remote operator operation {operation!r} is unsupported")
    arguments = require_object(value["arguments"], "remote operator request.arguments")
    if operation in {"remote_operator.capabilities.status", "remote_operator.connection.disable", "remote_operator.connection.status"}:
        require_keys(arguments, set(), set(), f"remote operator {operation} arguments")
    elif operation == "remote_operator.connection.enable":
        require_keys(arguments, set(), {"mode"}, "remote operator connection.enable arguments")
        mode = arguments.get("mode", "durable")
        if mode not in {"ephemeral", "durable"}:
            raise ContractError("remote operator connection mode must be ephemeral or durable")
    elif operation == "remote_operator.pairing.start":
        require_keys(arguments, set(), {"manual_code"}, "remote operator pairing.start arguments")
        require_bool(arguments.get("manual_code", True), "remote operator pairing.start manual_code")
    elif operation == "remote_operator.pairing.status":
        require_keys(arguments, set(), {"pairing_code", "manual_pairing_code"}, "remote operator pairing.status arguments")
        present = [key for key in ("pairing_code", "manual_pairing_code") if key in arguments]
        if len(present) != 1:
            raise ContractError("remote operator pairing.status requires exactly one pairing code")
        _optional_string(arguments, present[0], config["contract"]["max_pairing_code_chars"])
    elif operation == "remote_operator.clients.list":
        require_keys(arguments, {"environment_id"}, {"cursor", "limit", "order"}, "remote operator clients.list arguments")
        require_string(arguments["environment_id"], "remote operator environment_id", 512)
        _optional_string(arguments, "cursor", 4096)
        require_int(arguments.get("limit", config["contract"]["max_clients_limit"]), "remote operator clients limit", 1, config["contract"]["max_clients_limit"])
        if arguments.get("order", "desc") not in {"asc", "desc"}:
            raise ContractError("remote operator clients order must be asc or desc")
    elif operation == "remote_operator.clients.revoke":
        require_keys(arguments, {"environment_id", "client_id"}, set(), "remote operator clients.revoke arguments")
        require_string(arguments["environment_id"], "remote operator environment_id", 512)
        require_string(arguments["client_id"], "remote operator client_id", config["contract"]["max_client_id_chars"])
    elif operation == "remote_operator.deliver":
        require_keys(arguments, {"client_id", "stream_id", "message"}, set(), "remote operator deliver arguments")
        require_string(arguments["client_id"], "remote operator deliver client_id", config["contract"]["max_client_id_chars"])
        require_string(arguments["stream_id"], "remote operator deliver stream_id", config["contract"]["max_stream_id_chars"])
        validate_artifact_message(arguments["message"], "remote operator deliver message")
    return {"schema_version": 1, "request_id": request_id, "operation": operation, "arguments": arguments}


def parse_rfc3339(value, name):
    value = require_string(value, name, 128)
    normalized = value[:-1] + "+00:00" if value.endswith("Z") else value
    try:
        parsed = datetime.datetime.fromisoformat(normalized)
    except ValueError as error:
        raise ContractError(f"{name} is not RFC3339") from error
    if parsed.tzinfo is None:
        raise ContractError(f"{name} must include a timezone")
    return parsed


def validate_private_state(value):
    value = require_keys(
        value,
        {"schema_version", "desired_mode", "connection_state", "subscribe_cursor", "last_error", "enrollment", "next_seq_by_stream", "acked_by_stream", "outbound_buffer"},
        set(),
        "remote operator private state",
    )
    if value["schema_version"] != 1:
        raise ContractError("remote operator private state schema_version must be 1")
    if value["desired_mode"] not in DESIRED_MODES:
        raise ContractError("remote operator private state desired_mode is invalid")
    if value["connection_state"] not in CONNECTION_STATES:
        raise ContractError("remote operator private state connection_state is invalid")
    if value["subscribe_cursor"] is not None:
        require_string(value["subscribe_cursor"], "remote operator subscribe cursor", 4096)
    if value["last_error"] is not None:
        require_string(value["last_error"], "remote operator last error", 4096)
    enrollment = value["enrollment"]
    if enrollment is not None:
        enrollment = require_keys(
            enrollment,
            {"base_url", "account_id", "server_id", "environment_id", "server_name", "installation_id", "remote_control_token", "expires_at"},
            set(),
            "remote operator enrollment",
        )
        normalize_target(enrollment["base_url"])
        for key in ("account_id", "server_id", "environment_id", "server_name", "installation_id", "remote_control_token"):
            require_string(enrollment[key], f"remote operator enrollment.{key}", 32768 if key == "remote_control_token" else 4096)
        parse_rfc3339(enrollment["expires_at"], "remote operator enrollment.expires_at")
    for key in ("next_seq_by_stream", "acked_by_stream"):
        rows = require_object(value[key], f"remote operator private state.{key}")
        for stream, seq in rows.items():
            require_string(stream, f"remote operator private state.{key} stream", 2048)
            require_int(seq, f"remote operator private state.{key} seq", 0)
    if not isinstance(value["outbound_buffer"], list):
        raise ContractError("remote operator private state.outbound_buffer must be an array")
    for index, item in enumerate(value["outbound_buffer"]):
        item = require_keys(
            item,
            {"file", "sha256", "client_id", "stream_id", "seq_id", "segment_id"},
            set(),
            f"remote operator outbound buffer item {index}",
        )
        file_name = require_string(item["file"], f"remote operator outbound buffer item {index}.file", 256)
        if os.path.basename(file_name) != file_name or not file_name.endswith(".json"):
            raise ContractError("remote operator outbound buffer file is not a safe basename")
        if not re.fullmatch(r"[0-9a-f]{64}", require_string(item["sha256"], f"remote operator outbound buffer item {index}.sha256", 64)):
            raise ContractError("remote operator outbound buffer SHA-256 is malformed")
        require_string(item["client_id"], f"remote operator outbound buffer item {index}.client_id", 512)
        require_string(item["stream_id"], f"remote operator outbound buffer item {index}.stream_id", 512)
        require_int(item["seq_id"], f"remote operator outbound buffer item {index}.seq_id", 0)
        if item["segment_id"] is not None:
            require_int(item["segment_id"], f"remote operator outbound buffer item {index}.segment_id", 0)
    return value


def default_private_state():
    return {
        "schema_version": 1,
        "desired_mode": "disabled",
        "connection_state": "disabled",
        "subscribe_cursor": None,
        "last_error": None,
        "enrollment": None,
        "next_seq_by_stream": {},
        "acked_by_stream": {},
        "outbound_buffer": [],
    }


def public_status(state, interface_live, last_transport_evidence=None):
    state = validate_private_state(state)
    enrollment = state["enrollment"]
    return {
        "schema_version": 1,
        "interface": "world_remote_operator",
        "interface_process": "live" if interface_live else "not_live",
        "desired_mode": state["desired_mode"],
        "connection_state": state["connection_state"],
        "server_name": enrollment["server_name"] if enrollment else None,
        "server_id": enrollment["server_id"] if enrollment else None,
        "environment_id": enrollment["environment_id"] if enrollment else None,
        "token_expires_at": enrollment["expires_at"] if enrollment else None,
        "subscribe_cursor_present": state["subscribe_cursor"] is not None,
        "last_error": state["last_error"],
        "last_transport_evidence": last_transport_evidence,
    }


def redact(value, secrets, maximum=4096):
    text = str(value)
    for secret in secrets:
        if isinstance(secret, str) and secret:
            text = text.replace(secret, "[REDACTED]")
    text = re.sub(r'("?(?:remote_control_token|pairing_code|manual_pairing_code)"?\s*[:=]\s*)[^,}\s]+', r"\1[REDACTED]", text, flags=re.IGNORECASE)
    return text[:maximum]


def normalize_clients(value):
    response = require_object(value, "remote operator clients response")
    items = response.get("items")
    if not isinstance(items, list):
        raise ContractError("remote operator clients response has no items array")
    result = []
    for item in items:
        item = require_object(item, "remote operator client")
        client_id = require_string(item.get("client_id"), "remote operator client_id", 512)
        normalized = {"client_id": client_id}
        for key in ("display_name", "device_type", "platform", "os_version", "device_model", "app_version", "last_seen_at"):
            field_value = item.get(key)
            if field_value is not None:
                field_value = require_string(field_value, f"remote operator client.{key}", 4096)
                if key == "last_seen_at":
                    parse_rfc3339(field_value, "remote operator client.last_seen_at")
            normalized[key] = field_value
        result.append(normalized)
    cursor = response.get("cursor")
    if cursor is not None:
        cursor = require_string(cursor, "remote operator clients cursor", 4096)
    return {"schema_version": 1, "clients": result, "next_cursor": cursor}


def _canonical_message_bytes(message):
    return json.dumps(validate_artifact_message(message), ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")


def split_server_message(client_id, stream_id, seq_id, message, protocol):
    client_id = require_string(client_id, "remote operator client_id", 512)
    stream_id = require_string(stream_id, "remote operator stream_id", 512)
    seq_id = require_int(seq_id, "remote operator seq_id", 0)
    raw = _canonical_message_bytes(message)
    if len(raw) > protocol["reassembled_max_bytes"]:
        raise ContractError("remote operator outbound message exceeds its reassembled bound")
    direct = {"type": "server_message", "client_id": client_id, "stream_id": stream_id, "seq_id": seq_id, "message": json.loads(raw)}
    direct_size = len(json.dumps(direct, separators=(",", ":")).encode("utf-8"))
    if direct_size <= protocol["segment_max_bytes"]:
        return [direct]
    target = protocol["segment_target_bytes"]
    chunks = [raw[index : index + target] for index in range(0, len(raw), target)]
    if not chunks or len(chunks) > protocol["segment_count_max"]:
        raise ContractError("remote operator outbound message requires too many segments")
    envelopes = []
    for index, chunk in enumerate(chunks):
        envelope = {
            "type": "server_message_chunk",
            "client_id": client_id,
            "stream_id": stream_id,
            "seq_id": seq_id,
            "segment_id": index,
            "segment_count": len(chunks),
            "message_size_bytes": len(raw),
            "message_chunk_base64": base64.b64encode(chunk).decode("ascii"),
        }
        if len(json.dumps(envelope, separators=(",", ":")).encode("utf-8")) > protocol["segment_max_bytes"]:
            raise ContractError("remote operator segmented envelope exceeds its wire bound")
        envelopes.append(envelope)
    return envelopes


class ClientSegmentReassembler:
    def __init__(self, protocol):
        self.protocol = protocol
        self.assemblies = {}
        self.completed = {}

    def _key(self, envelope):
        return envelope["client_id"], envelope["stream_id"], envelope["seq_id"]

    def observe(self, value):
        value = require_object(value, "remote operator client envelope")
        event_type = require_string(value.get("type"), "remote operator client envelope type", 64)
        client_id = require_string(value.get("client_id"), "remote operator client envelope client_id", 512)
        stream_id = value.get("stream_id")
        if stream_id is not None:
            stream_id = require_string(stream_id, "remote operator client envelope stream_id", 512)
        seq_id = value.get("seq_id")
        if seq_id is not None:
            seq_id = require_int(seq_id, "remote operator client envelope seq_id", 0)
        cursor = value.get("cursor")
        if cursor is not None:
            cursor = require_string(cursor, "remote operator client envelope cursor", 4096)
        base = {"client_id": client_id, "stream_id": stream_id, "seq_id": seq_id, "cursor": cursor}
        if event_type == "client_message":
            require_keys(value, {"type", "client_id", "message"}, {"stream_id", "seq_id", "cursor"}, "remote operator client message")
            return "message", {**base, "message": validate_artifact_message(value["message"], "remote operator client message body")}
        if event_type == "client_message_chunk":
            require_keys(
                value,
                {"type", "client_id", "stream_id", "seq_id", "segment_id", "segment_count", "message_size_bytes", "message_chunk_base64"},
                {"cursor"},
                "remote operator client message chunk",
            )
            if stream_id is None or seq_id is None:
                raise ContractError("remote operator client chunk requires stream_id and seq_id")
            completed_key = (client_id, stream_id)
            if self.completed.get(completed_key, -1) >= seq_id:
                return "duplicate", base
            segment_id = require_int(value["segment_id"], "remote operator segment_id", 0)
            segment_count = require_int(value["segment_count"], "remote operator segment_count", 1, self.protocol["segment_count_max"])
            size = require_int(value["message_size_bytes"], "remote operator message_size_bytes", 1, self.protocol["reassembled_max_bytes"])
            if segment_id >= segment_count:
                raise ContractError("remote operator segment_id is outside segment_count")
            encoded = require_string(value["message_chunk_base64"], "remote operator message_chunk_base64")
            try:
                chunk = base64.b64decode(encoded, validate=True)
            except Exception as error:
                raise ContractError("remote operator message chunk is not canonical base64") from error
            if len(chunk) > self.protocol["segment_target_bytes"]:
                raise ContractError("remote operator message chunk exceeds its target bound")
            key = self._key(base)
            assembly = self.assemblies.get(key)
            if assembly is None:
                if segment_id != 0:
                    raise ContractError("remote operator segmented message did not start at segment zero")
                if len(self.assemblies) >= self.protocol["assembly_count_max"]:
                    raise ContractError("remote operator segmented assembly capacity is full")
                assembly = {"segment_count": segment_count, "size": size, "next": 0, "raw": bytearray(), "cursor": cursor}
                self.assemblies[key] = assembly
            if assembly["segment_count"] != segment_count or assembly["size"] != size:
                self.assemblies.pop(key, None)
                raise ContractError("remote operator segmented message metadata changed")
            if segment_id < assembly["next"]:
                return "pending", base
            if segment_id > assembly["next"]:
                self.assemblies.pop(key, None)
                raise ContractError("remote operator segmented message order changed")
            assembly["raw"].extend(chunk)
            assembly["next"] += 1
            if len(assembly["raw"]) > size:
                self.assemblies.pop(key, None)
                raise ContractError("remote operator segmented message exceeded its declared size")
            if assembly["next"] < segment_count:
                return "pending", base
            self.assemblies.pop(key, None)
            self.completed[completed_key] = seq_id
            if len(assembly["raw"]) != size:
                raise ContractError("remote operator segmented message size did not match")
            try:
                message = validate_artifact_message(require_object(json.loads(bytes(assembly["raw"])), "remote operator reassembled message"), "remote operator reassembled message")
            except Exception as error:
                raise ContractError(f"remote operator reassembled message is invalid: {error}") from error
            return "message", {**base, "message": message}
        if event_type == "ack":
            require_keys(value, {"type", "client_id", "stream_id", "seq_id"}, {"segment_id", "cursor"}, "remote operator ack")
            if stream_id is None or seq_id is None:
                raise ContractError("remote operator ack requires stream_id and seq_id")
            segment_id = value.get("segment_id")
            if segment_id is not None:
                segment_id = require_int(segment_id, "remote operator ack segment_id", 0, self.protocol["segment_count_max"] - 1)
            return "ack", {**base, "segment_id": segment_id}
        if event_type == "ping":
            require_keys(value, {"type", "client_id"}, {"stream_id", "seq_id", "cursor"}, "remote operator ping")
            return "ping", base
        if event_type == "client_closed":
            require_keys(value, {"type", "client_id"}, {"stream_id", "seq_id", "cursor"}, "remote operator client_closed")
            self.assemblies = {key: item for key, item in self.assemblies.items() if key[0] != client_id}
            self.completed = {key: item for key, item in self.completed.items() if key[0] != client_id}
            return "closed", base
        raise ContractError(f"remote operator client envelope type {event_type!r} is unsupported")


def inbound_artifact(event):
    event = require_object(event, "remote operator inbound event")
    message = validate_artifact_message(event["message"], "remote operator inbound message")
    body = json.dumps(message, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return {
        "schema_version": 1,
        "client_id": event["client_id"],
        "stream_id": event.get("stream_id"),
        "seq_id": event.get("seq_id"),
        "cursor": event.get("cursor"),
        "message": message,
        "message_sha256": hashlib.sha256(body).hexdigest(),
        "wire_protocol_version": "3",
    }
