import base64
import hashlib
import hmac
import ipaddress
import json
import math
import os
import queue
import re
import secrets
import socket
import sqlite3
import stat
import sys
import threading
import time
import urllib.parse

from octopus_content_hash import octopus_content_hash

INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.environ["OCTOPUS_INTERFACE_DIR"]
VAR_DIR = os.environ["OCTOPUS_VAR_DIR"]
MOVEMENTS_JSONL = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
BLOBS_DIR = os.environ["OCTOPUS_BLOBS_DIR"]
LEDGER_INDEX_SQLITE = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]
CONFIG_PATH = os.path.join(INTERFACE_DIR, "config.json")
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
PAYLOADS_DIR = os.path.join(VAR_DIR, "payloads")
MEDIA_DIR = os.path.join(VAR_DIR, "media")
EFFECTS_DIR = os.path.join(VAR_DIR, "outbound-effects")
SECRETS_DIR = os.path.join(VAR_DIR, "binding-secrets")
BINDINGS_PATH = os.path.join(VAR_DIR, "bindings.json")
TOPOLOGY_OWNERSHIP_PATH = os.path.join(VAR_DIR, "topology-ownership.json")
IDENTITY_SALT_PATH = os.path.join(VAR_DIR, "identity-salt")
DEDUP_PATH = os.path.join(VAR_DIR, "gateway-dedup.json")
PENDING_EVENTS_PATH = os.path.join(VAR_DIR, "pending-events.json")
THREADS_PATH = os.path.join(VAR_DIR, "participated-threads.json")
PAIRING_PATH = os.path.join(VAR_DIR, "pairing.json")
COMMAND_SYNC_PATH = os.path.join(VAR_DIR, "command-sync.json")
STREAM_STATE_PATH = os.path.join(VAR_DIR, "stream-state.json")
for _path in (VAR_DIR, PAYLOADS_DIR, MEDIA_DIR, EFFECTS_DIR):
    os.makedirs(_path, exist_ok=True)

_STDOUT_LOCK = threading.Lock()
_VERDICT_CONDITION = threading.Condition()
_VERDICTS = {}
_VERDICT_INPUT_ERROR = None
_NEXT_REQUEST_ID = 1
_STATE_LOCK = threading.RLock()
_AUTHORING_LOCK = threading.RLock()
_AUTHORING_FATAL = None
_LEDGER_FALLBACK_OFFSET = 0
_LEDGER_FALLBACK_SIZE = 0
_LEDGER_FALLBACK_IDENTITY = None
_LEDGER_INDEX_WARNING = None
PAIRING_TTL_MS = 10 * 60 * 1000
PAIRING_MAX_PENDING = 1024
PAIRING_MAX_APPROVED = 100000
PAIRING_MAX_STATE_BYTES = 2 * 1024 * 1024
PRIVATE_REQUEST_WORKERS = 8
PRIVATE_REQUEST_QUEUE_LIMIT = 64
PRIVATE_REGISTRATION_LIMIT = 32
PRIVATE_REGISTRATION_MAX_BYTES = 16 * 1024
TOPOLOGY_MAX_BYTES = 16 * 1024 * 1024
TOPOLOGY_OWNERSHIP_MAX_PATHS = 100_000


class ArtifactDeliveryRefused(RuntimeError):
    def __init__(self, verdict):
        self.verdict = verdict
        super().__init__(f"runtime refused discord artifact: {verdict!r}")


def require_object(value, name):
    if not isinstance(value, dict):
        raise ValueError(f"{name} must be a JSON object")
    return value


def require_keys(value, required, optional, name):
    obj = require_object(value, name)
    missing = sorted(set(required) - set(obj))
    unknown = sorted(set(obj) - set(required) - set(optional))
    if missing:
        raise ValueError(f"{name} is missing keys: {', '.join(missing)}")
    if unknown:
        raise ValueError(f"{name} has unknown keys: {', '.join(unknown)}")
    return obj


def require_string(value, name, allow_empty=False):
    if not isinstance(value, str) or value.strip() != value or (not allow_empty and not value):
        raise ValueError(f"{name} must be a{' possibly empty' if allow_empty else ' non-empty'} trimmed string")
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
    result = float(value)
    if minimum is not None and result < minimum:
        raise ValueError(f"{name} must be at least {minimum}")
    if maximum is not None and result > maximum:
        raise ValueError(f"{name} must be at most {maximum}")
    return result


def require_string_list(value, name, allow_empty=False):
    if not isinstance(value, list) or (not allow_empty and not value):
        raise ValueError(f"{name} must be a{' possibly empty' if allow_empty else ' non-empty'} array")
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


def require_snowflake(value, name):
    text = str(value)
    if not re.fullmatch(r"[1-9][0-9]{5,24}", text):
        raise ValueError(f"{name} must be a Discord snowflake")
    return text


def require_registry_identifier(value, name):
    value = require_string(value, name)
    if len(value) > 192 or not re.fullmatch(r"[a-z0-9][a-z0-9._-]*", value):
        raise ValueError(f"{name} must be a registry-safe identifier")
    return value


def require_identifier_prefix(value, name):
    value = require_string(value, name)
    if len(value) > 160 or not re.fullmatch(r"[a-z0-9][a-z0-9._-]*_", value):
        raise ValueError(f"{name} must be a registry-safe identifier prefix")
    return value


def _loopback_host(host):
    host = host.lower()
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def validate_api_base_url(value):
    value = require_string(value, "discord API base URL")
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise ValueError("discord API base URL is malformed") from error
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise ValueError("discord API base URL must use HTTP or HTTPS with a host")
    if parsed.username is not None or parsed.password is not None or "?" in value or "#" in value:
        raise ValueError("discord API base URL must not contain userinfo, query, or fragment")
    if port is not None and not 1 <= port <= 65535:
        raise ValueError("discord API base URL port is invalid")
    if parsed.scheme == "http" and not _loopback_host(parsed.hostname):
        raise ValueError("discord API base URL must use HTTPS outside loopback")
    return value.rstrip("/")


def validate_proxy_url(value):
    value = require_string(value, "discord proxy URL")
    if any(ord(character) <= 32 or ord(character) == 127 for character in value):
        raise ValueError("discord proxy URL must not contain control characters")
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise ValueError("discord proxy URL is malformed") from error
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise ValueError("discord proxy URL must use HTTP or HTTPS with a host")
    if port is not None and not 1 <= port <= 65535:
        raise ValueError("discord proxy URL port is invalid")
    if parsed.path not in ("", "/") or parsed.query or parsed.fragment or "?" in value or "#" in value:
        raise ValueError("discord proxy URL must not contain a path, query, or fragment")
    if parsed.netloc.count("@") > 1:
        raise ValueError("discord proxy URL userinfo is malformed")
    userinfo = parsed.netloc.rsplit("@", 1)[0] if "@" in parsed.netloc else ""
    if userinfo and re.search(r"%(?![0-9A-Fa-f]{2})", userinfo):
        raise ValueError("discord proxy URL userinfo has malformed percent encoding")
    decoded_userinfo = urllib.parse.unquote(userinfo)
    if any(ord(character) <= 32 or ord(character) == 127 for character in decoded_userinfo):
        raise ValueError("discord proxy URL userinfo must not contain control characters")
    if parsed.scheme == "http" and not _loopback_host(parsed.hostname):
        raise ValueError("discord proxy URL must use HTTPS outside loopback")
    return value[:-1] if parsed.path == "/" else value


def resolve_proxy_url(config):
    value = os.environ.get(config["discord_api"]["proxy_env"], "").strip()
    return None if not value else validate_proxy_url(value)


def sanitize_external_error(config, error):
    message = str(error)
    api = config.get("discord_api", {}) if isinstance(config, dict) else {}
    values = []
    for env_key in ("proxy_env", "token_env"):
        env_name = api.get(env_key)
        if isinstance(env_name, str) and env_name:
            secret = os.environ.get(env_name, "")
            if secret:
                values.extend((secret, urllib.parse.unquote(secret)))
    proxy_env = api.get("proxy_env")
    proxy = os.environ.get(proxy_env, "").strip() if isinstance(proxy_env, str) and proxy_env else ""
    if proxy:
        try:
            parsed = urllib.parse.urlsplit(proxy)
            userinfo = parsed.netloc.rsplit("@", 1)[0] if "@" in parsed.netloc else ""
            values.extend((
                userinfo,
                urllib.parse.unquote(userinfo),
                parsed.username or "",
                urllib.parse.unquote(parsed.username or ""),
                parsed.password or "",
                urllib.parse.unquote(parsed.password or ""),
            ))
        except ValueError:
            pass
    for value in sorted({value for value in values if value}, key=len, reverse=True):
        message = message.replace(value, "<redacted>")
        encoded = urllib.parse.quote(value, safe="")
        if encoded != value:
            message = message.replace(encoded, "<redacted>")
    return message


def external_error_observation(error):
    body = str(error).encode("utf-8", "replace")
    observation = {
        "exception": type(error).__name__,
        "error_bytes": len(body),
        "error_sha256": hashlib.sha256(body).hexdigest(),
    }
    status = getattr(error, "status", None)
    if isinstance(status, int) and not isinstance(status, bool):
        observation["status"] = status
    code = getattr(error, "code", None)
    if isinstance(code, int) and not isinstance(code, bool):
        observation["code"] = code
    retry_after = getattr(error, "retry_after", None)
    if isinstance(retry_after, (int, float)) and not isinstance(retry_after, bool) and math.isfinite(retry_after) and retry_after >= 0:
        observation["retry_after_ms"] = int(retry_after * 1000)
    return observation


def _validate_command_option(option, name):
    option = require_keys(
        option,
        {"name", "description", "type", "required"},
        {"choices", "min_value", "max_value", "min_length", "max_length", "channel_types"},
        name,
    )
    option_name = require_string(option["name"], f"{name}.name")
    if not re.fullmatch(r"[a-z0-9_-]{1,32}", option_name):
        raise ValueError(f"{name}.name is invalid")
    description = require_string(option["description"], f"{name}.description")
    if len(description) > 100:
        raise ValueError(f"{name}.description is too long")
    if option["type"] not in ("string", "integer", "boolean", "user", "channel", "role", "number", "attachment", "mentionable"):
        raise ValueError(f"{name}.type is unsupported")
    require_bool(option["required"], f"{name}.required")
    if "choices" in option:
        if option["type"] not in ("string", "integer", "number"):
            raise ValueError(f"{name}.choices is not valid for {option['type']}")
        choices = option["choices"]
        if not isinstance(choices, list) or len(choices) > 25:
            raise ValueError(f"{name}.choices must be an array of at most 25 entries")
        for index, choice in enumerate(choices):
            require_keys(choice, {"name", "value"}, set(), f"{name}.choices[{index}]")
            if len(require_string(choice["name"], f"{name}.choices[{index}].name")) > 100:
                raise ValueError(f"{name}.choices[{index}].name is too long")


def validate_config(config):
    require_keys(
        config,
        {"schema_version", "mode", "dependencies", "ledger", "discord_api", "gateway", "authz", "gating", "media", "outbound", "commands", "components", "voice", "driver_ipc", "inbound_journal", "private_run"},
        {"binding", "gateway_run_template"},
        "discord config",
    )
    if config["schema_version"] != 1:
        raise ValueError("discord config schema_version must be 1")
    if config["mode"] not in ("gateway", "binding"):
        raise ValueError("discord config mode must be gateway or binding")
    dependencies = require_keys(config["dependencies"], {"install_timeout_ms", "keep_versions"}, set(), "discord dependencies")
    require_int(dependencies["install_timeout_ms"], "discord dependencies.install_timeout_ms", 1000, 3600000)
    require_int(dependencies["keep_versions"], "discord dependencies.keep_versions", 1, 16)
    ledger = require_keys(config["ledger"], {"max_row_bytes"}, set(), "discord ledger")
    require_int(ledger["max_row_bytes"], "discord ledger.max_row_bytes", 256, 16777216)
    if config["mode"] == "gateway":
        if "binding" in config:
            raise ValueError("discord gateway config must not declare binding")
        template = require_object(config.get("gateway_run_template"), "discord gateway_run_template")
        for key in ("id", "ee", "worker"):
            require_string(template.get(key), f"discord gateway_run_template.{key}")
    else:
        if "gateway_run_template" in config:
            raise ValueError("discord binding config must not declare gateway_run_template")
        binding = require_keys(
            config.get("binding"),
            {"interface", "channel_id", "thread_id", "gateway_interface", "secret_handle", "host", "port", "run"},
            set(),
            "discord binding",
        )
        if require_string(binding["interface"], "discord binding.interface") != INTERFACE_ID:
            raise ValueError("discord binding.interface must match the interface folder")
        validate_coordinates(binding["channel_id"], binding["thread_id"])
        for key in ("gateway_interface", "run", "host", "secret_handle"):
            require_string(binding[key], f"discord binding.{key}")
        if not _loopback_host(binding["host"]):
            raise ValueError("discord binding.host must be loopback")
        if not os.path.isabs(binding["secret_handle"]):
            raise ValueError("discord binding.secret_handle must be absolute")
        load_secret_handle(binding["secret_handle"])
        require_int(binding["port"], "discord binding.port", 1, 65535)
    api = require_keys(
        config["discord_api"],
        {"base_url_env", "base_url_default", "token_env", "proxy_env", "ready_timeout_ms", "reconnect_backoff_ms", "reconnect_backoff_cap_ms"},
        set(),
        "discord discord_api",
    )
    for key in ("base_url_env", "base_url_default", "token_env", "proxy_env"):
        require_string(api[key], f"discord discord_api.{key}")
    validate_api_base_url(api["base_url_default"])
    for key in ("ready_timeout_ms", "reconnect_backoff_ms", "reconnect_backoff_cap_ms"):
        require_int(api[key], f"discord discord_api.{key}", 1, 600000000)
    gateway = require_keys(config["gateway"], {"message_content_intent", "members_intent", "voice_states_intent", "presences_intent", "status", "activity"}, set(), "discord gateway")
    for key in ("message_content_intent", "members_intent", "voice_states_intent", "presences_intent"):
        require_bool(gateway[key], f"discord gateway.{key}")
    if gateway["status"] not in ("online", "idle", "dnd", "invisible"):
        raise ValueError("discord gateway.status is invalid")
    if gateway["activity"] is not None:
        require_string(gateway["activity"], "discord gateway.activity")
    authz = require_keys(
        config["authz"],
        {"allow_all_users", "allow_all_users_env", "allowed_user_ids", "allowed_user_ids_env", "allowed_role_ids", "allowed_role_ids_env", "allowed_guild_ids", "allowed_guild_ids_env", "allowed_channel_ids", "allowed_channel_ids_env", "denied_channel_ids", "denied_channel_ids_env", "allow_bots", "deny_unmatched", "pairing"},
        set(),
        "discord authz",
    )
    for key in ("allow_all_users", "deny_unmatched"):
        require_bool(authz[key], f"discord authz.{key}")
    for key in ("allow_all_users_env", "allowed_user_ids_env", "allowed_role_ids_env", "allowed_guild_ids_env", "allowed_channel_ids_env", "denied_channel_ids_env"):
        require_string(authz[key], f"discord authz.{key}")
    for key in ("allowed_user_ids", "allowed_role_ids", "allowed_guild_ids", "allowed_channel_ids", "denied_channel_ids"):
        require_string_list(authz[key], f"discord authz.{key}", True)
        for index, value in enumerate(authz[key]):
            require_snowflake(value, f"discord authz.{key}[{index}]")
    if authz["allow_bots"] not in ("none", "mentions", "all"):
        raise ValueError("discord authz.allow_bots is invalid")
    pairing = require_keys(authz["pairing"], {"enabled", "dm_only", "prompt"}, set(), "discord authz.pairing")
    require_bool(pairing["enabled"], "discord authz.pairing.enabled")
    require_bool(pairing["dm_only"], "discord authz.pairing.dm_only")
    require_string(pairing["prompt"], "discord authz.pairing.prompt")
    gating = require_keys(
        config["gating"],
        {"respond_in_dms", "require_mention_in_guilds", "free_response_channel_ids", "free_response_channel_ids_env", "thread_require_mention", "reply_to_bot_counts_as_mention", "auto_thread", "no_thread_channel_ids", "no_thread_channel_ids_env", "auto_thread_archive_minutes", "history_backfill", "history_backfill_limit", "text_batch_delay_ms", "text_batch_split_delay_ms", "text_batch_split_threshold", "max_batch_events", "max_batch_bytes", "batch_lock_stripes"},
        set(),
        "discord gating",
    )
    for key in ("respond_in_dms", "require_mention_in_guilds", "thread_require_mention", "reply_to_bot_counts_as_mention", "auto_thread", "history_backfill"):
        require_bool(gating[key], f"discord gating.{key}")
    for key in ("free_response_channel_ids", "no_thread_channel_ids"):
        require_string_list(gating[key], f"discord gating.{key}", True)
        for index, value in enumerate(gating[key]):
            require_snowflake(value, f"discord gating.{key}[{index}]")
    for key in ("free_response_channel_ids_env", "no_thread_channel_ids_env"):
        require_string(gating[key], f"discord gating.{key}")
    if gating["auto_thread_archive_minutes"] not in (60, 1440, 4320, 10080):
        raise ValueError("discord gating.auto_thread_archive_minutes is invalid")
    for key in ("history_backfill_limit", "text_batch_delay_ms", "text_batch_split_delay_ms", "text_batch_split_threshold"):
        require_int(gating[key], f"discord gating.{key}", 0, 1000000)
    require_int(gating["max_batch_events"], "discord gating.max_batch_events", 1, 10000)
    require_int(gating["max_batch_bytes"], "discord gating.max_batch_bytes", 1, 1073741824)
    require_int(gating["batch_lock_stripes"], "discord gating.batch_lock_stripes", 1, 1024)
    media = require_keys(config["media"], {"max_download_bytes", "max_upload_bytes", "max_attachments_per_message", "allow_any_attachment", "inline_text_max_bytes", "inline_text_extensions"}, set(), "discord media")
    for key in ("max_download_bytes", "max_upload_bytes", "inline_text_max_bytes"):
        require_int(media[key], f"discord media.{key}", 1, 1073741824)
    require_int(media["max_attachments_per_message"], "discord media.max_attachments_per_message", 1, 10)
    require_bool(media["allow_any_attachment"], "discord media.allow_any_attachment")
    require_string_list(media["inline_text_extensions"], "discord media.inline_text_extensions", True)
    outbound = require_keys(config["outbound"], {"max_message_len", "min_edit_interval_ms", "reply_mode", "allowed_mentions", "reply_labels", "stream_labels", "error_labels", "turn_start_labels", "telemetry_labels", "allowed_operations", "reactions", "typing", "rate_limit"}, set(), "discord outbound")
    require_int(outbound["max_message_len"], "discord outbound.max_message_len", 1, 2000)
    require_int(outbound["min_edit_interval_ms"], "discord outbound.min_edit_interval_ms", 0, 60000)
    if outbound["reply_mode"] not in ("off", "first", "all"):
        raise ValueError("discord outbound.reply_mode is invalid")
    mentions = require_keys(outbound["allowed_mentions"], {"everyone", "roles", "users", "replied_user"}, set(), "discord outbound.allowed_mentions")
    for key in mentions:
        require_bool(mentions[key], f"discord outbound.allowed_mentions.{key}")
    all_labels = []
    for key in ("reply_labels", "stream_labels", "error_labels", "turn_start_labels", "telemetry_labels"):
        all_labels.extend(require_string_list(outbound[key], f"discord outbound.{key}", True))
    if len(all_labels) != len(set(all_labels)):
        raise ValueError("discord outbound label sets overlap")
    operations = require_string_list(outbound["allowed_operations"], "discord outbound.allowed_operations", True)
    supported = {"message.create", "message.edit", "message.delete", "reaction.add", "reaction.remove", "reaction.clear", "typing.start", "typing.stop", "thread.create", "thread.edit", "interaction.respond", "voice.join", "voice.leave", "voice.play"}
    if set(operations) - supported:
        raise ValueError("discord outbound.allowed_operations contains unsupported values")
    reactions = require_keys(outbound["reactions"], {"enabled", "in_progress", "success", "failure"}, set(), "discord outbound.reactions")
    require_bool(reactions["enabled"], "discord outbound.reactions.enabled")
    for key in ("in_progress", "success", "failure"):
        require_string(reactions[key], f"discord outbound.reactions.{key}")
    typing = require_keys(outbound["typing"], {"enabled", "refresh_seconds"}, set(), "discord outbound.typing")
    require_bool(typing["enabled"], "discord outbound.typing.enabled")
    require_int(typing["refresh_seconds"], "discord outbound.typing.refresh_seconds", 1, 60)
    rate = require_keys(outbound["rate_limit"], {"max_retry_after_ms"}, set(), "discord outbound.rate_limit")
    require_int(rate["max_retry_after_ms"], "discord outbound.rate_limit.max_retry_after_ms", 1, 600000)
    commands = require_keys(config["commands"], {"sync_policy", "mutation_interval_ms", "max_commands", "commands"}, set(), "discord commands")
    if commands["sync_policy"] not in ("safe", "bulk", "off"):
        raise ValueError("discord commands.sync_policy is invalid")
    require_int(commands["mutation_interval_ms"], "discord commands.mutation_interval_ms", 0, 60000)
    require_int(commands["max_commands"], "discord commands.max_commands", 0, 100)
    if not isinstance(commands["commands"], list) or len(commands["commands"]) > commands["max_commands"]:
        raise ValueError("discord commands.commands exceeds max_commands")
    names = set()
    for index, command in enumerate(commands["commands"]):
        name = f"discord commands.commands[{index}]"
        command = require_keys(command, {"name", "description", "dm_permission", "default_member_permissions", "options"}, set(), name)
        command_name = require_string(command["name"], f"{name}.name")
        if not re.fullmatch(r"[a-z0-9_-]{1,32}", command_name) or command_name in names:
            raise ValueError(f"{name}.name is invalid or repeated")
        names.add(command_name)
        if len(require_string(command["description"], f"{name}.description")) > 100:
            raise ValueError(f"{name}.description is too long")
        require_bool(command["dm_permission"], f"{name}.dm_permission")
        if command["default_member_permissions"] is not None:
            require_string(str(command["default_member_permissions"]), f"{name}.default_member_permissions")
        if not isinstance(command["options"], list) or len(command["options"]) > 25:
            raise ValueError(f"{name}.options must be an array of at most 25 entries")
        for option_index, option in enumerate(command["options"]):
            _validate_command_option(option, f"{name}.options[{option_index}]")
    components = require_keys(config["components"], {"max_rows", "max_items_per_row", "max_custom_id_len", "pending_ttl_seconds"}, set(), "discord components")
    require_int(components["max_rows"], "discord components.max_rows", 1, 5)
    require_int(components["max_items_per_row"], "discord components.max_items_per_row", 1, 5)
    require_int(components["max_custom_id_len"], "discord components.max_custom_id_len", 1, 100)
    require_int(components["pending_ttl_seconds"], "discord components.pending_ttl_seconds", 1, 86400)
    voice = require_keys(config["voice"], {"enabled", "require_opus", "silence_seconds", "minimum_speech_seconds", "maximum_utterance_seconds", "inactivity_seconds", "playback_timeout_seconds", "max_decoded_pcm_bytes", "max_buffered_pcm_bytes", "max_active_speakers", "ambient"}, set(), "discord voice")
    require_bool(voice["enabled"], "discord voice.enabled")
    require_bool(voice["require_opus"], "discord voice.require_opus")
    for key in ("silence_seconds", "minimum_speech_seconds", "maximum_utterance_seconds"):
        require_number(voice[key], f"discord voice.{key}", 0.05, 3600)
    for key in ("inactivity_seconds", "playback_timeout_seconds"):
        require_int(voice[key], f"discord voice.{key}", 1, 86400)
    require_int(voice["max_decoded_pcm_bytes"], "discord voice.max_decoded_pcm_bytes", 3840, 1073741824)
    require_int(voice["max_buffered_pcm_bytes"], "discord voice.max_buffered_pcm_bytes", voice["max_decoded_pcm_bytes"], 2147483648)
    require_int(voice["max_active_speakers"], "discord voice.max_active_speakers", 1, 1024)
    ambient = require_keys(voice["ambient"], {"enabled", "blob_id", "gain", "duck_gain", "speech_gain", "duck_release_ms"}, set(), "discord voice.ambient")
    require_bool(ambient["enabled"], "discord voice.ambient.enabled")
    if ambient["blob_id"] is not None:
        require_string(ambient["blob_id"], "discord voice.ambient.blob_id")
    for key in ("gain", "duck_gain", "speech_gain"):
        require_number(ambient[key], f"discord voice.ambient.{key}", 0, 4)
    require_int(ambient["duck_release_ms"], "discord voice.ambient.duck_release_ms", 0, 10000)
    ipc = require_keys(config["driver_ipc"], {"listen", "port", "state_file", "connect_timeout_ms", "request_timeout_ms", "reconnect_backoff_ms", "reconnect_backoff_cap_ms", "max_manifest_bytes", "max_frame_bytes"}, set(), "discord driver_ipc")
    host = require_string(ipc["listen"], "discord driver_ipc.listen")
    try:
        if not ipaddress.ip_address(host).is_loopback:
            raise ValueError("discord driver_ipc.listen must be loopback")
    except ValueError as error:
        if str(error).startswith("discord "):
            raise
        raise ValueError("discord driver_ipc.listen must be loopback") from error
    require_string(ipc["state_file"], "discord driver_ipc.state_file")
    require_int(ipc["port"], "discord driver_ipc.port", 0, 65535)
    for key in ("connect_timeout_ms", "request_timeout_ms", "reconnect_backoff_ms", "reconnect_backoff_cap_ms", "max_manifest_bytes", "max_frame_bytes"):
        require_int(ipc[key], f"discord driver_ipc.{key}", 1, 600000000)
    journal = require_keys(config["inbound_journal"], {"max_entries", "max_bytes", "delivered_retention_seconds", "attempt_history_limit", "delivery_max_attempts", "delivery_retry_backoff_ms", "delivery_retry_backoff_cap_ms", "private_admitted_limit", "max_error_chars"}, set(), "discord inbound_journal")
    require_int(journal["max_entries"], "discord inbound_journal.max_entries", 1, 1000000)
    require_int(journal["max_bytes"], "discord inbound_journal.max_bytes", 4096, 1073741824)
    require_int(journal["delivered_retention_seconds"], "discord inbound_journal.delivered_retention_seconds", 1, 31536000)
    require_int(journal["attempt_history_limit"], "discord inbound_journal.attempt_history_limit", 1, 1024)
    require_int(journal["delivery_max_attempts"], "discord inbound_journal.delivery_max_attempts", 1, 100)
    require_int(journal["delivery_retry_backoff_ms"], "discord inbound_journal.delivery_retry_backoff_ms", 1, 600000)
    require_int(journal["delivery_retry_backoff_cap_ms"], "discord inbound_journal.delivery_retry_backoff_cap_ms", journal["delivery_retry_backoff_ms"], 600000)
    require_int(journal["private_admitted_limit"], "discord inbound_journal.private_admitted_limit", 1, 1000000)
    require_int(journal["max_error_chars"], "discord inbound_journal.max_error_chars", 64, 1048576)
    private = require_keys(config["private_run"], {"ee_template", "parent_ee", "model", "reasoning_effort", "role_artifacts", "owners", "id_prefixes", "file_write_tool", "file_write_label", "file_write_result_label", "worker_environment_kind", "worker_environment_door_owner", "gateway_worker_environment_interface", "gateway_file_write_tool", "file_write_mark_outcome", "provider_context_output_labels", "authoring_max_attempts", "result_timeout_ms"}, set(), "discord private_run")
    for key in ("ee_template", "parent_ee", "model", "reasoning_effort", "file_write_tool", "file_write_label", "file_write_result_label", "worker_environment_kind", "worker_environment_door_owner", "gateway_worker_environment_interface", "gateway_file_write_tool", "file_write_mark_outcome"):
        require_string(private[key], f"discord private_run.{key}")
    require_string_list(private["role_artifacts"], "discord private_run.role_artifacts", True)
    require_string_list(private["provider_context_output_labels"], "discord private_run.provider_context_output_labels")
    owner_keys = {"discord", "provider", "interface_authoring", "worker"}
    owners = require_keys(private["owners"], owner_keys, set(), "discord private_run.owners")
    prefixes = require_keys(private["id_prefixes"], owner_keys | {"run", "ee", "ee_template", "worker_environment", "file_write_tool"}, set(), "discord private_run.id_prefixes")
    for key in owner_keys:
        require_registry_identifier(owners[key], f"discord private_run.owners.{key}")
        require_identifier_prefix(prefixes[key], f"discord private_run.id_prefixes.{key}")
    for key in ("run", "ee", "ee_template", "worker_environment", "file_write_tool"):
        require_identifier_prefix(prefixes[key], f"discord private_run.id_prefixes.{key}")
    require_int(private["authoring_max_attempts"], "discord private_run.authoring_max_attempts", 1, 32)
    require_int(private["result_timeout_ms"], "discord private_run.result_timeout_ms", 1, 600000)


def load_config():
    config = json.loads(
        read_bounded_regular(CONFIG_PATH, TOPOLOGY_MAX_BYTES, "interface config")
    )
    validate_config(config)
    return config


def resolve_token(config):
    env_name = config["discord_api"]["token_env"]
    token = os.environ.get(env_name, "").strip()
    if not token:
        raise RuntimeError(f"{env_name} is empty; config.json names only the secret handle")
    return token


def resolve_base_url(config):
    api = config["discord_api"]
    return validate_api_base_url(os.environ.get(api["base_url_env"], "").strip() or api["base_url_default"])


def env_flag(name):
    return bool(name) and os.environ.get(name, "").strip().lower() in ("1", "true", "yes", "on")


def env_snowflakes(name):
    if not name:
        return set()
    raw = os.environ.get(name, "").strip()
    if not raw:
        return set()
    result = set()
    for value in raw.replace(",", " ").split():
        value = value.strip()
        if value.startswith("<@") and value.endswith(">"):
            value = value.lstrip("<@!&").rstrip(">")
        result.add(require_snowflake(value, name))
    return result


def atomic_write_bytes(path, payload):
    parent = os.path.dirname(path)
    os.makedirs(parent, exist_ok=True)
    temporary = os.path.join(parent, f".{os.path.basename(path)}.{os.getpid()}.{time.time_ns()}.tmp")
    try:
        descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        directory = os.open(parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def atomic_write_json(path, value, indent=None):
    atomic_write_bytes(path, json.dumps(value, ensure_ascii=False, indent=indent).encode("utf-8"))


def read_json(path, default=None):
    try:
        with open(path, "r", encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError:
        return default


def read_bounded_json(path, maximum, name, default=None):
    try:
        payload = read_bounded_regular(path, maximum, name)
    except FileNotFoundError:
        return default
    try:
        return json.loads(payload)
    except json.JSONDecodeError as error:
        raise ValueError(f"discord {name} is malformed JSON") from error


def _read_runtime_verdicts():
    global _VERDICT_INPUT_ERROR
    try:
        for line in sys.stdin:
            message = json.loads(line)
            if message.get("kind") != "delivery_verdict":
                raise RuntimeError(f"unexpected runtime input: {message!r}")
            request_id = message.get("request_id")
            if not isinstance(request_id, int):
                raise RuntimeError("runtime verdict has no integer request_id")
            with _VERDICT_CONDITION:
                _VERDICTS[request_id] = message.get("verdict") or {}
                _VERDICT_CONDITION.notify_all()
        raise RuntimeError("runtime verdict input closed")
    except Exception as error:
        with _VERDICT_CONDITION:
            _VERDICT_INPUT_ERROR = error
            _VERDICT_CONDITION.notify_all()


def start_runtime_verdict_reader():
    threading.Thread(target=_read_runtime_verdicts, daemon=True).start()


def signal_ready():
    with _STDOUT_LOCK:
        sys.stdout.write(json.dumps({"kind": "ready"}, separators=(",", ":")) + "\n")
        sys.stdout.flush()


def hand_artifact(manifest_path, evidence=None):
    global _NEXT_REQUEST_ID
    with _VERDICT_CONDITION:
        request_id = _NEXT_REQUEST_ID
        _NEXT_REQUEST_ID += 1
    with _STDOUT_LOCK:
        sys.stdout.write(json.dumps({"kind": "arrival", "request_id": request_id, "artifact_manifest_path": manifest_path, "evidence": evidence or []}) + "\n")
        sys.stdout.flush()
    with _VERDICT_CONDITION:
        while request_id not in _VERDICTS and _VERDICT_INPUT_ERROR is None:
            _VERDICT_CONDITION.wait()
        if _VERDICT_INPUT_ERROR is not None:
            raise RuntimeError(f"runtime verdict input failed before request {request_id}: {_VERDICT_INPUT_ERROR}")
        verdict = _VERDICTS.pop(request_id)
    admitted = verdict.get("admitted")
    if isinstance(admitted, dict) and isinstance(admitted.get("seq"), int):
        return admitted["seq"]
    raise ArtifactDeliveryRefused(verdict)


def materialize_artifact(label, payload, key):
    payload_path = os.path.join(PAYLOADS_DIR, f"{key}.payload")
    manifest_path = os.path.join(PAYLOADS_DIR, f"{key}.artifact.json")
    atomic_write_bytes(payload_path, payload)
    atomic_write_json(manifest_path, {"label": label, "payload_path": payload_path})
    return manifest_path


def hand_json(label, payload, key):
    return hand_artifact(materialize_artifact(label, json.dumps(payload, ensure_ascii=False, sort_keys=True).encode("utf-8"), key))


def materialized_artifact_id(manifest_path, manifest_maximum, payload_maximum):
    manifest_bytes = read_bounded_regular(
        manifest_path,
        manifest_maximum,
        "materialized artifact manifest",
    )
    manifest = require_keys(json.loads(manifest_bytes), {"label", "payload_path"}, {"payload_content_hash"}, "discord materialized artifact manifest")
    label = require_string(manifest["label"], "discord materialized artifact label")
    payload_path = require_string(manifest["payload_path"], "discord materialized artifact payload_path")
    if not os.path.isabs(payload_path):
        raise ValueError("discord materialized artifact payload_path must be absolute")
    payload_root = os.path.realpath(PAYLOADS_DIR)
    resolved_payload = os.path.realpath(payload_path)
    if os.path.commonpath((resolved_payload, payload_root)) != payload_root:
        raise ValueError("discord materialized artifact payload_path escapes its private root")
    payload = read_bounded_regular(
        payload_path,
        payload_maximum,
        "materialized artifact payload",
    )
    payload_hash = octopus_content_hash(payload)
    declared = manifest.get("payload_content_hash")
    if declared is not None and declared != payload_hash:
        raise ValueError("discord materialized artifact payload hash mismatch")
    pinned = {"label": label, "payload_path": payload_path, "payload_content_hash": payload_hash}
    pinned_bytes = json.dumps(pinned, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"{label}:{octopus_content_hash(pinned_bytes)}"


def find_admitted_arrival_seq(artifact_id, max_row_bytes):
    artifact_id = require_string(artifact_id, "discord arrival artifact id")
    max_row_bytes = require_int(max_row_bytes, "discord ledger.max_row_bytes", 256, 16777216)
    try:
        uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
        connection = sqlite3.connect(uri, uri=True, timeout=0.25)
        try:
            if connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone() != ("3",):
                raise RuntimeError("discord ledger index has unsupported schema")
            row = connection.execute(
                "SELECT seq FROM movements WHERE kind = 'InterfaceArrival' AND from_kind = 'interface' AND from_id = ? AND artifact_id = ? ORDER BY seq ASC LIMIT 1",
                (INTERFACE_ID, artifact_id),
            ).fetchone()
        finally:
            connection.close()
        if row is not None:
            return require_int(row[0], "discord admitted arrival seq", 0)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError):
        pass
    try:
        before = os.lstat(MOVEMENTS_JSONL)
        descriptor = os.open(MOVEMENTS_JSONL, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    except FileNotFoundError:
        return None
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(MOVEMENTS_JSONL)
        if (
            stat.S_ISLNK(before.st_mode)
            or not stat.S_ISREG(before.st_mode)
            or not stat.S_ISREG(opened.st_mode)
            or stat.S_ISLNK(linked.st_mode)
            or (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino)
            or (linked.st_dev, linked.st_ino) != (opened.st_dev, opened.st_ino)
        ):
            raise RuntimeError("discord movement ledger changed while opening")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            while True:
                row_offset = handle.tell()
                line = handle.readline(max_row_bytes + 1)
                if not line:
                    break
                if len(line) > max_row_bytes:
                    raise RuntimeError(f"discord movement lookup row at byte {row_offset} exceeds {max_row_bytes} bytes")
                if not line.endswith(b"\n"):
                    break
                movement = require_object(json.loads(line), "discord movement lookup row")
                if movement.get("kind") != "InterfaceArrival" or movement.get("from") != {"Interface": INTERFACE_ID}:
                    continue
                artifact = movement.get("artifact")
                if isinstance(artifact, dict) and artifact.get("id") == artifact_id:
                    return require_int(movement.get("seq"), "discord admitted arrival seq", 0)
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    return None


def hand_json_idempotent(label, payload, key):
    config = load_config()
    manifest_path = materialize_artifact(label, _canonical_json_bytes(payload), key)
    artifact_id = materialized_artifact_id(
        manifest_path,
        config["driver_ipc"]["max_manifest_bytes"],
        config["driver_ipc"]["max_frame_bytes"],
    )
    evidence = [{"interface_id": INTERFACE_ID, "artifact_id": artifact_id, "label": "discord.arrival.idempotency", "bytes": list(hashlib.sha256(artifact_id.encode("utf-8")).digest())}]
    try:
        return hand_artifact(manifest_path, evidence)
    except ArtifactDeliveryRefused as error:
        refused = error.verdict.get("refused") if isinstance(error.verdict, dict) else None
        details = refused.get("detail") if isinstance(refused, dict) else None
        if isinstance(details, dict) and details.get("artifact_id") == artifact_id and details.get("message") == "arrival evidence already exists in the movement ledger":
            admitted = find_admitted_arrival_seq(artifact_id, config["ledger"]["max_row_bytes"])
            if admitted is not None:
                return admitted
        raise


def hand_failure(source_seq, failure_class, details, key):
    return hand_json(f"{INTERFACE_ID}.failure", {"source_movement_seq": source_seq, "failure_class": failure_class, "details": details}, key)


def load_bounded_ids(path, name, maximum):
    state = read_json(path, {"schema_version": 1, "ids": []})
    state = require_keys(state, {"schema_version", "ids"}, set(), name)
    if state["schema_version"] != 1:
        raise ValueError(f"{name} schema_version must be 1")
    ids = require_string_list(state["ids"], f"{name}.ids", True)
    if len(ids) > maximum:
        raise ValueError(f"{name}.ids exceeds its durable bound")
    return dict.fromkeys(ids)


def remember_bounded_id(path, name, identifier, maximum):
    identifier = require_string(str(identifier), f"{name} id")
    with _STATE_LOCK:
        ids = load_bounded_ids(path, name, maximum)
        existed = identifier in ids
        ids[identifier] = None
        values = list(ids)[-maximum:]
        atomic_write_json(path, {"schema_version": 1, "ids": values})
        return existed


def is_duplicate_event(identifier):
    return remember_bounded_id(DEDUP_PATH, "discord gateway dedup", str(identifier), 10000)


def _canonical_json_bytes(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")


def _pending_event_key(event_id):
    return hashlib.sha256(require_string(str(event_id), "discord pending event id").encode("utf-8")).hexdigest()


def _validate_pending_attempt(value, name):
    value = require_keys(value, {"number", "started_at_unix_ms", "outcome", "evidence"}, {"finished_at_unix_ms", "error", "retry_at_unix_ms", "admitted_seq"}, name)
    require_int(value["number"], f"{name}.number", 1)
    require_int(value["started_at_unix_ms"], f"{name}.started_at_unix_ms", 0)
    if value["outcome"] not in ("started", "interrupted", "failed", "delivered"):
        raise ValueError(f"{name}.outcome is invalid")
    if not isinstance(value["evidence"], list):
        raise ValueError(f"{name}.evidence must be an array")
    for index, evidence in enumerate(value["evidence"]):
        evidence = require_keys(evidence, {"phase", "outcome", "recorded_at_unix_ms"}, {"details"}, f"{name}.evidence[{index}]")
        require_string(evidence["phase"], f"{name}.evidence[{index}].phase")
        require_string(evidence["outcome"], f"{name}.evidence[{index}].outcome")
        require_int(evidence["recorded_at_unix_ms"], f"{name}.evidence[{index}].recorded_at_unix_ms", 0)
        if "details" in evidence:
            require_object(evidence["details"], f"{name}.evidence[{index}].details")
    if value["outcome"] != "started":
        require_int(value.get("finished_at_unix_ms"), f"{name}.finished_at_unix_ms", value["started_at_unix_ms"])
    if value["outcome"] == "failed":
        require_string(value.get("error"), f"{name}.error")
        require_int(value.get("retry_at_unix_ms"), f"{name}.retry_at_unix_ms", value["started_at_unix_ms"])
    if value["outcome"] == "delivered":
        require_int(value.get("admitted_seq"), f"{name}.admitted_seq", 0)
    return value


def _validate_pending_event(key, value, config):
    value = require_keys(
        value,
        {"event_id", "source_event_ids", "channel_id", "thread_id", "payload", "payload_sha256", "status", "created_at_unix_ms", "updated_at_unix_ms", "attempts"},
        {"binding_interface", "admitted_seq", "last_error", "next_retry_at_unix_ms"},
        f"discord pending event {key}",
    )
    event_id = require_string(value["event_id"], f"discord pending event {key}.event_id")
    if _pending_event_key(event_id) != key:
        raise ValueError(f"discord pending event {key} identity is invalid")
    source_ids = require_string_list(value["source_event_ids"], f"discord pending event {key}.source_event_ids")
    validate_coordinates(value["channel_id"], value["thread_id"])
    payload = require_object(value["payload"], f"discord pending event {key}.payload")
    digest = require_string(value["payload_sha256"], f"discord pending event {key}.payload_sha256")
    if not re.fullmatch(r"[0-9a-f]{64}", digest) or hashlib.sha256(_canonical_json_bytes(payload)).hexdigest() != digest:
        raise ValueError(f"discord pending event {key} payload hash is invalid")
    if value["status"] not in ("pending", "delivering", "delivered"):
        raise ValueError(f"discord pending event {key}.status is invalid")
    created = require_int(value["created_at_unix_ms"], f"discord pending event {key}.created_at_unix_ms", 0)
    require_int(value["updated_at_unix_ms"], f"discord pending event {key}.updated_at_unix_ms", created)
    if not isinstance(value["attempts"], list) or len(value["attempts"]) > config["inbound_journal"]["attempt_history_limit"]:
        raise ValueError(f"discord pending event {key}.attempts exceeds its bound")
    previous = 0
    for index, attempt in enumerate(value["attempts"]):
        attempt = _validate_pending_attempt(attempt, f"discord pending event {key}.attempts[{index}]")
        if attempt["number"] <= previous:
            raise ValueError(f"discord pending event {key}.attempts are not monotonic")
        previous = attempt["number"]
    if value["status"] == "delivering" and (not value["attempts"] or value["attempts"][-1]["outcome"] != "started"):
        raise ValueError(f"discord pending event {key} has no live delivery attempt")
    if value["status"] == "delivered":
        require_string(value.get("binding_interface"), f"discord pending event {key}.binding_interface")
        require_int(value.get("admitted_seq"), f"discord pending event {key}.admitted_seq", 0)
    if "last_error" in value and value["last_error"] is not None:
        require_string(value["last_error"], f"discord pending event {key}.last_error")
    if "next_retry_at_unix_ms" in value and value["next_retry_at_unix_ms"] is not None:
        require_int(value["next_retry_at_unix_ms"], f"discord pending event {key}.next_retry_at_unix_ms", 0)
    if payload.get("event_id") != event_id or payload.get("source_event_ids") != source_ids:
        raise ValueError(f"discord pending event {key} payload identity disagrees with its journal envelope")
    return value


def load_pending_events(config):
    state = read_json(PENDING_EVENTS_PATH, {"schema_version": 1, "events": {}})
    state = require_keys(state, {"schema_version", "events"}, set(), "discord pending events")
    if state["schema_version"] != 1:
        raise ValueError("discord pending events schema_version must be 1")
    events = require_object(state["events"], "discord pending events.events")
    if len(events) > config["inbound_journal"]["max_entries"]:
        raise ValueError("discord pending events exceeds max_entries")
    for key, value in events.items():
        if not re.fullmatch(r"[0-9a-f]{64}", require_string(key, "discord pending event key")):
            raise ValueError("discord pending event key must be a SHA-256 digest")
        _validate_pending_event(key, value, config)
    encoded = _canonical_json_bytes(state)
    if len(encoded) > config["inbound_journal"]["max_bytes"]:
        raise ValueError("discord pending events exceeds max_bytes")
    return state


def _prune_delivered_pending_events(config, state, now_ms, reserve):
    journal = config["inbound_journal"]
    cutoff = now_ms - journal["delivered_retention_seconds"] * 1000
    events = state["events"]
    for key in [key for key, value in events.items() if value["status"] == "delivered" and value["updated_at_unix_ms"] < cutoff]:
        events.pop(key)
    delivered = sorted(
        ((value["updated_at_unix_ms"], key) for key, value in events.items() if value["status"] == "delivered"),
        key=lambda item: (item[0], item[1]),
    )
    while len(events) + reserve > journal["max_entries"] and delivered:
        _, key = delivered.pop(0)
        events.pop(key, None)
    if len(events) + reserve > journal["max_entries"]:
        raise RuntimeError("discord pending journal is full of undelivered events")


def _commit_pending_events(config, state):
    encoded = _canonical_json_bytes(state)
    if len(encoded) > config["inbound_journal"]["max_bytes"]:
        raise RuntimeError("discord pending journal cannot retain another event within max_bytes")
    atomic_write_bytes(PENDING_EVENTS_PATH, encoded)


def _fit_pending_event_bytes(config, state):
    maximum = config["inbound_journal"]["max_bytes"]
    delivered = sorted(
        ((value["updated_at_unix_ms"], key) for key, value in state["events"].items() if value["status"] == "delivered"),
        key=lambda item: (item[0], item[1]),
    )
    while len(_canonical_json_bytes(state)) > maximum and delivered:
        _, key = delivered.pop(0)
        state["events"].pop(key, None)
    if len(_canonical_json_bytes(state)) > maximum:
        raise RuntimeError("discord pending journal is full of undelivered event bytes")


def enqueue_pending_event(config, channel_id, thread_id, payload, event_id, source_event_ids):
    event_id = require_string(str(event_id), "discord pending event id")
    source_event_ids = require_string_list([str(value) for value in source_event_ids], "discord pending source event ids")
    channel_id = require_snowflake(channel_id, "discord pending channel_id")
    thread_id = None if thread_id is None else require_snowflake(thread_id, "discord pending thread_id")
    payload = json.loads(_canonical_json_bytes(payload))
    payload["event_id"] = event_id
    payload["source_event_ids"] = source_event_ids
    payload_hash = hashlib.sha256(_canonical_json_bytes(payload)).hexdigest()
    key = _pending_event_key(event_id)
    now_ms = int(time.time() * 1000)
    with _STATE_LOCK:
        state = load_pending_events(config)
        existing = state["events"].get(key)
        if existing is not None:
            if existing["payload_sha256"] != payload_hash or existing["channel_id"] != channel_id or existing["thread_id"] != thread_id or existing["source_event_ids"] != source_event_ids:
                raise RuntimeError(f"discord pending event {event_id!r} was reused with different evidence")
            return json.loads(_canonical_json_bytes(existing)), False
        _prune_delivered_pending_events(config, state, now_ms, 1)
        entry = {
            "event_id": event_id,
            "source_event_ids": source_event_ids,
            "channel_id": channel_id,
            "thread_id": thread_id,
            "payload": payload,
            "payload_sha256": payload_hash,
            "status": "pending",
            "created_at_unix_ms": now_ms,
            "updated_at_unix_ms": now_ms,
            "attempts": [],
        }
        state["events"][key] = entry
        _fit_pending_event_bytes(config, state)
        _commit_pending_events(config, state)
        return json.loads(_canonical_json_bytes(entry)), True


def pending_events_for_replay(config):
    with _STATE_LOCK:
        state = load_pending_events(config)
        before = _canonical_json_bytes(state)
        _prune_delivered_pending_events(config, state, int(time.time() * 1000), 0)
        _fit_pending_event_bytes(config, state)
        if _canonical_json_bytes(state) != before:
            _commit_pending_events(config, state)
        values = list(state["events"].values())
    return sorted((json.loads(_canonical_json_bytes(value)) for value in values), key=lambda value: (value["created_at_unix_ms"], value["event_id"]))


def begin_pending_attempt(config, event_id):
    key = _pending_event_key(event_id)
    now_ms = int(time.time() * 1000)
    with _STATE_LOCK:
        state = load_pending_events(config)
        entry = state["events"].get(key)
        if entry is None:
            raise RuntimeError(f"discord pending event {event_id!r} is absent")
        if entry["status"] == "delivered":
            return json.loads(_canonical_json_bytes(entry))
        if entry["status"] == "delivering":
            previous = entry["attempts"][-1]
            previous["outcome"] = "interrupted"
            previous["finished_at_unix_ms"] = now_ms
        number = entry["attempts"][-1]["number"] + 1 if entry["attempts"] else 1
        entry["attempts"].append({"number": number, "started_at_unix_ms": now_ms, "outcome": "started", "evidence": []})
        entry["attempts"] = entry["attempts"][-config["inbound_journal"]["attempt_history_limit"]:]
        entry["status"] = "delivering"
        entry["updated_at_unix_ms"] = now_ms
        entry.pop("next_retry_at_unix_ms", None)
        _commit_pending_events(config, state)
        return json.loads(_canonical_json_bytes(entry))


def append_pending_attempt_evidence(config, event_id, phase, outcome, details=None):
    key = _pending_event_key(event_id)
    now_ms = int(time.time() * 1000)
    with _STATE_LOCK:
        state = load_pending_events(config)
        entry = state["events"].get(key)
        if entry is None or entry["status"] != "delivering" or not entry["attempts"]:
            raise RuntimeError(f"discord pending event {event_id!r} has no live attempt")
        evidence = {"phase": require_string(phase, "discord pending evidence phase"), "outcome": require_string(outcome, "discord pending evidence outcome"), "recorded_at_unix_ms": now_ms}
        if details is not None:
            evidence["details"] = require_object(details, "discord pending evidence details")
        entry["attempts"][-1]["evidence"].append(evidence)
        entry["attempts"][-1]["evidence"] = entry["attempts"][-1]["evidence"][-config["inbound_journal"]["attempt_history_limit"]:]
        entry["updated_at_unix_ms"] = now_ms
        _commit_pending_events(config, state)


def fail_pending_attempt(config, event_id, error, retry_after_ms):
    key = _pending_event_key(event_id)
    now_ms = int(time.time() * 1000)
    message = sanitize_external_error(config, error)[:config["inbound_journal"]["max_error_chars"]]
    retry_at = now_ms + require_int(retry_after_ms, "discord pending retry_after_ms", 1)
    with _STATE_LOCK:
        state = load_pending_events(config)
        entry = state["events"].get(key)
        if entry is None or entry["status"] != "delivering" or not entry["attempts"]:
            raise RuntimeError(f"discord pending event {event_id!r} has no live attempt")
        attempt = entry["attempts"][-1]
        attempt.update({"outcome": "failed", "finished_at_unix_ms": now_ms, "error": message, "retry_at_unix_ms": retry_at})
        entry["status"] = "pending"
        entry["updated_at_unix_ms"] = now_ms
        entry["last_error"] = message
        entry["next_retry_at_unix_ms"] = retry_at
        _commit_pending_events(config, state)
        return json.loads(_canonical_json_bytes(entry))


def complete_pending_event(config, event_id, binding_interface, admitted_seq):
    key = _pending_event_key(event_id)
    now_ms = int(time.time() * 1000)
    with _STATE_LOCK:
        state = load_pending_events(config)
        entry = state["events"].get(key)
        if entry is None or entry["status"] != "delivering" or not entry["attempts"]:
            raise RuntimeError(f"discord pending event {event_id!r} has no live attempt")
        attempt = entry["attempts"][-1]
        attempt.update({"outcome": "delivered", "finished_at_unix_ms": now_ms, "admitted_seq": require_int(admitted_seq, "discord pending admitted_seq", 0)})
        entry["status"] = "delivered"
        entry["updated_at_unix_ms"] = now_ms
        entry["binding_interface"] = require_string(binding_interface, "discord pending binding interface")
        entry["admitted_seq"] = admitted_seq
        entry.pop("last_error", None)
        entry.pop("next_retry_at_unix_ms", None)
        _prune_delivered_pending_events(config, state, now_ms, 0)
        _commit_pending_events(config, state)
        return json.loads(_canonical_json_bytes(entry))


def thread_participated(identifier):
    return str(identifier) in load_bounded_ids(THREADS_PATH, "discord participated threads", 1000)


def mark_thread_participated(identifier):
    remember_bounded_id(THREADS_PATH, "discord participated threads", str(identifier), 1000)


def load_pairing():
    state = read_bounded_json(
        PAIRING_PATH,
        PAIRING_MAX_STATE_BYTES,
        "pairing state",
        {"schema_version": 2, "approved": [], "pending": {}},
    )
    state = require_keys(state, {"schema_version", "approved", "pending"}, set(), "discord pairing")
    if state["schema_version"] != 2:
        raise ValueError("discord pairing schema_version must be 2")
    approved = require_string_list(state["approved"], "discord pairing.approved", True)
    if len(approved) > PAIRING_MAX_APPROVED:
        raise ValueError("discord pairing approved exceeds its finite bound")
    for index, user_id in enumerate(approved):
        require_snowflake(user_id, f"discord pairing.approved[{index}]")
    pending = require_object(state["pending"], "discord pairing.pending")
    if len(pending) > PAIRING_MAX_PENDING:
        raise ValueError("discord pairing pending exceeds its finite bound")
    for code, entry in pending.items():
        if not re.fullmatch(r"DC-[0-9]{6}", code):
            raise ValueError("discord pairing contains a malformed code")
        entry = require_keys(
            entry,
            {"user_id", "created_at_unix_ms"},
            set(),
            f"discord pairing.pending.{code}",
        )
        require_snowflake(entry["user_id"], f"discord pairing.pending.{code}.user_id")
        require_int(entry["created_at_unix_ms"], f"discord pairing.pending.{code}.created_at_unix_ms", 0)
    return state


def prune_pairing(state, now_ms):
    pending = state["pending"]
    expired = [
        code
        for code, entry in pending.items()
        if entry["created_at_unix_ms"] + PAIRING_TTL_MS <= now_ms
    ]
    for code in expired:
        pending.pop(code)
    return bool(expired)


def commit_pairing(state):
    payload = json.dumps(state, ensure_ascii=False, sort_keys=True).encode("utf-8")
    if len(payload) > PAIRING_MAX_STATE_BYTES:
        raise RuntimeError("discord pairing state exceeds its finite byte bound")
    atomic_write_bytes(PAIRING_PATH, payload)


def register_pairing(user_id, now_ms=None):
    user_id = require_snowflake(user_id, "discord pairing user")
    now_ms = int(time.time() * 1000) if now_ms is None else require_int(now_ms, "discord pairing time", 0)
    with _STATE_LOCK:
        state = load_pairing()
        changed = prune_pairing(state, now_ms)
        for code, entry in state["pending"].items():
            if entry["user_id"] == user_id:
                if changed:
                    commit_pairing(state)
                return code
        if len(state["pending"]) >= PAIRING_MAX_PENDING:
            if changed:
                commit_pairing(state)
            raise RuntimeError("discord pairing pending capacity is exhausted")
        for _ in range(100):
            code = f"DC-{secrets.randbelow(1000000):06d}"
            if code not in state["pending"]:
                state["pending"][code] = {"user_id": user_id, "created_at_unix_ms": now_ms}
                commit_pairing(state)
                return code
    raise RuntimeError("discord pairing code space is saturated")


def approve_pairing(code, now_ms=None):
    code = require_string(code, "discord pairing approval code")
    if not re.fullmatch(r"DC-[0-9]{6}", code):
        raise ValueError("discord pairing approval code is malformed")
    now_ms = int(time.time() * 1000) if now_ms is None else require_int(now_ms, "discord pairing time", 0)
    with _STATE_LOCK:
        state = load_pairing()
        prune_pairing(state, now_ms)
        entry = state["pending"].pop(code, None)
        if entry is None:
            commit_pairing(state)
            raise ValueError("discord pairing approval code is absent or expired")
        user_id = entry["user_id"]
        if user_id not in state["approved"]:
            if len(state["approved"]) >= PAIRING_MAX_APPROVED:
                raise RuntimeError("discord pairing approved capacity is exhausted")
            state["approved"].append(user_id)
        commit_pairing(state)
        return {"status": "approved", "user_id": user_id}


def revoke_pairing(user_id):
    user_id = require_snowflake(user_id, "discord pairing revoke user")
    with _STATE_LOCK:
        state = load_pairing()
        if user_id not in state["approved"]:
            raise ValueError("discord pairing user is not approved")
        state["approved"] = [entry for entry in state["approved"] if entry != user_id]
        state["pending"] = {
            code: entry
            for code, entry in state["pending"].items()
            if entry["user_id"] != user_id
        }
        commit_pairing(state)
        return {"status": "revoked", "user_id": user_id}


def pairing_approved(user_id, now_ms=None):
    if not user_id:
        return False
    user_id = require_snowflake(user_id, "discord pairing user")
    now_ms = int(time.time() * 1000) if now_ms is None else require_int(now_ms, "discord pairing time", 0)
    with _STATE_LOCK:
        state = load_pairing()
        if prune_pairing(state, now_ms):
            commit_pairing(state)
        return user_id in state["approved"]


def validate_coordinates(channel_id, thread_id):
    require_snowflake(channel_id, "discord channel_id")
    if thread_id is not None:
        require_snowflake(thread_id, "discord thread_id")


def coordinate_key(channel_id, thread_id):
    validate_coordinates(channel_id, thread_id)
    return f"{channel_id}:{thread_id or ''}"


def file_hexdigest(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        while True:
            chunk = handle.read(1024 * 1024)
            if not chunk:
                return digest.hexdigest()
            digest.update(chunk)


def _validate_secret_file(path, name):
    try:
        metadata = os.lstat(path)
    except FileNotFoundError as error:
        raise RuntimeError(f"{name} is unavailable") from error
    if not stat.S_ISREG(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise RuntimeError(f"{name} must be a regular non-symlink file")
    if stat.S_IMODE(metadata.st_mode) != 0o600:
        raise RuntimeError(f"{name} must have mode 0600")
    if hasattr(os, "geteuid") and metadata.st_uid != os.geteuid():
        raise RuntimeError(f"{name} must be owned by the interface process user")
    with open(path, "rb") as handle:
        secret = handle.read()
    if len(secret) != 32:
        raise RuntimeError(f"{name} must contain exactly 32 bytes")
    return secret


def _secret_root():
    try:
        metadata = os.lstat(SECRETS_DIR)
    except FileNotFoundError:
        os.makedirs(SECRETS_DIR, mode=0o700, exist_ok=False)
        metadata = os.lstat(SECRETS_DIR)
    if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise RuntimeError("discord binding secret directory must be a regular directory")
    if stat.S_IMODE(metadata.st_mode) != 0o700:
        raise RuntimeError("discord binding secret directory must have mode 0700")
    if hasattr(os, "geteuid") and metadata.st_uid != os.geteuid():
        raise RuntimeError("discord binding secret directory must be owned by the interface process user")
    return os.path.realpath(SECRETS_DIR)


def binding_secret_path(binding):
    opaque_id = require_string(binding.get("opaque_id"), "discord binding opaque_id")
    if not re.fullmatch(r"[0-9a-f]{24}", opaque_id):
        raise ValueError("discord binding opaque_id is malformed")
    root = _secret_root()
    path = os.path.join(root, f"{opaque_id}.key")
    if os.path.commonpath((os.path.realpath(path), root)) != root:
        raise ValueError("discord binding secret handle escaped its private directory")
    return path


def ensure_binding_secret(binding):
    path = binding_secret_path(binding)
    if not os.path.lexists(path):
        atomic_write_bytes(path, secrets.token_bytes(32))
    _validate_secret_file(path, "discord binding secret handle")
    return path


def binding_secret(binding):
    path = binding_secret_path(binding)
    expected = binding.get("secret_handle")
    if expected is not None and os.path.abspath(expected) != path:
        raise RuntimeError("discord binding secret handle disagrees with its durable identity")
    return _validate_secret_file(path, "discord binding secret handle")


def load_secret_handle(path):
    path = require_string(path, "discord binding secret_handle")
    if not os.path.isabs(path):
        raise RuntimeError("discord binding secret_handle must be absolute")
    return _validate_secret_file(path, "discord binding secret_handle")


def grant_media(binding, path):
    path = os.path.realpath(path)
    media_root = os.path.realpath(MEDIA_DIR)
    if os.path.commonpath((path, media_root)) != media_root or not os.path.isfile(path):
        raise ValueError("discord media grant escaped the gateway media directory")
    handle = os.path.basename(path)
    size = os.path.getsize(path)
    digest = file_hexdigest(path)
    message = f"{binding['interface']}\0{handle}\0{digest}\0{size}".encode("utf-8")
    token = hmac.new(binding_secret(binding), message, hashlib.sha256).hexdigest()
    return {"media_handle": handle, "media_sha256": digest, "media_size": size, "media_token": token}


def read_granted_media(interface, handle, digest, size, token, offset, limit):
    interface = require_string(interface, "discord media grant interface")
    handle = require_string(handle, "discord media grant handle")
    digest = require_string(digest, "discord media grant digest")
    token = require_string(token, "discord media grant token")
    size = require_int(size, "discord media grant size", 0)
    offset = require_int(offset, "discord media grant offset", 0, size)
    limit = require_int(limit, "discord media grant limit", 1, 1048576)
    if os.path.basename(handle) != handle or not re.fullmatch(r"[A-Za-z0-9._-]{1,255}", handle):
        raise ValueError("discord media grant handle is malformed")
    if not re.fullmatch(r"[0-9a-f]{64}", digest) or not re.fullmatch(r"[0-9a-f]{64}", token):
        raise ValueError("discord media grant digest or token is malformed")
    binding = binding_by_interface(interface)
    if binding is None:
        raise ValueError("discord media grant interface has no durable binding")
    message = f"{interface}\0{handle}\0{digest}\0{size}".encode("utf-8")
    expected = hmac.new(binding_secret(binding), message, hashlib.sha256).hexdigest()
    if not hmac.compare_digest(expected, token):
        raise ValueError("discord media grant token is invalid")
    path = os.path.realpath(os.path.join(MEDIA_DIR, handle))
    if os.path.commonpath((path, os.path.realpath(MEDIA_DIR))) != os.path.realpath(MEDIA_DIR) or not os.path.isfile(path):
        raise FileNotFoundError("discord granted media is unavailable")
    if os.path.getsize(path) != size:
        raise RuntimeError("discord granted media size changed")
    with open(path, "rb") as source:
        source.seek(offset)
        chunk = source.read(min(limit, size - offset))
    return {"offset": offset, "data_base64": base64.b64encode(chunk).decode("ascii"), "eof": offset + len(chunk) == size}


def _identity_salt():
    if not os.path.lexists(IDENTITY_SALT_PATH):
        atomic_write_bytes(IDENTITY_SALT_PATH, secrets.token_bytes(32))
    return _validate_secret_file(IDENTITY_SALT_PATH, "discord identity salt")


def validate_binding(binding, key):
    fields = {"channel_id", "thread_id", "opaque_id", "interface", "provider_interface", "interface_authoring_interface", "worker_environment_interface", "file_write_tool", "worker_interface", "run", "ee", "ee_template", "secret_handle", "host", "port", "status"}
    require_keys(binding, fields, set(), f"discord binding {key}")
    validate_coordinates(binding["channel_id"], binding["thread_id"])
    if coordinate_key(binding["channel_id"], binding["thread_id"]) != key:
        raise ValueError(f"discord binding {key} coordinates mismatch")
    opaque_id = require_string(binding["opaque_id"], f"discord binding {key}.opaque_id")
    if not re.fullmatch(r"[0-9a-f]{24}", opaque_id):
        raise ValueError(f"discord binding {key}.opaque_id is malformed")
    for field in ("interface", "provider_interface", "interface_authoring_interface", "worker_environment_interface", "file_write_tool", "worker_interface", "run", "ee", "ee_template"):
        require_registry_identifier(binding[field], f"discord binding {key}.{field}")
    host = require_string(binding["host"], f"discord binding {key}.host")
    if not _loopback_host(host):
        raise ValueError(f"discord binding {key}.host must be loopback")
    require_string(binding["secret_handle"], f"discord binding {key}.secret_handle")
    if not os.path.isabs(binding["secret_handle"]):
        raise ValueError(f"discord binding {key}.secret_handle must be absolute")
    binding_secret(binding)
    require_int(binding["port"], f"discord binding {key}.port", 1, 65535)
    if binding["status"] not in ("planned", "ready"):
        raise ValueError(f"discord binding {key}.status is invalid")


def load_bindings():
    state = read_bounded_json(
        BINDINGS_PATH,
        TOPOLOGY_MAX_BYTES,
        "binding state",
        {"schema_version": 1, "bindings": {}},
    )
    state = require_keys(state, {"schema_version", "bindings"}, set(), "discord binding state")
    if state["schema_version"] != 1:
        raise ValueError("discord binding state schema_version must be 1")
    bindings = require_object(state["bindings"], "discord binding state.bindings")
    identities = {name: set() for name in ("opaque_id", "interface", "worker_environment_interface", "file_write_tool", "run", "ee")}
    for key, binding in bindings.items():
        require_string(key, "discord binding key")
        validate_binding(binding, key)
        for field, seen in identities.items():
            if binding[field] in seen:
                raise ValueError(f"discord binding state repeats {field}")
            seen.add(binding[field])
    return state


def binding_by_interface(interface):
    with _STATE_LOCK:
        matches = [value for value in load_bindings()["bindings"].values() if value["interface"] == interface]
    return matches[0] if len(matches) == 1 else None


def ensure_binding_record(config, channel_id, thread_id, host, port):
    channel_id = require_snowflake(channel_id, "discord channel_id")
    thread_id = None if thread_id is None else require_snowflake(thread_id, "discord thread_id")
    host = require_string(host, "discord binding host")
    if not _loopback_host(host):
        raise ValueError("discord binding host must be loopback")
    key = coordinate_key(channel_id, thread_id)
    with _STATE_LOCK:
        state = load_bindings()
        existing = state["bindings"].get(key)
        if existing is not None:
            existing["host"] = host
            existing["port"] = port
            atomic_write_json(BINDINGS_PATH, state, indent=2)
            return existing
        opaque = hashlib.sha256(_identity_salt() + b"\0" + key.encode("utf-8")).hexdigest()[:24]
        prefixes = config["private_run"]["id_prefixes"]
        binding = {
            "channel_id": channel_id,
            "thread_id": thread_id,
            "opaque_id": opaque,
            "interface": f"{prefixes['discord']}{opaque}",
            "provider_interface": f"{prefixes['provider']}{opaque}",
            "interface_authoring_interface": f"{prefixes['interface_authoring']}{opaque}",
            "worker_environment_interface": f"{prefixes['worker_environment']}{opaque}",
            "file_write_tool": f"{prefixes['file_write_tool']}{opaque}",
            "worker_interface": f"{prefixes['worker']}{opaque}",
            "run": f"{prefixes['run']}{opaque}",
            "ee": f"{prefixes['ee']}{opaque}",
            "ee_template": f"{prefixes['ee_template']}{opaque}",
            "host": host,
            "port": port,
            "status": "planned",
        }
        binding["secret_handle"] = ensure_binding_secret(binding)
        validate_binding(binding, key)
        state["bindings"][key] = binding
        atomic_write_json(BINDINGS_PATH, state, indent=2)
        return binding


def mark_binding_ready(binding):
    key = coordinate_key(binding["channel_id"], binding["thread_id"])
    with _STATE_LOCK:
        state = load_bindings()
        current = state["bindings"].get(key)
        if current not in ({**binding, "status": "planned"}, {**binding, "status": "ready"}):
            raise RuntimeError(f"discord binding {key} changed during activation")
        state["bindings"][key] = {**binding, "status": "ready"}
        atomic_write_json(BINDINGS_PATH, state, indent=2)
        binding["status"] = "ready"


def root_slots():
    try:
        slots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
    except (KeyError, json.JSONDecodeError) as error:
        raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON is absent or malformed") from error
    slots = require_object(slots, "OCTOPUS_ROOT_SLOTS_JSON")
    result = {}
    for name in ("registry_interface_root", "registry_ee_template_root", "registry_tool_root", "registry_worker_environment_kind_root", "run_records_root", "ee_instances_root"):
        path = slots.get(name)
        if not isinstance(path, str) or not os.path.isabs(path):
            raise RuntimeError(f"OCTOPUS_ROOT_SLOTS_JSON has no absolute {name}")
        result[name] = os.path.realpath(path)
    return result


def rooted_path(root, *parts):
    root = os.path.realpath(root)
    path = os.path.realpath(os.path.join(root, *parts))
    if os.path.commonpath((path, root)) != root:
        raise ValueError("discord topology path escaped its granted root")
    return path


def json_text(value):
    return json.dumps(value, indent=2, ensure_ascii=False) + "\n"


def child_descriptor(identifier, template, interface_type, role):
    template = require_registry_identifier(template, "discord interface template")
    path = rooted_path(root_slots()["registry_interface_root"], template, "interface.json")
    try:
        descriptor = json.loads(read_bounded_regular(path, TOPOLOGY_MAX_BYTES, "interface template descriptor"))
    except json.JSONDecodeError as error:
        raise ValueError("discord interface template descriptor is invalid JSON") from error
    descriptor = require_object(descriptor, "discord interface template descriptor")
    for field in ("id", "interface_type", "role", "direction", "source_names"):
        if field not in descriptor:
            raise ValueError(f"discord interface template descriptor has no {field}")
    if descriptor["id"] != template:
        raise ValueError("discord interface template identity does not match its registry folder")
    if "reference" in descriptor:
        raise ValueError("discord interface templates must be self-contained exact descriptors")
    if descriptor["interface_type"] != interface_type:
        raise ValueError("discord interface template has the wrong interface_type")
    if descriptor["role"] != role:
        raise ValueError("discord interface template has the wrong role")
    if descriptor["direction"] != "bidirectional":
        raise ValueError("discord interface template must be bidirectional")
    sources = require_string_list(descriptor["source_names"], "discord interface template source_names")
    for source in sources:
        if not re.fullmatch(r"[a-z0-9_-]+(?:\.[a-z0-9_-]+)*", source):
            raise ValueError("discord interface template source_names entry is malformed")
    for field in ("interface_driver_config", "interface_policy"):
        if field in descriptor and not isinstance(descriptor[field], dict):
            raise ValueError(f"discord interface template {field} must be a JSON object")
    child = json.loads(json.dumps(descriptor))
    child["id"] = require_registry_identifier(identifier, "discord exact interface")
    return child


def merge_driver_config(descriptor, overlay):
    current = descriptor.get("interface_driver_config")
    if current is None:
        current = {}
    current = require_object(current, "discord interface_driver_config")
    merged = json.loads(json.dumps(current))
    for key, value in overlay.items():
        if key in ("private_config", "host_slots") and isinstance(merged.get(key), dict) and isinstance(value, dict):
            merged[key].update(json.loads(json.dumps(value)))
        else:
            merged[key] = json.loads(json.dumps(value))
    return merged


def template_life(template):
    template = require_registry_identifier(template, "discord interface template")
    path = rooted_path(root_slots()["registry_interface_root"], template, "life.json")
    try:
        value = json.loads(read_bounded_regular(path, TOPOLOGY_MAX_BYTES, "interface template life"))
    except FileNotFoundError as error:
        raise RuntimeError(
            f"discord topology requires installed interface owner {template!r} with life.json"
        ) from error
    except json.JSONDecodeError as error:
        raise ValueError("discord interface template life is invalid JSON") from error
    value = require_object(value, "discord interface template life")
    if not isinstance(value.get("processes"), list) or not value["processes"]:
        raise ValueError("discord interface template life declares no processes")
    return value


def exact_tool_descriptor(config, identifier, interface):
    private = config["private_run"]
    owner = require_registry_identifier(private["file_write_tool"], "discord file-write tool owner")
    path = rooted_path(root_slots()["registry_tool_root"], owner, "tool.json")
    descriptor, _ = read_topology_json(path, f"tool owner {owner}")
    if descriptor.get("id") != owner:
        raise ValueError("discord tool owner identity does not match its registry folder")
    if "reference" in descriptor:
        raise ValueError("discord tool owner must be a self-contained exact descriptor")
    if descriptor.get("interface") != private["worker_environment_door_owner"]:
        raise ValueError("discord file-write tool owner is not owned by the configured door owner")
    kinds = require_string_list(
        descriptor.get("supported_worker_environment_kinds"),
        "discord file-write tool owner supported_worker_environment_kinds",
    )
    if private["worker_environment_kind"] not in kinds:
        raise ValueError("discord file-write tool owner does not support the configured worker environment kind")
    require_object(descriptor.get("process"), "discord file-write tool owner process")
    require_object(descriptor.get("args_schema"), "discord file-write tool owner args_schema")
    descriptor["id"] = require_registry_identifier(identifier, "discord exact file-write tool")
    descriptor["interface"] = require_registry_identifier(interface, "discord exact worker-environment interface")
    return descriptor


def ee_template_descriptor(config, identifier, interfaces, binding_tool):
    owner = config["private_run"]["ee_template"]
    path = rooted_path(root_slots()["registry_ee_template_root"], owner, "ee_template.json")
    try:
        template = json.loads(
            read_bounded_regular(path, TOPOLOGY_MAX_BYTES, "EE template owner")
        )
    except FileNotFoundError as error:
        raise RuntimeError(
            f"discord topology requires installed EE template owner {owner!r}"
        ) from error
    except json.JSONDecodeError as error:
        raise ValueError("discord EE template owner is invalid JSON") from error
    template = require_object(template, "discord EE template owner")
    if template.get("id") != owner:
        raise ValueError("discord EE template owner identity does not match its registry folder")
    require_string(template.get("goal_class"), "discord EE template owner goal_class")
    for field in (
        "allowed_tools",
        "allowed_interfaces",
        "allowed_worker_environment_kinds",
        "allowed_root_slots",
        "hard_blocked_tools",
        "allowed_child_ee_templates",
    ):
        if field in template:
            require_string_list(template[field], f"discord EE template owner {field}", True)
    if config["private_run"]["worker_environment_kind"] not in template.get(
        "allowed_worker_environment_kinds", []
    ):
        raise ValueError(
            "discord EE template owner does not admit the configured worker environment kind"
        )
    template["id"] = require_registry_identifier(identifier, "discord exact EE template")
    template["allowed_interfaces"] = list(dict.fromkeys(interfaces))
    allowed_tools = template.get("allowed_tools")
    if allowed_tools is not None:
        allowed_tools = require_string_list(allowed_tools, "discord EE template owner allowed_tools", True)
        owner_tool = config["private_run"]["file_write_tool"]
        if owner_tool not in allowed_tools:
            raise ValueError("discord EE template owner does not admit the file-write tool owner")
        template["allowed_tools"] = list(
            dict.fromkeys(
                binding_tool if tool == owner_tool else tool
                for tool in allowed_tools
            )
        )
    return template


def read_topology_json(path, label):
    raw = read_bounded_regular(path, TOPOLOGY_MAX_BYTES, label)
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ValueError(f"discord {label} is invalid JSON") from error
    return require_object(value, f"discord {label}"), raw


def worker_role_artifact_bindings(worker, policy=None):
    worker = require_registry_identifier(worker, "discord exact worker interface")
    if policy is None:
        path = rooted_path(root_slots()["registry_interface_root"], worker, "policy.json")
        try:
            policy, _ = read_topology_json(path, f"exact worker {worker} policy")
        except FileNotFoundError as error:
            raise RuntimeError(
                f"discord topology requires exact worker policy {worker!r}"
            ) from error
    policy = require_object(policy, "discord exact worker policy")
    if policy.get("id") != worker:
        raise ValueError("discord exact worker policy identity does not match its registry folder")
    roles = require_string_list(
        policy.get("role_artifacts"),
        "discord exact worker policy.role_artifacts",
        True,
    )
    return [{"id": artifact} for artifact in roles]


def append_missing_strings(document, field, additions, label):
    current = require_string_list(
        document.get(field), f"discord {label}.{field}", True
    )
    document[field] = current
    seen = set(current)
    for value in additions:
        require_string(value, f"discord {label}.{field} addition")
        if value not in seen:
            current.append(value)
            seen.add(value)


def parent_ee_chain(config):
    current = config["private_run"]["parent_ee"]
    seen_ees = set()
    chain = []
    while current is not None:
        current = require_registry_identifier(current, "discord parent EE")
        if current in seen_ees:
            raise ValueError("discord parent EE chain contains a cycle")
        seen_ees.add(current)
        instance_path = rooted_path(root_slots()["ee_instances_root"], f"{current}.json")
        instance, _ = read_topology_json(instance_path, f"parent EE {current}")
        if instance.get("ee_id") != current:
            raise ValueError("discord parent EE identity drifted")
        template = require_registry_identifier(instance.get("template_name"), "discord parent EE template")
        if template not in chain:
            chain.append(template)
        current = instance.get("parent_ee_id")
    if not chain:
        raise ValueError("discord private topology has no parent EE chain")
    return chain


def shared_policy_writes(config, binding):
    roots = root_slots()
    door = binding["worker_environment_interface"]
    tool = binding["file_write_tool"]
    exact_interfaces = [
        binding["interface"],
        binding["provider_interface"],
        binding["interface_authoring_interface"],
        door,
        binding["worker_interface"],
    ]
    writes = []
    parent_templates = parent_ee_chain(config)
    for index, parent_template in enumerate(parent_templates):
        parent_path = rooted_path(
            roots["registry_ee_template_root"], parent_template, "ee_template.json"
        )
        parent, parent_raw = read_topology_json(parent_path, f"parent EE template {parent_template}")
        if parent.get("id") != parent_template:
            raise ValueError("discord parent EE template identity drifted")
        allowed_kinds = parent.get("allowed_worker_environment_kinds")
        if allowed_kinds is not None:
            require_string_list(
                allowed_kinds,
                "discord parent EE template allowed_worker_environment_kinds",
                True,
            )
            if config["private_run"]["worker_environment_kind"] not in allowed_kinds:
                raise ValueError(
                    "discord parent EE template does not admit the configured worker environment kind"
                )
        append_missing_strings(parent, "allowed_interfaces", exact_interfaces, "parent EE template")
        if parent.get("allowed_tools") is not None:
            append_missing_strings(parent, "allowed_tools", [tool], "parent EE template")
        if index == 0:
            append_missing_strings(
                parent,
                "allowed_child_ee_templates",
                [binding["ee_template"]],
                "parent EE template",
            )
        parent_content = json_text(parent)
        if parent_content.encode("utf-8") != parent_raw:
            writes.append(
                {
                    "path": parent_path,
                    "content": parent_content,
                    "expected_before_hash": octopus_content_hash(parent_raw),
                }
            )
    return sorted(writes, key=lambda write: write["path"])


def child_life_json(secret_handle):
    secret_handle = require_string(secret_handle, "discord child secret_handle")
    if not os.path.isabs(secret_handle):
        raise ValueError("discord child secret_handle must be absolute")
    load_secret_handle(secret_handle)
    runtime_source = os.environ.get("OCTOPUS_CONTENT_HASH_RUNTIME")
    if not isinstance(runtime_source, str) or not os.path.isabs(runtime_source):
        raise RuntimeError("discord child topology requires the package-owned dependency runtime")
    runtime_source = os.path.realpath(runtime_source)
    return {
        "processes": [{
            "bin": "python3",
            "max_in_flight_tool_processes": 16,
            "io_limits": {"max_control_frame_bytes": 1048576, "max_stderr_bytes": 262144, "max_manifest_bytes": 1048576, "max_payload_bytes": 268435456},
            "args": [{"repository_file": "registry/interface/world_discord/discord_boot.py"}],
            "required_files": [
                "registry/interface/world_discord/discord_boot.py",
                "registry/interface/world_discord/discord_life.py",
                "registry/interface/world_discord/discord_boundary.py",
                "registry/interface/world_discord/discord_voice.py",
                "registry/interface/world_discord/octopus_content_hash.py",
                "registry/interface/world_discord/requirements.txt",
                "registry/interface/world_discord/requirements.lock.txt",
            ],
            "env": {"OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE": runtime_source},
            "inherit_env": ["PATH"],
            "authority": {
                "read_root_slots": [], "write_root_slots": [], "executable_root_slots": [],
                "read_paths": [secret_handle, runtime_source], "write_paths": [], "executable_paths": [runtime_source],
                "executable_commands": ["python3"], "read_env_paths": [],
                "write_env_paths": [], "executable_env_paths": [], "allow_network": False,
                "allow_local_ipc": True,
            },
            "read_handles": ["ledger", "blobs", "ledger_index"],
        }]
    }


def binding_topology(config, binding):
    private = config["private_run"]
    owners = private["owners"]
    interface = binding["interface"]
    provider = binding["provider_interface"]
    authoring_source = binding["interface_authoring_interface"]
    authoring = binding["worker_environment_interface"]
    file_write_tool = binding["file_write_tool"]
    worker = binding["worker_interface"]
    run = binding["run"]
    ee = binding["ee"]
    ee_template = binding["ee_template"]
    prefix = f"cp_{binding['opaque_id'][:12]}"
    child_config = json.loads(json.dumps(config))
    child_config["mode"] = "binding"
    child_config.pop("gateway_run_template", None)
    secret_handle = ensure_binding_secret(binding)
    child_config["binding"] = {"interface": interface, "channel_id": binding["channel_id"], "thread_id": binding["thread_id"], "gateway_interface": INTERFACE_ID, "secret_handle": secret_handle, "host": binding["host"], "port": binding["port"], "run": run}
    discord_descriptor = child_descriptor(
        interface, owners["discord"], "world", "service"
    )
    provider_descriptor = child_descriptor(
        provider, owners["provider"], "world", "provider"
    )
    provider_descriptor["interface_driver_config"] = merge_driver_config(provider_descriptor, {
        "private_config": {
            "model": private["model"],
            "reasoning_effort": private["reasoning_effort"],
            "context_output_labels": private["provider_context_output_labels"],
            "run_authoring_profile": {
                "file_write_tool": file_write_tool,
                "file_write_label": private["file_write_label"],
                "file_write_result_label": private["file_write_result_label"],
                "result_timeout_ms": private["result_timeout_ms"],
            },
        }
    })
    provider_descriptor["interface_driver_config"]["private_config"].pop("run_authoring", None)
    authoring_source_descriptor = child_descriptor(
        authoring_source,
        owners["interface_authoring"],
        "system",
        "system",
    )
    worker_descriptor = child_descriptor(
        worker, owners["worker"], "system", "worker"
    )
    door_descriptor = child_descriptor(
        authoring,
        private["worker_environment_door_owner"],
        "system",
        "system",
    )
    tool_descriptor = exact_tool_descriptor(config, file_write_tool, authoring)
    exact_template = ee_template_descriptor(
        config,
        ee_template,
        [interface, provider, authoring_source, authoring, worker],
        file_write_tool,
    )
    worker_policy = {
        "id": worker,
        "ceiling": {
            "interfaces": [interface, provider, authoring_source, authoring, worker],
            "tools": [file_write_tool], "wek": [private["worker_environment_kind"]],
            "mark_outcomes": [private["file_write_mark_outcome"]], "hard_blocked_tools": [],
        },
        "role_artifacts": private["role_artifacts"],
    }
    mandatory_roles = worker_role_artifact_bindings(worker, worker_policy)
    context = list(mandatory_roles)
    run_record = {
        "id": run, "ee": ee, "worker": worker, "trace": f"trace:{run}", "status": "running",
        "interface_state": {provider: {"context_generation": 0, "authoring_generation": 0, "frontier_checkpoint_ids": [f"{prefix}_discord_output", f"{prefix}_authoring_call", f"{prefix}_authoring_result"]}},
        "checkpoints": [
            {"id": f"{prefix}_discord_input", "run": run, "trace": f"trace:{run}:{prefix}_discord_input", "epoch": 0, "interface_binding": authored_interface_binding(interface), "outbound_interface_bindings": [authored_interface_binding(provider)], "artifact_bindings": context, "required_verified_artifacts": []},
            {"id": f"{prefix}_discord_output", "run": run, "trace": f"trace:{run}:{prefix}_discord_output", "epoch": 0, "interface_binding": authored_interface_binding(provider), "outbound_interface_bindings": [authored_interface_binding(interface)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
            {"id": f"{prefix}_authoring_call", "run": run, "trace": f"trace:{run}:{prefix}_authoring_call", "epoch": 0, "interface_binding": authored_interface_binding(authoring_source), "outbound_interface_bindings": [authored_interface_binding(authoring)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
            {"id": f"{prefix}_authoring_result", "run": run, "trace": f"trace:{run}:{prefix}_authoring_result", "epoch": 0, "interface_binding": authored_interface_binding(authoring), "outbound_interface_bindings": [authored_interface_binding(provider)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
        ],
        "active_checkpoints": [f"{prefix}_discord_input", f"{prefix}_discord_output", f"{prefix}_authoring_call", f"{prefix}_authoring_result"],
    }
    activation = {"schema_version": 1, "interface": authoring_source, "activations": [{"activation_driver_ref": "app_server_transport", "config": {"bind_interface_socket_when_env_unset": True}}]}
    ee_instance = {"ee_id": ee, "template_name": ee_template, "parent_ee_id": private["parent_ee"], "goal": f"Serve the Discord boundary bound to interface {interface}"}
    roots = root_slots()
    interface_root = roots["registry_interface_root"]
    topology = {
        rooted_path(interface_root, interface, "config.json"): json_text(child_config),
        rooted_path(interface_root, interface, "life.json"): json_text(child_life_json(secret_handle)),
        rooted_path(interface_root, interface, "interface.json"): json_text(discord_descriptor),
        rooted_path(interface_root, provider, "life.json"): json_text(template_life(owners["provider"])),
        rooted_path(interface_root, provider, "interface.json"): json_text(provider_descriptor),
        rooted_path(interface_root, authoring_source, "life.json"): json_text(template_life(owners["interface_authoring"])),
        rooted_path(interface_root, authoring_source, "activation.json"): json_text(activation),
        rooted_path(interface_root, authoring_source, "interface.json"): json_text(authoring_source_descriptor),
        rooted_path(interface_root, authoring, "life.json"): json_text(template_life(private["worker_environment_door_owner"])),
        rooted_path(interface_root, authoring, "interface.json"): json_text(door_descriptor),
        rooted_path(roots["registry_tool_root"], file_write_tool, "tool.json"): json_text(tool_descriptor),
        rooted_path(interface_root, worker, "policy.json"): json_text(worker_policy),
        rooted_path(interface_root, worker, "interface.json"): json_text(worker_descriptor),
        rooted_path(roots["registry_ee_template_root"], ee_template, "ee_template.json"): json_text(exact_template),
        rooted_path(roots["ee_instances_root"], f"{ee}.json"): json_text(ee_instance),
        rooted_path(roots["run_records_root"], f"{run}.json"): json_text(run_record),
    }
    return topology


def ensure_gateway_run(config):
    template = require_object(config.get("gateway_run_template"), "discord gateway_run_template")
    run_id = require_string(template.get("id"), "discord gateway_run_template.id")
    path = os.path.join(root_slots()["run_records_root"], f"{run_id}.json")
    try:
        record = require_object(
            json.loads(
                read_bounded_regular(
                    path,
                    config["driver_ipc"]["max_manifest_bytes"],
                    "gateway run",
                )
            ),
            "discord gateway run",
        )
    except FileNotFoundError as error:
        raise RuntimeError(
            f"discord gateway run {path!r} is not authored; provision gateway_run_template before enabling the interface"
        ) from error
    if not isinstance(record, dict) or record.get("id") != run_id or record.get("ee") != template.get("ee") or record.get("worker") != template.get("worker"):
        raise RuntimeError("discord gateway run identity conflicts with gateway_run_template")
    configured_door = config["private_run"]["gateway_worker_environment_interface"]
    if not any(
        require_interface_binding(checkpoint.get("interface_binding"), f"discord gateway checkpoint {checkpoint.get('id')}.interface_binding") == configured_door
        or configured_door in [
            require_interface_binding(binding, f"discord gateway checkpoint {checkpoint.get('id')}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(checkpoint.get("outbound_interface_bindings", []))
        ]
        for checkpoint in record.get("checkpoints", [])
        if isinstance(checkpoint, dict)
    ):
        raise RuntimeError("discord gateway run is not bound to its exact worker-environment interface")
    return path


def gateway_rearm_write(config, opaque_id):
    path = ensure_gateway_run(config)
    current = read_bounded_regular(
        path,
        config["driver_ipc"]["max_manifest_bytes"],
        "gateway run",
    )
    record = require_object(json.loads(current), "discord gateway run")
    template = config["gateway_run_template"]
    run_id = template["id"]
    worker = template["worker"]
    authoring = config["private_run"]["gateway_worker_environment_interface"]
    if record.get("id") != run_id or record.get("worker") != worker:
        raise RuntimeError("discord gateway run identity drifted")
    mandatory_roles = worker_role_artifact_bindings(worker)
    interface_state = require_object(record.setdefault("interface_state", {}), "discord gateway interface_state")
    state = require_object(interface_state.setdefault(INTERFACE_ID, {}), "discord gateway interface state")
    generation = require_int(state.get("authoring_generation", 0), "discord authoring_generation", 0) + 1
    state["authoring_generation"] = generation
    suffix = f"{opaque_id[:12]}_{generation}"
    call = f"cp_discord_gateway_authoring_call_{suffix}"
    result = f"cp_discord_gateway_authoring_result_{suffix}"
    record["checkpoints"] = [
        {"id": call, "run": run_id, "trace": f"trace:{run_id}:{call}", "epoch": 0, "interface_binding": authored_interface_binding(INTERFACE_ID), "outbound_interface_bindings": [authored_interface_binding(authoring)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
        {"id": result, "run": run_id, "trace": f"trace:{run_id}:{result}", "epoch": 0, "interface_binding": authored_interface_binding(authoring), "outbound_interface_bindings": [authored_interface_binding(INTERFACE_ID)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
    ]
    record["active_checkpoints"] = [call, result]
    return {"path": path, "content": json_text(record), "expected_before_hash": octopus_content_hash(current)}


def missing_topology_writes(config, topology):
    ownership = load_topology_ownership()
    writes = []
    for path, content in topology.items():
        desired = content.encode("utf-8")
        try:
            existing = read_bounded_regular(
                path,
                config["driver_ipc"]["max_manifest_bytes"],
                "topology document",
            )
        except FileNotFoundError:
            if path in ownership["paths"]:
                raise RuntimeError(f"discord package-authored topology target {path!r} disappeared")
            writes.append({"path": path, "content": content, "expected_before_hash": None})
            continue
        if existing == desired:
            continue
        observed = octopus_content_hash(existing)
        if ownership["paths"].get(path) != observed:
            raise RuntimeError(f"discord private topology target {path!r} drifted from its last package-authored bytes")
        writes.append({"path": path, "content": content, "expected_before_hash": observed})
    return writes


def load_topology_ownership():
    state = read_bounded_json(
        TOPOLOGY_OWNERSHIP_PATH,
        TOPOLOGY_MAX_BYTES,
        "topology ownership",
        {"schema_version": 1, "paths": {}},
    )
    state = require_keys(state, {"schema_version", "paths"}, set(), "discord topology ownership")
    if state["schema_version"] != 1:
        raise ValueError("discord topology ownership schema_version must be 1")
    paths = require_object(state["paths"], "discord topology ownership.paths")
    if len(paths) > TOPOLOGY_OWNERSHIP_MAX_PATHS:
        raise ValueError("discord topology ownership has too many paths")
    for path, digest in paths.items():
        require_string(path, "discord topology ownership path")
        if not os.path.isabs(path):
            raise ValueError("discord topology ownership path must be absolute")
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError("discord topology ownership digest is malformed")
    return state


def commit_topology_ownership(config, topology):
    ownership = load_topology_ownership()
    for path, content in sorted(topology.items()):
        desired = content.encode("utf-8")
        existing = read_bounded_regular(
            path,
            config["driver_ipc"]["max_manifest_bytes"],
            "topology document",
        )
        if existing != desired:
            raise RuntimeError(f"discord package-authored topology target {path!r} did not materialize exactly")
        ownership["paths"][path] = octopus_content_hash(existing)
    if len(ownership["paths"]) > TOPOLOGY_OWNERSHIP_MAX_PATHS:
        raise ValueError("discord topology ownership has too many paths")
    atomic_write_json(TOPOLOGY_OWNERSHIP_PATH, ownership, indent=2)


def package_owned_topology(topology):
    roots = root_slots()
    owned_roots = (
        roots["registry_interface_root"],
        roots["registry_ee_template_root"],
        roots["registry_tool_root"],
    )
    return {
        path: content
        for path, content in topology.items()
        if any(
            os.path.commonpath((os.path.realpath(path), root)) == root
            and os.path.realpath(path) != root
            for root in owned_roots
        )
    }


def wait_for_shared_policy_writes(config, binding, submitted):
    submitted_by_path = {write["path"]: write for write in submitted}
    deadline = time.monotonic() + config["private_run"]["result_timeout_ms"] / 1000.0
    while True:
        pending = shared_policy_writes(config, binding)
        if not pending:
            return True
        for write in pending:
            original = submitted_by_path.get(write["path"])
            if (
                original is None
                or original["expected_before_hash"] != write["expected_before_hash"]
            ):
                return False
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        time.sleep(min(0.05, remaining))


def reset_authoring_coordinator():
    global _AUTHORING_FATAL
    with _AUTHORING_LOCK:
        _AUTHORING_FATAL = None


def ensure_private_binding(config, hub, channel_id, thread_id, evidence=None):
    global _AUTHORING_FATAL
    with _AUTHORING_LOCK:
        if _AUTHORING_FATAL is not None:
            raise RuntimeError(f"discord topology authoring is fail-closed after an unresolved attempt: {_AUTHORING_FATAL}")
        binding = ensure_binding_record(config, channel_id, thread_id, hub.host, hub.port)
        try:
            for attempt in range(config["private_run"]["authoring_max_attempts"]):
                topology = binding_topology(config, binding)
                managed_topology = package_owned_topology(topology)
                topology_writes = missing_topology_writes(
                    config,
                    managed_topology if binding["status"] == "ready" else topology,
                )
                policy_writes = shared_policy_writes(config, binding)
                pending_writes = topology_writes + policy_writes
                if pending_writes:
                    rearm = gateway_rearm_write(config, binding["opaque_id"])
                    writes = [rearm, *pending_writes]
                    if evidence is not None:
                        evidence("topology_authoring", "prepared", {"binding_interface": binding["interface"], "write_count": len(writes), "gateway_before_hash": rearm["expected_before_hash"]})
                    nonce = time.time_ns()
                    admitted_seq = hand_json(
                        config["private_run"]["file_write_label"],
                        {"tool": config["private_run"]["gateway_file_write_tool"], "arguments": {"writes": writes}, "correlation_id": f"discord-binding-authoring:{binding['opaque_id']}:{nonce}"},
                        f"discord-binding-authoring-{binding['opaque_id']}-{nonce}",
                    )
                    if evidence is not None:
                        evidence("topology_authoring", "file_write_admitted", {"binding_interface": binding["interface"], "movement_seq": admitted_seq})
                if policy_writes and not wait_for_shared_policy_writes(
                    config, binding, policy_writes
                ):
                    if attempt + 1 < config["private_run"]["authoring_max_attempts"]:
                        continue
                    raise RuntimeError(
                        "discord exact policy admission could not be authored without overwriting concurrent registry edits"
                    )
                hub.wait_for(binding["interface"], config["driver_ipc"]["connect_timeout_ms"])
                commit_topology_ownership(config, managed_topology)
                if binding["status"] != "ready":
                    mark_binding_ready(binding)
                if evidence is not None:
                    evidence("topology_authoring", "binding_ready", {"binding_interface": binding["interface"]})
                return binding
            raise RuntimeError("discord private topology authoring exhausted its attempts")
        except Exception as error:
            message = sanitize_external_error(config, error)
            _AUTHORING_FATAL = message
            if evidence is not None:
                evidence("topology_authoring", "failed", {"binding_interface": binding["interface"], "error": message[:config["inbound_journal"]["max_error_chars"]]})
            raise


def reconcile_known_bindings(config, hub, evidence=None):
    with _STATE_LOCK:
        coordinates = [
            (binding["channel_id"], binding["thread_id"])
            for _, binding in sorted(load_bindings()["bindings"].items())
        ]
    for channel_id, thread_id in coordinates:
        ensure_private_binding(config, hub, channel_id, thread_id, evidence)


def read_frame(sock, buffer, maximum):
    while True:
        newline = buffer.find(b"\n")
        if newline >= 0:
            payload = bytes(buffer[:newline])
            del buffer[: newline + 1]
            if not payload:
                raise RuntimeError("discord IPC received an empty frame")
            value = json.loads(payload)
            if not isinstance(value, dict):
                raise RuntimeError("discord IPC frame must be an object")
            return value
        if len(buffer) >= maximum:
            raise RuntimeError("discord IPC frame exceeds max_frame_bytes")
        chunk = sock.recv(min(65536, maximum - len(buffer)))
        if not chunk:
            raise ConnectionError("discord IPC peer closed")
        buffer.extend(chunk)


def write_frame(sock, value, maximum, lock=None):
    payload = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
    if len(payload) > maximum:
        raise RuntimeError("discord IPC frame exceeds max_frame_bytes")
    if lock is None:
        sock.sendall(payload)
    else:
        with lock:
            sock.sendall(payload)


class JsonPeer:
    def __init__(self, sock, maximum, timeout_seconds, handler):
        self.sock = sock
        self.maximum = maximum
        self.timeout_seconds = timeout_seconds
        self.handler = handler
        self.buffer = bytearray()
        self.write_lock = threading.Lock()
        self.condition = threading.Condition()
        self.responses = {}
        self.pending_requests = set()
        self.next_id = 1
        self.last_inbound_request_id = 0
        self.request_queue = queue.Queue(maxsize=PRIVATE_REQUEST_QUEUE_LIMIT)
        self.closed = threading.Event()
        self.error = None
        self.request_workers = [
            threading.Thread(target=self._request_worker, daemon=True)
            for _ in range(PRIVATE_REQUEST_WORKERS)
        ]
        for worker in self.request_workers:
            worker.start()
        self.reader = threading.Thread(target=self._read_loop, daemon=True)
        self.reader.start()

    def _record_response(self, request_id, frame):
        with self.condition:
            if request_id in self.pending_requests:
                if request_id in self.responses:
                    raise RuntimeError("discord IPC peer sent a duplicate response")
                self.responses[request_id] = frame
                self.condition.notify_all()
            elif request_id >= self.next_id:
                raise RuntimeError("discord IPC peer sent an unsolicited response")

    def _read_loop(self):
        try:
            while not self.closed.is_set():
                frame = read_frame(self.sock, self.buffer, self.maximum)
                frame_type = frame.get("type")
                request_id = frame.get("request_id")
                if frame_type == "response" and isinstance(request_id, int) and not isinstance(request_id, bool):
                    self._record_response(request_id, frame)
                elif frame_type == "request" and isinstance(request_id, int) and not isinstance(request_id, bool):
                    if request_id <= self.last_inbound_request_id:
                        raise RuntimeError("discord IPC peer repeated or reordered a request id")
                    self.last_inbound_request_id = request_id
                    try:
                        self.request_queue.put_nowait((request_id, frame))
                    except queue.Full:
                        self._write_response(
                            {
                                "type": "response",
                                "request_id": request_id,
                                "ok": False,
                                "error": external_error_observation(
                                    RuntimeError("discord IPC request capacity is exhausted")
                                ),
                            }
                        )
                else:
                    raise RuntimeError("discord IPC peer sent an invalid frame")
        except Exception as error:
            self.error = error
            self.close()

    def _request_worker(self):
        while not self.closed.is_set():
            try:
                request_id, frame = self.request_queue.get(timeout=0.1)
            except queue.Empty:
                continue
            try:
                self._handle_request(request_id, frame)
            finally:
                self.request_queue.task_done()

    def _write_response(self, response):
        try:
            write_frame(self.sock, response, self.maximum, self.write_lock)
        except Exception as error:
            self.error = error
            self.close()

    def _handle_request(self, request_id, frame):
        try:
            result = self.handler(frame.get("operation"), frame.get("payload"))
            response = {"type": "response", "request_id": request_id, "ok": True, "result": result}
        except Exception as error:
            response = {"type": "response", "request_id": request_id, "ok": False, "error": external_error_observation(error)}
        self._write_response(response)

    def request(self, operation, payload):
        with self.write_lock:
            with self.condition:
                if len(self.pending_requests) >= PRIVATE_REQUEST_QUEUE_LIMIT:
                    raise RuntimeError("discord IPC outgoing request capacity is exhausted")
                request_id = self.next_id
                self.next_id += 1
                self.pending_requests.add(request_id)
            try:
                write_frame(
                    self.sock,
                    {"type": "request", "request_id": request_id, "operation": operation, "payload": payload},
                    self.maximum,
                )
            except Exception:
                with self.condition:
                    self.pending_requests.discard(request_id)
                self.close()
                raise
        deadline = time.monotonic() + self.timeout_seconds
        with self.condition:
            while request_id not in self.responses and not self.closed.is_set():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    self.pending_requests.discard(request_id)
                    self.responses.pop(request_id, None)
                    raise TimeoutError(f"discord IPC {operation} timed out")
                self.condition.wait(remaining)
            if request_id not in self.responses:
                self.pending_requests.discard(request_id)
                self.responses.pop(request_id, None)
                raise ConnectionError(f"discord IPC closed: {self.error}")
            response = self.responses.pop(request_id)
            self.pending_requests.discard(request_id)
        if response.get("ok") is not True:
            raise RuntimeError(f"discord IPC {operation} failed: {response.get('error')}")
        return response.get("result")

    def close(self):
        if self.closed.is_set():
            return
        self.closed.set()
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass
        with self.condition:
            self.condition.notify_all()


class GatewayHub:
    def __init__(self, config, outbound_handler):
        self.config = config
        self.outbound_handler = outbound_handler
        self.condition = threading.Condition()
        self.peers = {}
        self.maximum = config["driver_ipc"]["max_frame_bytes"]
        self.timeout_seconds = config["driver_ipc"]["request_timeout_ms"] / 1000.0
        self.closed = threading.Event()
        self.registration_slots = threading.BoundedSemaphore(PRIVATE_REGISTRATION_LIMIT)
        self.listener, self.host, self.port = self._bind_listener()
        threading.Thread(target=self._accept_loop, daemon=True).start()

    def _bind_listener(self):
        ipc = self.config["driver_ipc"]
        state_path = os.path.join(VAR_DIR, ipc["state_file"])
        state = read_json(state_path)
        if state is not None:
            state = require_keys(state, {"host", "port"}, set(), "discord IPC state")
            host = require_string(state["host"], "discord IPC state.host")
            port = require_int(state["port"], "discord IPC state.port", 1, 65535)
            if host != ipc["listen"] or ipc["port"] not in (0, port):
                raise RuntimeError("discord IPC config disagrees with durable address")
        else:
            host, port = ipc["listen"], ipc["port"]
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((host, port))
        listener.listen(128)
        actual_host, actual_port = listener.getsockname()
        if state is None:
            atomic_write_json(state_path, {"host": actual_host, "port": actual_port}, indent=2)
        return listener, actual_host, actual_port

    def _accept_loop(self):
        while not self.closed.is_set():
            try:
                sock, _ = self.listener.accept()
            except OSError:
                if self.closed.is_set():
                    return
                raise
            if not self.registration_slots.acquire(blocking=False):
                try:
                    sock.close()
                except OSError:
                    pass
                continue
            try:
                threading.Thread(target=self._register, args=(sock,), daemon=True).start()
            except Exception:
                self.registration_slots.release()
                try:
                    sock.close()
                except OSError:
                    pass
                raise

    def _register(self, sock):
        peer = None
        interface = None
        registration_released = False
        try:
            sock.settimeout(self.config["driver_ipc"]["connect_timeout_ms"] / 1000.0)
            buffer = bytearray()
            registration_maximum = min(self.maximum, PRIVATE_REGISTRATION_MAX_BYTES)
            frame = read_frame(sock, buffer, registration_maximum)
            if frame.get("type") != "register":
                raise RuntimeError("discord IPC first frame must be register")
            interface = require_string(frame.get("interface"), "discord IPC interface")
            secret = require_string(frame.get("secret"), "discord IPC secret")
            binding = binding_by_interface(interface)
            if binding is None or not hmac.compare_digest(binding_secret(binding).hex(), secret):
                raise RuntimeError("discord IPC registration was not authorized")
            write_frame(sock, {"type": "registered", "interface": interface}, registration_maximum)
            sock.settimeout(None)
            peer = JsonPeer(sock, self.maximum, self.timeout_seconds, self.outbound_handler)
            peer.buffer.extend(buffer)
            with self.condition:
                previous = self.peers.get(interface)
                self.peers[interface] = peer
                self.condition.notify_all()
            if previous is not None:
                previous.close()
            self.registration_slots.release()
            registration_released = True
            peer.closed.wait()
        except Exception as error:
            sys.stderr.write(f"discord IPC registration failed: {error}\n")
            sys.stderr.flush()
        finally:
            if not registration_released:
                self.registration_slots.release()
            if interface is not None:
                with self.condition:
                    if self.peers.get(interface) is peer:
                        self.peers.pop(interface, None)
            if peer is not None:
                peer.close()
            else:
                try:
                    sock.close()
                except OSError:
                    pass

    def wait_for(self, interface, timeout_ms):
        deadline = time.monotonic() + timeout_ms / 1000.0
        with self.condition:
            while True:
                if self.closed.is_set():
                    raise ConnectionError("discord gateway hub is closed")
                peer = self.peers.get(interface)
                if peer is not None and not peer.closed.is_set():
                    return peer
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"discord binding {interface} did not activate")
                self.condition.wait(remaining)

    def dispatch(self, binding, payload):
        peer = self.wait_for(binding["interface"], self.config["driver_ipc"]["connect_timeout_ms"])
        result = peer.request("arrival", payload)
        if not isinstance(result, dict) or not isinstance(result.get("admitted_seq"), int):
            raise RuntimeError("discord binding returned invalid arrival evidence")
        return result["admitted_seq"]

    def close(self):
        if self.closed.is_set():
            return
        self.closed.set()
        try:
            self.listener.close()
        except OSError:
            pass
        with self.condition:
            peers = list(self.peers.values())
            self.peers.clear()
            self.condition.notify_all()
        for peer in peers:
            peer.close()


class BindingLink:
    def __init__(self, config, arrival_handler):
        self.config = config
        self.arrival_handler = arrival_handler
        self.condition = threading.Condition()
        self.peer = None
        self.closed = False
        threading.Thread(target=self._connect_loop, daemon=True).start()

    def _connect_loop(self):
        binding = self.config["binding"]
        ipc = self.config["driver_ipc"]
        backoff = ipc["reconnect_backoff_ms"]
        while not self.closed:
            peer = None
            try:
                sock = socket.create_connection((binding["host"], binding["port"]), ipc["connect_timeout_ms"] / 1000.0)
                write_frame(sock, {"type": "register", "interface": INTERFACE_ID, "secret": load_secret_handle(binding["secret_handle"]).hex()}, ipc["max_frame_bytes"])
                buffer = bytearray()
                registered = read_frame(sock, buffer, ipc["max_frame_bytes"])
                if registered != {"type": "registered", "interface": INTERFACE_ID}:
                    raise RuntimeError("discord gateway returned invalid registration")
                sock.settimeout(None)
                peer = JsonPeer(sock, ipc["max_frame_bytes"], ipc["request_timeout_ms"] / 1000.0, self._handle)
                peer.buffer.extend(buffer)
                with self.condition:
                    self.peer = peer
                    self.condition.notify_all()
                backoff = ipc["reconnect_backoff_ms"]
                peer.closed.wait()
            except Exception as error:
                sys.stderr.write(f"discord binding IPC disconnected: {error}\n")
                sys.stderr.flush()
            finally:
                with self.condition:
                    if self.peer is peer:
                        self.peer = None
                if peer is not None:
                    peer.close()
            time.sleep(backoff / 1000.0)
            backoff = min(backoff * 2, ipc["reconnect_backoff_cap_ms"])

    def _handle(self, operation, payload):
        if operation != "arrival":
            raise RuntimeError(f"discord binding does not accept operation {operation!r}")
        admitted_seq = self.arrival_handler(payload, self)
        return {"admitted_seq": admitted_seq}

    def request(self, operation, payload):
        deadline = time.monotonic() + self.config["driver_ipc"]["connect_timeout_ms"] / 1000.0
        with self.condition:
            while self.peer is None or self.peer.closed.is_set():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("discord gateway IPC is unavailable")
                self.condition.wait(remaining)
            peer = self.peer
        return peer.request(operation, payload)


def load_cursor():
    state = read_json(CURSOR_PATH, {"schema_version": 1, "next_seq": 0})
    state = require_keys(state, {"schema_version", "next_seq"}, set(), "discord ledger cursor")
    if state["schema_version"] != 1:
        raise ValueError("discord ledger cursor schema_version must be 1")
    return require_int(state["next_seq"], "discord ledger cursor.next_seq", 0)


def commit_cursor(next_seq):
    atomic_write_json(CURSOR_PATH, {"schema_version": 1, "next_seq": next_seq})


def content_hexdigest(content):
    if isinstance(content, list):
        if any(isinstance(item, bool) or not isinstance(item, int) or not 0 <= item <= 255 for item in content):
            raise ValueError("discord movement artifact content contains a non-byte")
        content = bytes(content)
    if not isinstance(content, (bytes, bytearray)) or len(content) != 32:
        raise ValueError("discord movement artifact content must be a 32-byte hash")
    return bytes(content).hex()


def read_bounded_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise ValueError(f"discord {name} is not a regular file")
        if (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ValueError(f"discord {name} changed while opening")
        if opened.st_size > maximum:
            raise ValueError(f"discord {name} exceeds {maximum} bytes")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ValueError(f"discord {name} grew above {maximum} bytes while reading")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def open_regular_file_stable(path, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode) or (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise RuntimeError(f"discord {name} changed while opening")
        return os.fdopen(descriptor, "rb")
    except Exception:
        os.close(descriptor)
        raise


def materialize_payload(content, manifest_maximum, payload_maximum):
    manifest_hash = content_hexdigest(content)
    manifest_bytes = read_bounded_regular(os.path.join(BLOBS_DIR, manifest_hash), manifest_maximum, "cached artifact manifest")
    if octopus_content_hash(manifest_bytes) != manifest_hash:
        raise ValueError("discord cached artifact manifest hash mismatch")
    manifest = require_object(json.loads(manifest_bytes), "discord cached artifact manifest")
    payload_hash = manifest.get("payload_content_hash")
    if not isinstance(payload_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", payload_hash):
        raise ValueError("discord cached artifact manifest has invalid payload_content_hash")
    label = require_string(manifest.get("label"), "discord cached artifact manifest.label")
    payload = read_bounded_regular(os.path.join(BLOBS_DIR, payload_hash), payload_maximum, "cached artifact payload")
    if octopus_content_hash(payload) != payload_hash:
        raise ValueError("discord cached artifact payload hash mismatch")
    return label, payload


def movement_interface_endpoint(value):
    if not isinstance(value, dict) or set(value) != {"Interface"}:
        return None
    identifier = value.get("Interface")
    if not isinstance(identifier, str) or not identifier or identifier.strip() != identifier:
        return None
    return identifier


def require_outbound_runtime(movement):
    metadata = require_object(
        movement.get("invocation_metadata"),
        "discord outbound movement.invocation_metadata",
    )
    runtime = require_object(
        metadata.get("runtime"),
        "discord outbound movement runtime metadata",
    )
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        require_string(
            runtime.get(field),
            f"discord outbound movement runtime.{field}",
        )
    require_int(runtime.get("epoch"), "discord outbound movement runtime.epoch", 0)
    if runtime["interface"] != INTERFACE_ID:
        raise ValueError("discord outbound runtime interface does not match its exact target")
    return runtime


def _read_outbound_index(next_seq, limit):
    uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
    connection = sqlite3.connect(uri, uri=True, timeout=0.25)
    try:
        if connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone() != ("3",):
            raise RuntimeError("discord ledger index has unsupported schema")
        rows = connection.execute(
            "SELECT seq, artifact_content, runtime_run, runtime_checkpoint, from_id FROM movements "
            "WHERE runtime_interface = ? AND kind = 'InterfaceOutbound' "
            "AND from_kind = 'interface' AND to_kind = 'interface' AND to_id = ? AND seq >= ? "
            "ORDER BY seq ASC LIMIT ?",
            (INTERFACE_ID, INTERFACE_ID, next_seq, limit),
        ).fetchall()
    finally:
        connection.close()
    result = []
    for seq, content, run, checkpoint, source_interface in rows:
        seq = require_int(seq, "discord ledger index seq", 0)
        require_string(source_interface, f"discord ledger index movement {seq}.from_id")
        if content is not None and (not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content)):
            raise RuntimeError(f"discord ledger index movement {seq} has malformed content")
        require_string(run, f"discord ledger index movement {seq}.run")
        require_string(checkpoint, f"discord ledger index movement {seq}.checkpoint")
        result.append((seq, None if content is None else bytes.fromhex(content), run))
    return result


def _read_outbound_ledger(next_seq, limit, max_row_bytes):
    global _LEDGER_FALLBACK_IDENTITY, _LEDGER_FALLBACK_OFFSET, _LEDGER_FALLBACK_SIZE
    max_row_bytes = require_int(max_row_bytes, "discord ledger.max_row_bytes", 256, 16777216)
    try:
        handle = open_regular_file_stable(MOVEMENTS_JSONL, "movement ledger")
    except FileNotFoundError:
        return []
    result = []
    with handle:
        status = os.fstat(handle.fileno())
        size = status.st_size
        identity = (status.st_dev, status.st_ino)
        if identity != _LEDGER_FALLBACK_IDENTITY or size < _LEDGER_FALLBACK_SIZE or _LEDGER_FALLBACK_OFFSET > size:
            _LEDGER_FALLBACK_OFFSET = 0
        handle.seek(_LEDGER_FALLBACK_OFFSET)
        safe_offset = _LEDGER_FALLBACK_OFFSET
        has_uncommitted = False
        while True:
            row_offset = handle.tell()
            line = handle.readline(max_row_bytes + 1)
            if not line:
                if not has_uncommitted:
                    safe_offset = handle.tell()
                break
            if len(line) > max_row_bytes:
                raise RuntimeError(f"discord ledger row at byte {row_offset} exceeds {max_row_bytes} bytes")
            if not line.endswith(b"\n"):
                raise RuntimeError(f"discord ledger row at byte {row_offset} is partial")
            movement = require_object(json.loads(line), "discord ledger movement")
            seq = require_int(movement.get("seq"), "discord ledger movement.seq", 0)
            if seq < next_seq:
                if not has_uncommitted:
                    safe_offset = handle.tell()
                continue
            if movement.get("kind") != "InterfaceOutbound":
                if not has_uncommitted:
                    safe_offset = handle.tell()
                continue
            if movement_interface_endpoint(movement.get("to")) != INTERFACE_ID:
                if not has_uncommitted:
                    safe_offset = handle.tell()
                continue
            if movement_interface_endpoint(movement.get("from")) is None:
                if not has_uncommitted:
                    safe_offset = handle.tell()
                continue
            runtime = require_outbound_runtime(movement)
            artifact = movement.get("artifact")
            if not has_uncommitted:
                safe_offset = row_offset
                has_uncommitted = True
            result.append((seq, None if not artifact else artifact.get("content"), runtime.get("run")))
            if len(result) >= limit:
                break
        _LEDGER_FALLBACK_OFFSET = safe_offset
        _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
        _LEDGER_FALLBACK_IDENTITY = identity
    return result


def read_own_outbound(next_seq, limit=128, *, max_row_bytes):
    global _LEDGER_INDEX_WARNING
    try:
        return _read_outbound_index(next_seq, limit)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
        message = str(error)
        if message != _LEDGER_INDEX_WARNING:
            sys.stderr.write(f"discord ledger index unavailable; reading ledger truth: {message}\n")
            sys.stderr.flush()
            _LEDGER_INDEX_WARNING = message
        return _read_outbound_ledger(next_seq, limit, max_row_bytes)


def effect_path(source_seq):
    return os.path.join(EFFECTS_DIR, f"{source_seq}.json")


def validate_effect(source_seq, state):
    state = require_keys(state, {"schema_version", "source_movement_seq", "source_artifact_hash", "run", "artifact_label", "status"}, {"result", "error", "evidence_movement_seq"}, f"discord effect {source_seq}")
    if state["schema_version"] != 1 or state["source_movement_seq"] != source_seq:
        raise ValueError(f"discord effect {source_seq} identity is invalid")
    if not isinstance(state["source_artifact_hash"], str) or not re.fullmatch(r"[0-9a-f]{64}", state["source_artifact_hash"]):
        raise ValueError(f"discord effect {source_seq} artifact hash is invalid")
    require_string(state["run"], f"discord effect {source_seq}.run")
    require_string(state["artifact_label"], f"discord effect {source_seq}.artifact_label")
    if state["status"] not in ("started", "succeeded", "indeterminate", "evidenced", "failure_evidenced"):
        raise ValueError(f"discord effect {source_seq}.status is invalid")
    if state["status"] in ("succeeded", "evidenced") and not isinstance(state.get("result"), dict):
        raise ValueError(f"discord effect {source_seq} success has no result")
    if state["status"] == "indeterminate":
        require_string(state.get("error"), f"discord effect {source_seq}.error")
    if state["status"] in ("evidenced", "failure_evidenced"):
        require_int(state.get("evidence_movement_seq"), f"discord effect {source_seq}.evidence_movement_seq", 0)
    return state


def load_effect(source_seq):
    state = read_json(effect_path(source_seq))
    return None if state is None else validate_effect(source_seq, state)


def commit_effect(source_seq, state):
    atomic_write_json(effect_path(source_seq), validate_effect(source_seq, state), indent=2)


def hand_delivery_result(source_seq, content_hash, run, result):
    return hand_json("discord.delivery.result", {"source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "ok": True, "result": result}, f"discord-delivery-result-{source_seq}")


def stream_state():
    state = read_json(STREAM_STATE_PATH, {"schema_version": 1, "message_id": None, "text": "", "last_seq": None, "last_edit_ms": 0})
    state = require_keys(state, {"schema_version", "message_id", "text", "last_seq", "last_edit_ms"}, set(), "discord stream state")
    if state["schema_version"] != 1:
        raise ValueError("discord stream state schema_version must be 1")
    if state["message_id"] is not None:
        require_snowflake(state["message_id"], "discord stream state.message_id")
    if not isinstance(state["text"], str):
        raise ValueError("discord stream state.text must be a string")
    if state["last_seq"] is not None:
        require_int(state["last_seq"], "discord stream state.last_seq", 0)
    require_int(state["last_edit_ms"], "discord stream state.last_edit_ms", 0)
    return state


def update_stream_state(**fields):
    with _STATE_LOCK:
        state = stream_state()
        state.update(fields)
        atomic_write_json(STREAM_STATE_PATH, state)
        return state
